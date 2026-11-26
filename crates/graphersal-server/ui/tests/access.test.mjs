// Node tests of access in the `server` profile (ui/access-api.js, B4): the login and logout
// against a mock of `POST /api/login` / `POST /api/logout` (node:http on port 0, shaped after
// book/src/reference/admin_api.md "Login"), the return path `next` (no open redirect), the principal and the
// header, "my databases" (`GET /api/databases`), the refusal handling of the admin client (401:
// login page; 404: recheck) and the per-principal profile views (ui/profile-views.json) checked
// by the engine's validateProfile when a playground is found.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import {
  accessRequest, safeNext, nextFromSearch, afterLoginTarget, rootHref, tokenProblem, login, logout,
  principalOf, headerModel, mineRows, mineNote, mineKindText, emptyMineText, fetchMine, refusalAction, loginHref,
  LOGIN_HELP, MAX_NEXT_CHARS,
} from '../access-api.js';
import { createAdmin, sendAdmin, adminRequest } from '../admin-api.js';
import { fetchDatabases } from '../databases.js';

const UI = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TOKEN = 'a'.repeat(60) + 'b1c2';

/** A mock server: login (TOKEN = bob), logout, `GET /api/databases` per cookie, the admin API
 * answering 401 without the cookie and 404 for bob (no admin role). */
async function mockServer() {
  const log = [];
  const sessions = new Set();
  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks).toString();
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, body: raw, cookie: req.headers.cookie || '' });
    const cookie = (req.headers.cookie || '').match(/gs_session=([^;]+)/);
    const who = cookie && sessions.has(cookie[1]) ? 'bob' : null;
    const send = (status, body, extra = {}) => {
      res.writeHead(status, { 'Content-Type': 'application/json', ...extra });
      res.end(JSON.stringify(body));
    };
    const unauthorized = () => send(401, { error: 'log in first', login: '/' }, { 'WWW-Authenticate': 'Bearer' });
    if (url.pathname === '/api/login' && req.method === 'POST') {
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        return send(400, { error: 'the body must be {"token"}' });
      }
      if (Object.keys(body).some((k) => k !== 'token')) return send(400, { error: 'the body must be {"token"}' });
      if (body.token !== TOKEN) return unauthorized();
      sessions.add('s1');
      return send(200, { principal: { id: 'bob', kind: 'user', admin: false } }, {
        'Set-Cookie': 'gs_session=s1; HttpOnly; SameSite=Strict; Path=/',
      });
    }
    if (url.pathname === '/api/logout' && req.method === 'POST') {
      const ended = !!who;
      sessions.delete('s1');
      return send(200, { loggedOut: ended }, { 'Set-Cookie': 'gs_session=; Max-Age=0' });
    }
    if (url.pathname === '/api/databases' && req.method === 'GET') {
      if (!who) return send(200, { databases: [{ name: 'demo', state: 'loaded', level: 'read', readOnly: true, description: '', playground: '/db/demo/' }] });
      return send(200, {
        databases: [
          { name: 'w', state: 'loaded', level: 'write', readOnly: false, description: 'the writable one', playground: '/db/w/' },
          { name: 'cur', state: 'loaded', level: 'read', readOnly: true, description: '', playground: '/db/cur/', aliasOf: 'r' },
          { name: 'r', state: 'unloaded', level: 'read', readOnly: true, description: '', playground: '/db/r/' },
          { name: 'z z', state: 'loading', level: 'write', readOnly: false, description: '', playground: '/db/z z/' },
          { state: 'loaded' },
        ],
      });
    }
    if (url.pathname === '/api/info') {
      return send(200, { protocol: 'graphersal/1', principal: who ? { id: 'bob', kind: 'user', admin: false } : { id: 'anonymous', kind: 'anonymous', admin: false } });
    }
    if (url.pathname.startsWith('/api/admin/')) {
      if (!who) return unauthorized();
      return send(404, { error: `unknown endpoint ${url.pathname}` });
    }
    return send(404, { error: `not found: ${url.pathname}` });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { base, log, close: () => new Promise((r) => server.close(r)) };
}

