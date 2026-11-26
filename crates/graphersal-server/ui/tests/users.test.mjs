// Node tests of the Users tab's model (ui/admin-api.js "Users and grants", B4): the requests,
// the rows (token as `…last4`), the last-admin rule of the row actions, the forms, the texts
// and a whole round against a stateful mock of `/api/admin/users` and `/api/admin/grants`
// (node:http on port 0, shaped after book/src/reference/admin_api.md "Users and grants").
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, createAdmin, tokenText, userRows, grantRows, subjectText, usableAdmins, userActions,
  userNameProblem, addUserForm, grantForm, userConfirmText, userOutcomeText, shownToken,
  LAST_ADMIN_TITLE, RESERVED_USER_NAMES, grantKind, grantDatabaseText, grantChoices, GRANT_KINDS,
} from '../admin-api.js';
import { readFileSync } from 'node:fs';

const LAST_ADMIN_HELP = 'make another user an admin first';

/** A stateful mock of the users and grants routes (alice admin, bob with two grants). */
async function mockServer() {
  let counter = 0;
  const newToken = () => `${'0'.repeat(60)}${String(1000 + ++counter)}`;
  const users = {
    alice: { roles: ['admin'], token: 'x'.repeat(60) + 'a1b2' },
    bob: { roles: [], token: 'y'.repeat(60) + 'c3d4' },
  };
  let grants = [
    { subject: 'user:bob', database: 'acme', level: 'read' },
    { subject: 'user:bob', database: 'demo', level: 'write' },
  ];
  const log = [];
  const userJson = (name) => {
    const u = users[name];
    return {
      name, roles: u.roles, admin: u.roles.includes('admin'), hasToken: !!u.token,
      tokenLast4: u.token ? u.token.slice(-4) : null,
      grants: grants.filter((g) => g.subject === `user:${name}`).map((g) => ({ database: g.database, level: g.level })),
    };
  };
  const usable = () => Object.values(users).filter((u) => u.token && u.roles.includes('admin')).length;
  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks).toString();
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, body: raw });
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    const body = raw ? JSON.parse(raw) : null;
    const parts = url.pathname.replace(/^\/api\/admin\//, '').split('/').map(decodeURIComponent);
    const lastAdmin = (name) => send(409, { error: `'${name}' is the last admin`, help: LAST_ADMIN_HELP });
    const isLast = (name) => users[name].roles.includes('admin') && users[name].token && usable() <= 1;
    if (parts[0] === 'users') {
      const name = parts[1];
      if (parts.length === 1 && req.method === 'GET') {
        return send(200, { users: Object.keys(users).sort().map(userJson), grants });
      }
      if (parts.length === 1 && req.method === 'POST') {
        if (users[body.name]) return send(409, { error: `a user '${body.name}' exists` });
        users[body.name] = { roles: body.admin ? ['admin'] : [], token: newToken() };
        return send(201, { ...userJson(body.name), token: users[body.name].token });
      }
      if (!users[name]) return send(404, { error: `no user '${name}'`, help: 'GET /api/admin/users lists the users' });
      if (parts.length === 2 && req.method === 'DELETE') {
        if (isLast(name)) return lastAdmin(name);
        const before = grants.length;
        grants = grants.filter((g) => g.subject !== `user:${name}`);
        delete users[name];
        return send(200, { name, removed: true, grantsRemoved: before - grants.length, mcpSessionsEnded: 1 });
      }
      if (parts[2] === 'token' && req.method === 'POST') {
        users[name].token = newToken();
        return send(200, { ...userJson(name), token: users[name].token, mcpSessionsEnded: 0 });
      }
      if (parts[2] === 'token' && req.method === 'DELETE') {
        if (isLast(name)) return lastAdmin(name);
        users[name].token = null;
        return send(200, { ...userJson(name), mcpSessionsEnded: 2 });
      }
      if (parts[2] === 'admin' && req.method === 'PUT') {
        if (!body.admin && isLast(name)) return lastAdmin(name);
        users[name].roles = body.admin ? ['admin'] : [];
        return send(200, userJson(name));
      }
    }
    if (parts[0] === 'grants' && parts.length === 1 && req.method === 'POST') {
      const keys = Object.keys(body).sort().join(',');
      if (keys !== 'database,level,subject') return send(400, { error: 'invalid body' });
      grants = grants.filter((g) => !(g.subject === body.subject && g.database === body.database));
      grants.push(body);
      return send(200, body);
    }
    if (parts[0] === 'grants' && parts.length === 3 && req.method === 'DELETE') {
      const [, subject, database] = parts;
      const before = grants.length;
      grants = grants.filter((g) => !(g.subject === subject && g.database === database));
      return send(200, { subject, database, revoked: grants.length < before });
    }
    return send(404, { error: 'unknown admin endpoint' });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { base, log, close: () => new Promise((r) => server.close(r)) };
}

