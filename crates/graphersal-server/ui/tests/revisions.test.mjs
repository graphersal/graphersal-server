// Node tests of the revisions the web UI sends with its changes (docs/admin-api.md "Revisions
// (concurrent changes)"): the requests carry `If-Match` only when a revision is given, a `409`
// with the current `revision` is a conflict (a warning notice saying the tab read its data
// again), the database rows and the alias list carry their revisions, and a round against a
// mock server that refuses a stale revision like graphersal-server does.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, sendAdmin, createAdmin, parseAdminError, AdminError, isConflict, refusalNotice, CONFLICT_RELOADED,
} from '../admin-api.js';
import { databaseRows, fetchDatabases } from '../databases.js';

const HELP = 'changed by someone else: reload';

test('the changes of shared objects carry the revision only when one is given', () => {
  assert.deepEqual(adminRequest.readOnly('acme', true), { method: 'PUT', path: 'api/admin/databases/acme/read-only', body: { readOnly: true } });
  assert.deepEqual(adminRequest.readOnly('acme', true, 'r1'), { method: 'PUT', path: 'api/admin/databases/acme/read-only', body: { readOnly: true }, ifMatch: 'r1' });
  assert.deepEqual(adminRequest.autoLoad('acme', false, 'r2').ifMatch, 'r2');
  assert.deepEqual(adminRequest.setAlias('live', 'acme', 'a1'), { method: 'PUT', path: 'api/admin/aliases/live', body: { target: 'acme' }, ifMatch: 'a1' });
  assert.deepEqual(adminRequest.removeAlias('live', 'a1'), { method: 'DELETE', path: 'api/admin/aliases/live', ifMatch: 'a1' });
  assert.deepEqual(adminRequest.renameAlias('live', 'next', 'a1').ifMatch, 'a1');
  for (const request of [
    adminRequest.removeUser('bob', 'u1'), adminRequest.newToken('bob', 'u1'), adminRequest.revokeToken('bob', 'u1'),
    adminRequest.setAdmin('bob', true, 'u1'), adminRequest.grant('bob', 'acme', 'read', 'u1'), adminRequest.revokeGrant('user:bob', 'acme', 'u1'),
  ]) {
    assert.equal(request.ifMatch, 'u1', request.path);
  }
  assert.equal('ifMatch' in adminRequest.grant('bob', 'acme', 'read', ''), false, 'an empty revision is none (an older server)');
  assert.equal('ifMatch' in adminRequest.removeAlias('live'), false);
});

test('sendAdmin sends the revision as If-Match', async () => {
  const seen = [];
  const fetch = async (url, init) => {
    seen.push(init.headers);
    return { ok: true, status: 200, text: async () => '{}' };
  };
  await sendAdmin({ base: 'http://h/', fetch }, adminRequest.setAdmin('bob', true, 'abc123'));
  await sendAdmin({ base: 'http://h/', fetch }, adminRequest.setAdmin('bob', true));
  assert.equal(seen[0]['If-Match'], '"abc123"');
  assert.equal(seen[1]['If-Match'], undefined);
});

test('a 409 with the current revision is a conflict with a warning notice', () => {
  const body = JSON.stringify({ error: 'the aliases changed by someone else since you read them', help: HELP, revision: 'r9' });
  const err = parseAdminError(409, body);
  assert.ok(err instanceof AdminError);
  assert.equal(err.revision, 'r9');
  assert.equal(isConflict(err), true);
  const notice = refusalNotice(err);
  assert.equal(notice.tone, 'warn');
  assert.equal(notice.text, 'the aliases changed by someone else since you read them');
  assert.equal(notice.help, `${HELP}. ${CONFLICT_RELOADED}`);
  // Other refusals stay errors, verbatim.
  const taken = parseAdminError(409, JSON.stringify({ error: 'name taken', help: 'choose another name' }));
  assert.equal(isConflict(taken), false);
  assert.deepEqual(refusalNotice(taken), { tone: 'error', text: 'name taken', help: 'choose another name' });
  assert.deepEqual(refusalNotice(new Error('boom')), { tone: 'error', text: 'boom', help: '' });
  assert.equal(isConflict(null), false);
});