/** A fetch that keeps the cookie the mock sets (as a browser does for the page's origin). */
function cookieFetch() {
  let jar = '';
  const fetchFn = async (url, init = {}) => {
    const headers = { ...(init.headers || {}) };
    if (jar) headers.Cookie = jar;
    const r = await fetch(url, { ...init, headers });
    const set = r.headers.get('set-cookie');
    if (set) {
      const pair = set.split(';')[0];
      jar = pair.endsWith('=') ? '' : pair;
    }
    return r;
  };
  return { fetch: fetchFn, jar: () => jar };
}

test('the access routes', () => {
  assert.deepEqual(accessRequest.login('t'), { method: 'POST', path: 'api/login', body: { token: 't' } });
  assert.deepEqual(accessRequest.logout(), { method: 'POST', path: 'api/logout' });
  assert.deepEqual(accessRequest.databases(), { method: 'GET', path: 'api/databases' });
  assert.deepEqual(accessRequest.info(), { method: 'GET', path: 'api/info' });
});

test('next: only a path from the server root is followed (no open redirect)', () => {
  for (const ok of ['/', '/db/acme/', '/db/acme/?tab=x', '/db/a%20b/#/play']) assert.equal(safeNext(ok), ok, ok);
  for (const bad of [
    '', null, undefined, 42, 'db/acme/', 'https://evil.example/', '//evil.example/', '/\\evil.example',
    '\\\\evil', '/db/a b/', '/db/\tx', '/db/\nx', '/x\u0000', '/x\u007f', 'javascript:alert(1)', ' /db/a/',
    `/${'x'.repeat(MAX_NEXT_CHARS)}`,
  ]) {
    assert.equal(safeNext(bad), null, JSON.stringify(bad));
  }
  assert.equal(nextFromSearch('?next=%2Fdb%2Facme%2F'), '/db/acme/');
  assert.equal(nextFromSearch('?next=/db/acme/'), '/db/acme/');
  assert.equal(nextFromSearch('?next=%2F%2Fevil.example'), null);
  assert.equal(nextFromSearch('?next=https%3A%2F%2Fevil.example'), null);
  assert.equal(nextFromSearch('?other=1'), null);
  assert.equal(nextFromSearch(''), null);
  assert.equal(nextFromSearch(undefined), null);
  const root = new URL('http://h:1/?next=x#/play?tab=login');
  assert.equal(rootHref(root), 'http://h:1/');
  assert.equal(afterLoginTarget('?next=%2Fdb%2Fw%2F', root), '/db/w/');
  assert.equal(afterLoginTarget('?next=%2F%2Fevil', root), 'http://h:1/', 'a refused next goes to the root');
  assert.equal(afterLoginTarget('', 'http://h:1/proxy/'), 'http://h:1/proxy/');
});

test('a pasted token is checked before it is sent', () => {
  assert.equal(tokenProblem(''), 'Paste your token.');
  assert.equal(tokenProblem('   '), 'Paste your token.');
  assert.match(tokenProblem('ab cd'), /no spaces/);
  assert.equal(tokenProblem(`  ${TOKEN}\n`), null, 'surrounding white space is trimmed');
});