test('the users and grants requests', () => {
  assert.deepEqual(adminRequest.users(), { method: 'GET', path: 'api/admin/users' });
  assert.deepEqual(adminRequest.addUser('carol'), { method: 'POST', path: 'api/admin/users', body: { name: 'carol' } });
  assert.deepEqual(adminRequest.addUser('carol', true), { method: 'POST', path: 'api/admin/users', body: { name: 'carol', admin: true } });
  assert.deepEqual(adminRequest.removeUser('a b'), { method: 'DELETE', path: 'api/admin/users/a%20b' });
  assert.deepEqual(adminRequest.newToken('bob'), { method: 'POST', path: 'api/admin/users/bob/token' });
  assert.deepEqual(adminRequest.revokeToken('bob'), { method: 'DELETE', path: 'api/admin/users/bob/token' });
  assert.deepEqual(adminRequest.setAdmin('bob', 1), { method: 'PUT', path: 'api/admin/users/bob/admin', body: { admin: true } });
  assert.deepEqual(adminRequest.grant('anonymous', 'demo', 'read'), {
    method: 'POST', path: 'api/admin/grants', body: { subject: 'anonymous', database: 'demo', level: 'read' },
  });
  assert.deepEqual(adminRequest.revokeGrant('user:bob', '*'), { method: 'DELETE', path: 'api/admin/grants/user%3Abob/*' });
});

test('rows: the token as …last4, grants sorted with anonymous first', () => {
  assert.equal(tokenText({ hasToken: true, tokenLast4: 'a1b2' }), '…a1b2');
  assert.equal(tokenText({ hasToken: true, tokenLast4: null }), 'set');
  assert.equal(tokenText({ hasToken: false, tokenLast4: 'a1b2' }), 'none');
  assert.throws(() => userRows({}), /no user list/);
  const rows = userRows({ users: [
    { name: 'zed', roles: ['admin'], hasToken: false, tokenLast4: null, grants: [] },
    { name: 'bob', roles: [], admin: false, hasToken: true, tokenLast4: 'c3d4', grants: [{ database: 'acme', level: 'read' }, { database: 'x', level: 'none' }] },
    { roles: [] },
  ] });
  assert.deepEqual(rows.map((u) => [u.name, u.admin, u.tokenText]), [['bob', false, '…c3d4'], ['zed', true, 'none']]);
  assert.deepEqual(rows[0].grants, [{ database: 'acme', level: 'read', kind: 'database', target: null, databaseText: 'acme' }], 'an invalid level is dropped');
  const grants = grantRows({ grants: [
    { subject: 'user:bob', database: 'b', level: 'write' },
    { subject: 'user:bob', database: 'a', level: 'read' },
    { subject: 'anonymous', database: '*', level: 'read' },
    { subject: 'role:x', database: 'a', level: 'bogus' },
  ] });
  assert.deepEqual(grants.map((g) => [g.subjectText, g.databaseText, g.level]), [
    ['everyone (anonymous)', 'every database', 'read'], ['bob', 'a', 'read'], ['bob', 'b', 'write'],
  ]);
  assert.equal(subjectText('role:ops'), 'role:ops');
});