test('database rows and the alias list carry their revisions', () => {
  const answer = databaseRows({
    databases: [{ name: 'acme', state: 'unloaded', revision: 'd1' }, { name: 'old', state: 'unloaded' }],
    aliases: { live: 'acme' },
    aliasesRevision: 'a1',
  });
  assert.deepEqual(answer.rows.map((r) => [r.name, r.revision]), [['acme', 'd1'], ['old', '']]);
  assert.equal(answer.aliasesRevision, 'a1');
  assert.equal(databaseRows({ databases: [] }).aliasesRevision, '', 'an older server: none');
});

/** A mock of the alias and flag routes that refuses a stale `If-Match` as graphersal-server does. */
async function mockServer() {
  const state = { aliases: { live: 'acme' }, aliasesRev: 1, readOnly: false, dbRev: 1 };
  const server = http.createServer(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    const ifMatch = (req.headers['if-match'] || '').replace(/"/g, '');
    const send = (status, value) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(value));
    };
    const stale = (current) => ifMatch && ifMatch !== current;
    const url = new URL(req.url, 'http://h');
    if (req.method === 'GET' && url.pathname === '/api/admin/databases') {
      return send(200, {
        databases: [{ name: 'acme', state: 'unloaded', readOnly: state.readOnly, revision: `d${state.dbRev}` }],
        aliases: state.aliases,
        aliasesRevision: `a${state.aliasesRev}`,
        problems: [],
      });
    }
    if (req.method === 'PUT' && url.pathname === '/api/admin/aliases/live') {
      if (stale(`a${state.aliasesRev}`)) {
        return send(409, { error: 'the aliases changed by someone else since you read them', help: HELP, revision: `a${state.aliasesRev}` });
      }
      state.aliases.live = body.target;
      state.aliasesRev++;
      return send(200, { alias: 'live', target: body.target });
    }
    if (req.method === 'PUT' && url.pathname === '/api/admin/databases/acme/read-only') {
      if (stale(`d${state.dbRev}`)) {
        return send(409, { error: "the settings of database 'acme' changed by someone else", help: HELP, revision: `d${state.dbRev}` });
      }
      state.readOnly = body.readOnly;
      state.dbRev++;
      return send(200, { name: 'acme', state: 'unloaded', readOnly: state.readOnly, revision: `d${state.dbRev}` });
    }
    return send(404, { error: 'unknown' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { server, state, options: { base, fetch: globalThis.fetch } };
}

test('two pages: the second change from a stale page is refused, a reload makes it pass', async () => {
  const { server, state, options } = await mockServer();
  try {
    const admin = createAdmin(options);
    // Two browsers read the same list.
    const pageA = await fetchDatabases(options);
    const pageB = await fetchDatabases(options);
    // A flips the alias; B, still showing the old list, tries too and is refused.
    await admin.setAlias('live', 'beta', pageA.aliasesRevision);
    await assert.rejects(admin.setAlias('live', 'gamma', pageB.aliasesRevision), (err) => isConflict(err) && err.revision === 'a2');
    assert.equal(state.aliases.live, 'beta', 'the refused change changed nothing');
    // B reads again (what the tab does on a conflict) and repeats the change.
    const again = await fetchDatabases(options);
    await admin.setAlias('live', 'gamma', again.aliasesRevision);
    assert.equal(state.aliases.live, 'gamma');
    // The same for a database's flag, row by row.
    const rowA = pageA.rows[0];
    const rowB = pageB.rows[0];
    const answer = await admin.readOnly('acme', true, rowA.revision);
    assert.equal(answer.revision, 'd2');
    await assert.rejects(admin.readOnly('acme', false, rowB.revision), (err) => isConflict(err) && refusalNotice(err).tone === 'warn');
    assert.equal(state.readOnly, true);
    // Without a revision (an older page, a script) the change is applied as before.
    await admin.readOnly('acme', false);
    assert.equal(state.readOnly, false);
  } finally {
    server.close();
  }
});