test('login, my databases and logout against the mock', async () => {
  const srv = await mockServer();
  try {
    const jar = cookieFetch();
    const options = { base: srv.base, fetch: jar.fetch };
    // A wrong token: the server's error, with the login help.
    await assert.rejects(login(options, 'nope'), (err) => {
      assert.equal(err.status, 401);
      assert.equal(err.message, 'log in first');
      assert.equal(err.help, LOGIN_HELP);
      assert.equal(err.login, '/');
      return true;
    });
    // Anonymous sees the databases open to everyone.
    assert.deepEqual((await fetchMine(options)).map((r) => r.name), ['demo']);
    // The token is trimmed and sent as the only field.
    const principal = await login(options, ` ${TOKEN} `);
    assert.deepEqual(principal, { id: 'bob', kind: 'user', admin: false });
    const sent = srv.log.filter((e) => e.path === '/api/login').at(-1);
    assert.equal(sent.method, 'POST');
    assert.deepEqual(JSON.parse(sent.body), { token: TOKEN });
    assert.equal(jar.jar(), 'gs_session=s1');
    // bob's databases, sorted, the malformed entry skipped.
    const rows = await fetchMine(options);
    assert.deepEqual(rows.map((r) => [r.name, r.level, r.open, r.aliasOf]), [
      ['cur', 'read', true, 'r'], ['r', 'read', false, null], ['w', 'write', true, null], ['z z', 'write', false, null],
    ]);
    assert.equal(rows[2].href, 'db/w/');
    assert.equal(rows[3].href, 'db/z%20z/');
    assert.equal(rows[2].levelText, 'read and write');
    assert.equal(rows[0].levelText, 'read only');
    assert.equal(rows[2].description, 'the writable one');
    // The admin API is 404 for bob: the refusal says "recheck".
    const seen = [];
    const admin = createAdmin({ ...options, onRefusal: (err) => seen.push(refusalAction(err)) });
    await assert.rejects(admin.users(), (err) => err.status === 404);
    assert.deepEqual(seen, ['recheck']);
    // Logout ends the session; then the admin API is 401 and the refusal says "login".
    assert.equal(await logout(options), true);
    assert.equal(jar.jar(), '');
    assert.equal(await logout(options), false, 'no live session the second time');
    await assert.rejects(admin.users(), (err) => err.status === 401 && err.login === '/');
    assert.deepEqual(seen, ['recheck', 'login']);
    // The database list of the Databases tab reports a refusal the same way.
    const refused = [];
    await assert.rejects(fetchDatabases({ ...options, onRefusal: (err) => refused.push(refusalAction(err)) }),
      (err) => err.status === 401 && err.login === '/');
    assert.deepEqual(refused, ['login']);
  } finally {
    await srv.close();
  }
});

test('a refusal hook that throws never hides the request\'s own error', async () => {
  const srv = await mockServer();
  try {
    await assert.rejects(
      sendAdmin({ base: srv.base, fetch, onRefusal: () => { throw new Error('hook'); } }, adminRequest.users()),
      (err) => err.status === 401 && err.message === 'log in first',
    );
  } finally {
    await srv.close();
  }
});

test('refusalAction and the login link', () => {
  assert.equal(refusalAction({ status: 401, login: '/' }), 'login');
  assert.equal(refusalAction({ status: 401, login: '' }), null, 'a 401 without a login page');
  assert.equal(refusalAction({ status: 401, login: '//evil' }), null);
  assert.equal(refusalAction({ status: 404 }), 'recheck');
  assert.equal(refusalAction({ status: 409 }), null);
  assert.equal(refusalAction(null), null);
  assert.equal(loginHref('/', 'http://h/#/play?tab=users'), 'http://h/');
  assert.equal(loginHref('/', 'http://h/proxy/'), 'http://h/proxy/', 'a proxy prefix survives');
  assert.equal(loginHref('/login', 'http://h/proxy/'), 'http://h/proxy/login');
  assert.equal(loginHref('https://evil/', 'http://h/'), 'http://h/');
});

test('the principal and the header', () => {
  assert.deepEqual(principalOf(null), { id: 'anonymous', kind: 'anonymous', admin: false });
  assert.deepEqual(principalOf({}), { id: 'anonymous', kind: 'anonymous', admin: false });
  assert.deepEqual(principalOf({ principal: { id: 'alice', kind: 'user', admin: true } }), { id: 'alice', kind: 'user', admin: true });
  assert.deepEqual(principalOf({ principal: { id: 'x', kind: 'user', admin: 'yes' } }).admin, false);
  assert.deepEqual(headerModel(principalOf(null)), { show: false, name: '', role: '', logout: false });
  assert.deepEqual(headerModel({ id: 'bob', kind: 'user', admin: false }), { show: true, name: 'bob', role: '', logout: true });
  assert.deepEqual(headerModel({ id: 'alice', kind: 'user', admin: true }), { show: true, name: 'alice', role: 'admin', logout: true });
  assert.deepEqual(headerModel({ id: 'operator', kind: 'operator', admin: true }), { show: true, name: 'operator', role: 'admin', logout: false });
});