test('the last usable admin cannot be removed, demoted or left without a token', () => {
  const alice = { name: 'alice', admin: true, hasToken: true };
  const bob = { name: 'bob', admin: false, hasToken: true };
  const zed = { name: 'zed', admin: true, hasToken: false };
  const rows = [alice, bob, zed];
  assert.equal(usableAdmins(rows), 1, 'an admin without a token cannot act');
  const a = userActions(alice, rows);
  assert.equal(a.newToken.enabled, true, 'a lost token is replaced, never revoked');
  for (const key of ['revokeToken', 'admin', 'remove']) {
    assert.equal(a[key].enabled, false, key);
    assert.equal(a[key].title, LAST_ADMIN_TITLE, key);
  }
  const b = userActions(bob, rows);
  assert.deepEqual([b.newToken.enabled, b.revokeToken.enabled, b.admin.enabled, b.remove.enabled], [true, true, true, true]);
  assert.equal(userActions(zed, rows).revokeToken.enabled, false, 'no token to revoke');
  assert.equal(userActions(zed, rows).remove.enabled, true, 'not a usable admin');
  const two = [alice, { ...bob, admin: true }];
  assert.equal(userActions(alice, two).remove.enabled, true);
  const busy = userActions(bob, rows, { busy: true });
  assert.ok(Object.values(busy).every((x) => !x.enabled));
});

test('the forms', () => {
  assert.equal(userNameProblem(''), 'Enter a user name.');
  assert.match(userNameProblem('-x'), /starts with a lower-case letter or a digit/);
  for (const name of RESERVED_USER_NAMES) assert.match(userNameProblem(name.toUpperCase()), /reserved/);
  assert.deepEqual(addUserForm({ name: ' Carol ', admin: true }), { name: 'carol', admin: true });
  assert.deepEqual(addUserForm({ name: 'carol' }), { name: 'carol', admin: false });
  assert.deepEqual(addUserForm({ name: '' }), { problems: ['Enter a user name.'] });
  assert.deepEqual(grantForm({ subject: 'bob', database: 'acme', level: 'read' }), { subject: 'user:bob', database: 'acme', level: 'read' });
  assert.deepEqual(grantForm({ subject: 'anonymous', database: '*', level: 'write' }), { subject: 'anonymous', database: '*', level: 'write' });
  assert.deepEqual(grantForm({ subject: 'role:ops', database: 'a', level: 'read' }).subject, 'role:ops');
  assert.deepEqual(grantForm({}).problems.length, 3);
  assert.deepEqual(grantForm({ subject: 'bob', database: 'a', level: 'none' }), { problems: ['Choose read or write.'] });
});

test('texts', () => {
  assert.match(userConfirmText('removeUser', 'bob'), /sessions end at once/);
  assert.match(userConfirmText('newToken', 'bob'), /old one stops working/);
  assert.match(userConfirmText('revokeToken', 'bob'), /cannot log in/);
  assert.match(userConfirmText('demote', 'bob'), /admin role/);
  assert.equal(userConfirmText('revokeGrant', 'anonymous', '*'), 'Revoke the grant of everyone (anonymous) on every database?');
  assert.equal(userConfirmText('setAdmin', 'bob'), null);
  assert.match(userOutcomeText('addUser', 'carol', { name: 'carol', admin: true }), /\(admin\).*shown only once/);
  assert.equal(userOutcomeText('newToken', 'bob', { name: 'bob', mcpSessionsEnded: 1 }), '"bob" has a new token; 1 MCP session ended. Copy it now: it is shown only once.');
  assert.equal(userOutcomeText('revokeToken', 'bob', { name: 'bob', mcpSessionsEnded: 2 }), '"bob" has no token now; 2 MCP sessions ended.');
  assert.equal(userOutcomeText('removeUser', 'bob', { name: 'bob', grantsRemoved: 2, mcpSessionsEnded: 0 }), 'Removed the user "bob" and 2 grants.');
  assert.equal(userOutcomeText('setAdmin', 'bob', { name: 'bob', admin: false }), '"bob" is no longer an admin.');
  assert.equal(userOutcomeText('grant', 'x', { subject: 'anonymous', database: 'demo', level: 'read' }), 'everyone (anonymous) can read "demo".');
  assert.equal(userOutcomeText('revokeGrant', 'x', { subject: 'user:bob', database: 'acme', revoked: false }), 'bob had no grant on "acme".');
  assert.deepEqual(shownToken({ name: 'carol', token: 'abc' }), { name: 'carol', token: 'abc' });
  assert.equal(shownToken({ name: 'carol' }), null);
});

