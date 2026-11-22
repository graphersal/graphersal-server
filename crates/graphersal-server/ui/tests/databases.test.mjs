// Node tests of the database list model (ui/databases.js): the rows of `GET api/admin/databases`
// (moved with the list from the engine's playground, same assertions), its fetch with the host's
// headers against a mock server (node:http on port 0), the host headers, and the root info a
// profile module gets from the playground (`hostInfo`).
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  databaseHref, databaseRows, fetchDatabases, hostInfo, anyLoading, hostHeaders, DATABASE_STATES,
} from '../databases.js';

const DATABASES = {
  databases: [
    { name: 'zeta', state: 'failed', kind: 'plain', readOnly: false, description: '', reason: 'bad file' },
    { name: 'acme', state: 'loaded', kind: 'plain', readOnly: false, description: 'CRM' },
    { name: 'base', state: 'unloaded', kind: 'base', readOnly: true, description: 'shared base' },
    { name: 'live', state: 'loaded', kind: 'plain', readOnly: false, description: '', aliasOf: 'acme' },
    { name: 'docs', state: 'needs-model', kind: 'plain', readOnly: false, description: '' },
    { name: 'next', state: 'loading', kind: 'layer', readOnly: false, description: '' },
    { name: 'future', state: 'hibernating', kind: 'plain' },
    { state: 'loaded' },
    'nonsense',
  ],
};

test('databaseHref: the page of a database relative to the server root', () => {
  assert.equal(databaseHref('acme'), 'db/acme/');
  assert.equal(databaseHref('a b/c'), 'db/a%20b%2Fc/');
});

test('databaseRows: sorted rows with state words, links and tones; malformed entries counted', () => {
  const { rows, skipped } = databaseRows(DATABASES);
  assert.equal(skipped, 2);
  assert.deepEqual(rows.map((r) => r.name), ['acme', 'base', 'docs', 'future', 'live', 'next', 'zeta']);
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.acme.href, 'db/acme/');
  assert.equal(by.acme.stateText, DATABASE_STATES.loaded);
  assert.equal(by.acme.tone, 'ok');
  assert.equal(by.base.readOnly, true);
  assert.equal(by.docs.stateText, 'needs a model');
  assert.equal(by.zeta.tone, 'danger');
  assert.equal(by.zeta.reason, 'bad file');
  assert.equal(by.live.aliasOf, 'acme');
  assert.equal(by.acme.aliasOf, null);
  assert.equal(by.future.stateText, 'hibernating', 'a state of a newer server is shown as it is');
  assert.equal(anyLoading(rows), true);
  assert.equal(anyLoading(rows.filter((r) => r.state !== 'loading')), false);
  assert.throws(() => databaseRows({}), /no database list/);
});

test('hostHeaders: the host page\'s text headers, none without the global', () => {
  assert.deepEqual(hostHeaders({}), {});
  assert.deepEqual(hostHeaders({ GRAPHERSAL_ENGINE: { headers: { Authorization: 'Bearer t', Bad: 'a\nb', N: 1 } } }), { Authorization: 'Bearer t' });
  assert.deepEqual(hostHeaders(null), {});
});

/** A mock server: the admin list, which requires the host's header. */
async function mockServer() {
  const server = http.createServer((req, res) => {
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    if (req.url === '/api/admin/databases') {
      if (req.headers.authorization !== 'Bearer t') return send(401, { error: 'a token is required', help: 'send it' });
      return send(200, DATABASES);
    }
    return send(404, { error: 'not found' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, base: `http://127.0.0.1:${server.address().port}/` };
}

test('fetchDatabases against a mock server, with the host headers', async () => {
  const { server, base } = await mockServer();
  try {
    const { rows } = await fetchDatabases({ base, fetch: globalThis.fetch, headers: { Authorization: 'Bearer t' } });
    assert.equal(rows.length, 7);
    assert.equal(new URL(rows[0].href, base).href, `${base}db/acme/`);
    await assert.rejects(fetchDatabases({ base, fetch: globalThis.fetch }), (e) => /a token is required/.test(e.message) && e.help === 'send it');
  } finally {
    server.close();
  }
});

test('hostInfo: the root info from api.info.host, {} when the playground passes none', () => {
  const host = { server: 'graphersal-server', uploadBytes: 1024, mcp: null };
  assert.equal(hostInfo({ info: { host } }).uploadBytes, 1024);
  assert.deepEqual(hostInfo({ info: { host: null } }), {});
  assert.deepEqual(hostInfo({ info: {} }), {});
  assert.deepEqual(hostInfo(undefined), {});
});