test('my databases: rows, notes and the empty text', () => {
  assert.throws(() => mineRows({}), /no database list/);
  assert.throws(() => mineRows(null), /no database list/);
  const rows = mineRows({ databases: [
    { name: 'b', state: 'failed', level: 'write' },
    { name: 'a', state: 'needs-model', level: 'something' },
    { name: 'c', state: 'brand-new', level: 'read' },
  ] });
  assert.deepEqual(rows.map((r) => [r.name, r.level, r.stateText, r.tone]), [
    ['a', 'read', 'needs a model', 'warn'], ['b', 'write', 'failed', 'danger'], ['c', 'read', 'brand-new', ''],
  ]);
  assert.equal(mineNote(rows[0]), 'Not loaded: ask an admin to load it.');
  assert.equal(mineNote({ open: false, state: 'loading' }), 'Loading: it opens in a moment.');
  assert.equal(mineNote({ open: true, state: 'loaded' }), '');
  assert.match(emptyMineText({ kind: 'user' }), /Ask an admin for a grant/);
  assert.match(emptyMineText({ kind: 'anonymous' }), /Sign in/);
});

test('my databases marks an alias with the database it names now, and a real database', () => {
  const rows = mineRows({ databases: [
    { name: 'prod', state: 'loaded', level: 'read', kind: 'alias', aliasOf: 'prod-2026-11' },
    { name: 'acme', state: 'loaded', level: 'write', kind: 'database' },
  ] });
  assert.deepEqual(rows.map((r) => [r.name, r.kind, r.kindText, r.aliasOf]), [
    ['acme', 'database', 'database', null],
    ['prod', 'alias', 'alias → prod-2026-11', 'prod-2026-11'],
  ]);
  assert.equal(mineKindText(null), 'database');
  const tab = readFileSync(new URL('../mine-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes('{{ row.kindText }}'), 'the tab shows the mark');
});

/** The engine's playground directory for the profile checks, or null (skipped): as
 * profile.test.mjs finds it, GRAPHERSAL_PLAYGROUND, else `ui/` beside graphersal-serve's
 * manifest as `cargo metadata` resolves it. */
function playgroundDir() {
  if (process.env.GRAPHERSAL_PLAYGROUND) return process.env.GRAPHERSAL_PLAYGROUND;
  try {
    const out = execFileSync(
      'cargo',
      ['metadata', '--format-version', '1', '--offline', '--locked', '--manifest-path', join(UI, '..', 'Cargo.toml')],
      { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'ignore'] },
    );
    const serve = JSON.parse(out).packages.find((p) => p.name === 'graphersal-serve');
    return serve ? join(dirname(serve.manifest_path), 'ui') : null;
  } catch {
    return null;
  }
}

test('each principal\'s profile view is a valid profile of its own tabs', async (t) => {
  const profile = JSON.parse(readFileSync(join(UI, 'profile.json'), 'utf8'));
  const views = JSON.parse(readFileSync(join(UI, 'profile-views.json'), 'utf8'));
  assert.deepEqual(Object.keys(views).sort(), ['anonymous', 'user']);
  const merged = (key) => ({ ...profile, ...views[key], version: '0.1.0' });
  assert.deepEqual(merged('user').tabs.map((x) => x.id), ['mine']);
  assert.deepEqual(merged('anonymous').tabs.map((x) => x.id), ['login']);
  for (const key of Object.keys(views)) {
    for (const tab of views[key].tabs) assert.ok(existsSync(join(UI, tab.module)), `${tab.module} exists`);
  }
  assert.equal(profile.header.module, 'header.js');
  assert.ok(existsSync(join(UI, 'header.js')));
  const dir = playgroundDir();
  const file = dir && join(dir, 'js', 'ui-profiles.js');
  if (!file || !existsSync(file)) {
    if (process.env.CI) assert.fail('no playground (set GRAPHERSAL_PLAYGROUND)');
    t.skip('no playground found (set GRAPHERSAL_PLAYGROUND)');
    return;
  }
  const { validateProfile, createUi } = await import(pathToFileURL(file).href);
  for (const p of [{ ...profile, version: '0.1.0' }, merged('user'), merged('anonymous')]) {
    const valid = validateProfile(p);
    const ui = createUi(valid, { server: true });
    assert.equal(ui.hasWorkspace, false);
    assert.ok(ui.tab(valid.startTab), `${valid.startTab} is one of its tabs`);
  }
});