test('a round against the mock: add (token once), grant, new token, revoke, the last admin, remove', async () => {
  const srv = await mockServer();
  try {
    const admin = createAdmin({ base: srv.base, fetch });
    let answer = await admin.users();
    assert.deepEqual(userRows(answer).map((u) => [u.name, u.tokenText]), [['alice', '…a1b2'], ['bob', '…c3d4']]);
    // Add: the token is in this answer only.
    const added = await admin.addUser('carol');
    const token = shownToken(added);
    assert.equal(token.name, 'carol');
    assert.equal(token.token.length, 64);
    answer = await admin.users();
    const carol = userRows(answer).find((u) => u.name === 'carol');
    assert.equal(carol.tokenText, `…${token.token.slice(-4)}`);
    assert.ok(!JSON.stringify(answer).includes(token.token), 'the listing never carries a token');
    // An anonymous grant, then its revoke.
    const g = grantForm({ subject: 'anonymous', database: 'demo', level: 'read' });
    await admin.grant(g.subject, g.database, g.level);
    assert.deepEqual(grantRows(await admin.users())[0], { subject: 'anonymous', subjectText: 'everyone (anonymous)', database: 'demo', databaseText: 'demo', kind: 'database', target: null, level: 'read' });
    assert.equal((await admin.revokeGrant('anonymous', 'demo')).revoked, true);
    assert.equal((await admin.revokeGrant('anonymous', 'demo')).revoked, false);
    // A new token replaces the old one; a revoke leaves none.
    const fresh = shownToken(await admin.newToken('carol'));
    assert.notEqual(fresh.token, token.token);
    const revoked = await admin.revokeToken('carol');
    assert.equal(userOutcomeText('revokeToken', 'carol', revoked), '"carol" has no token now; 2 MCP sessions ended.');
    // The last admin: the server refuses with help, the client shows both.
    await assert.rejects(admin.removeUser('alice'), (err) => {
      assert.equal(err.status, 409);
      assert.equal(err.help, LAST_ADMIN_HELP);
      return true;
    });
    await assert.rejects(admin.setAdmin('alice', false), (err) => err.status === 409);
    // Make bob an admin: now alice may be demoted.
    await admin.setAdmin('bob', true);
    assert.equal((await admin.setAdmin('alice', false)).admin, false);
    const removed = await admin.removeUser('carol');
    assert.equal(userOutcomeText('removeUser', 'carol', removed), 'Removed the user "carol"; 1 MCP session ended.');
    await assert.rejects(admin.removeUser('carol'), (err) => err.status === 404 && /lists the users/.test(err.help));
    const paths = srv.log.map((e) => `${e.method} ${e.path}`);
    assert.ok(paths.includes('DELETE /api/admin/grants/anonymous/demo'), paths.join('\n'));
  } finally {
    await srv.close();
  }
});

test('grants on aliases: every grant table marks an alias with its target, a real database, every database and a missing name', () => {
  assert.deepEqual(GRANT_KINDS, ['database', 'alias', 'all', 'missing']);
  assert.deepEqual(grantKind({ database: 'prod', kind: 'alias', target: 'prod-2026-11' }), { kind: 'alias', target: 'prod-2026-11' });
  assert.deepEqual(grantKind({ database: '*', kind: 'database' }), { kind: 'all', target: null });
  assert.deepEqual(grantKind({ database: 'acme' }), { kind: 'database', target: null }, 'an older server without kind');
  assert.deepEqual(grantKind({ database: 'acme', kind: 'database', target: 'x' }), { kind: 'database', target: null });
  assert.equal(grantDatabaseText({ database: 'prod', kind: 'alias', target: 'prod-2026-11' }), 'prod → prod-2026-11 (alias)');
  assert.equal(grantDatabaseText({ database: 'acme', kind: 'database' }), 'acme');
  assert.equal(grantDatabaseText({ database: 'gone', kind: 'missing' }), 'gone (no such database)');
  assert.equal(grantDatabaseText({ database: '*', kind: 'all' }), 'every database');
  // The "Grants" table and the users' grants.
  const grants = grantRows({ grants: [
    { subject: 'user:bob', database: 'prod', level: 'read', kind: 'alias', target: 'prod-2026-11' },
    { subject: 'user:bob', database: 'acme', level: 'write', kind: 'database', target: null },
    { subject: 'anonymous', database: 'gone', level: 'read', kind: 'missing', target: null },
  ] });
  assert.deepEqual(grants.map((g) => [g.subject, g.databaseText, g.kind, g.target]), [
    ['anonymous', 'gone (no such database)', 'missing', null],
    ['user:bob', 'acme', 'database', null],
    ['user:bob', 'prod → prod-2026-11 (alias)', 'alias', 'prod-2026-11'],
  ]);
  const [bob] = userRows({ users: [{ name: 'bob', roles: [], hasToken: true, tokenLast4: 'c3d4', grants: [
    { database: 'prod', level: 'read', kind: 'alias', target: 'prod-2026-11' },
  ] }] });
  assert.deepEqual(bob.grants, [{ database: 'prod', level: 'read', kind: 'alias', target: 'prod-2026-11', databaseText: 'prod → prod-2026-11 (alias)' }]);
  // The grant form's picker offers the aliases too, marked, BEFORE the real databases (round 4).
  const choices = grantChoices([
    { name: 'prod', aliasOf: 'prod-2026-11' },
    { name: 'prod-2026-11', aliasOf: null },
    { name: 'acme' },
    { name: 'cur', aliasOf: 'acme' },
  ]);
  assert.deepEqual(choices, [
    { value: 'cur', text: 'cur → acme (alias)', kind: 'alias' },
    { value: 'prod', text: 'prod → prod-2026-11 (alias)', kind: 'alias' },
    { value: 'acme', text: 'acme', kind: 'database' },
    { value: 'prod-2026-11', text: 'prod-2026-11', kind: 'database' },
  ]);
  assert.deepEqual(grantChoices(null), []);
  assert.equal(
    userOutcomeText('grant', 'user:bob', { subject: 'user:bob', database: 'prod', level: 'read', kind: 'alias', target: 'prod-2026-11' }),
    'bob can read "prod" (an alias of "prod-2026-11" now: reachable only through "prod").',
  );
  assert.equal(userOutcomeText('grant', 'user:bob', { subject: 'user:bob', database: 'acme', level: 'write', kind: 'database' }), 'bob can read and write "acme".');
  // The Users tab renders the marked texts and the picker's choices.
  const tab = readFileSync(new URL('../users-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes('grantChoices(list.rows)'), 'the picker is built from grantChoices');
  assert.ok(tab.includes('{{ c.text }}'), 'the picker shows the marked text');
  assert.ok(tab.indexOf('label="Aliases"') < tab.indexOf('label="Databases"'), 'aliases first in the picker');
  assert.ok(tab.includes('{{ g.databaseText }}'), 'the grants table shows the marked text');
  assert.ok(tab.includes('${g.databaseText}: ${g.level}'), 'a user\'s grants show the marked text');
});
