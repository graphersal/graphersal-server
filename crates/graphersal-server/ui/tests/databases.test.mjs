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

test('sizes: on disk and in memory, human-readable with the exact bytes in the tooltip', async () => {
  const { exactBytes } = await import('../admin-api.js');
  const { diskTitle, memoryText, memoryTitle } = await import('../databases.js');
  assert.equal(exactBytes(0), '0 bytes');
  assert.equal(exactBytes(1), '1 byte');
  assert.equal(exactBytes(1572864), '1,572,864 bytes');
  assert.equal(exactBytes(null), '');
  const { rows } = databaseRows({
    databases: [
      { name: 'acme', state: 'loaded', kind: 'plain', sizeBytes: 9728, memoryBytes: 1572864 },
      { name: 'idle', state: 'unloaded', kind: 'plain', sizeBytes: 100, memoryBytes: null },
      { name: 'l1', state: 'loaded', kind: 'layer', base: 'prod', sizeBytes: 50, memoryBytes: 2048, sharedMemoryBytes: 4096 },
      { name: 'old', state: 'loaded', kind: 'plain', sizeBytes: 1 },
    ],
  });
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.acme.memoryReported, true);
  assert.equal(by.idle.memoryReported, true, 'null is still a reported field');
  assert.equal(by.old.memoryReported, false, 'an older server sends no memory');
  assert.equal(memoryText(by.acme), '1.5 MB');
  assert.match(memoryTitle(by.acme), /^1,572,864 bytes: the graph's data/);
  assert.equal(memoryText(by.idle), '', 'not loaded: no memory');
  assert.equal(memoryTitle(by.idle), '');
  assert.equal(by.l1.sharedMemoryBytes, 4096);
  assert.match(memoryTitle(by.l1), /^2,048 bytes: this layer's own changes; its base "prod" takes 4\.0 KB \(4,096 bytes\)/);
  assert.match(diskTitle(by.acme), /^9,728 bytes in the database's directory/);
});

test('sizes: the On disk and In memory columns sort numerically, unloaded last', async () => {
  const { sortRows } = await import('../sort.js');
  const { SIZE_COLUMNS } = await import('../databases.js');
  const { rows } = databaseRows({
    databases: [
      { name: 'a', state: 'loaded', sizeBytes: 1048576, memoryBytes: 10 },
      { name: 'b', state: 'unloaded', sizeBytes: 9, memoryBytes: null },
      { name: 'c', state: 'loaded', sizeBytes: 2048, memoryBytes: 1048576 },
      { name: 'd', state: 'loaded', sizeBytes: 100, memoryBytes: 900 },
    ],
  });
  const names = (key, dir) => sortRows(rows, { key, dir }, SIZE_COLUMNS).map((r) => r.name);
  assert.deepEqual(names('size', 'asc'), ['b', 'd', 'c', 'a']);
  assert.deepEqual(names('size', 'desc'), ['a', 'c', 'd', 'b']);
  assert.deepEqual(names('memory', 'asc'), ['a', 'd', 'c', 'b']);
  assert.deepEqual(names('memory', 'desc'), ['c', 'd', 'a', 'b'], 'not loaded: last both ways');
});

test('memoryRows: the server memory summary, free under the limit or on the machine', async () => {
  const { memoryRows } = await import('../databases.js');
  assert.deepEqual(memoryRows({}), []);
  assert.deepEqual(memoryRows(null), []);
  const GiB = 1024 ** 3;
  const rows = memoryRows({
    memory: {
      processBytes: 150 * 1024 * 1024, liveBytes: null, databasesBytes: 2 * 1024 * 1024,
      machineTotalBytes: 16 * GiB, machineAvailableBytes: 8 * GiB,
      limitBytes: 12 * GiB, limitSource: 'physical', configuredLimit: null, headroomBytes: null,
      availableBytes: 12 * GiB, freeBytes: 12 * GiB - 150 * 1024 * 1024, freeBasis: 'limit',
    },
  });
  assert.deepEqual(rows.map((r) => r.key), ['process', 'databases', 'machine', 'limit', 'free']);
  const by = Object.fromEntries(rows.map((r) => [r.key, r]));
  assert.equal(by.process.text, '150 MB');
  assert.match(by.process.title, /^157,286,400 bytes: the resident memory/);
  assert.equal(by.databases.text, '2.0 MB');
  assert.equal(by.machine.text, '8.0 GB available of 16 GB');
  assert.equal(by.limit.text, "12 GB (80 % of the machine's memory)");
  assert.match(by.limit.title, /memory_limit not set \("auto"\)\.$/);
  assert.match(by.free.text, / under the limit$/);

  const none = Object.fromEntries(memoryRows({
    memory: {
      processBytes: null, databasesBytes: 0, machineTotalBytes: null, machineAvailableBytes: 4096,
      limitBytes: null, limitSource: 'none', configuredLimit: 'none', headroomBytes: 1024,
      freeBytes: 4096, freeBasis: 'machine',
    },
  }).map((r) => [r.key, r]));
  assert.equal(none.process.text, 'unknown');
  assert.match(none.process.title, /not known on this platform/);
  assert.equal(none.machine.text, '4.0 KB available of unknown');
  assert.equal(none.limit.text, 'none');
  assert.match(none.limit.title, /memory_limit = "none", memory_headroom = 1024\.$/);
  assert.equal(none.free.text, '4.0 KB on the machine');
  assert.equal(memoryRows({ memory: { configuredLimit: 8589934592, limitBytes: 8589934592, limitSource: 'configured' } })
    .find((r) => r.key === 'limit').text, '8.0 GB (set in server.toml)');
});

test('memoryRows: free is capped by what the machine has available', async () => {
  const { memoryRows } = await import('../databases.js');
  const free = memoryRows({
    memory: {
      processBytes: 1024, databasesBytes: 0, machineTotalBytes: 8192, machineAvailableBytes: 2048,
      limitBytes: 6144, limitSource: 'configured', configuredLimit: 6144, headroomBytes: null,
      freeBytes: 2048, freeBasis: 'machine-below-limit',
    },
  }).find((r) => r.key === 'free');
  assert.equal(free.text, '2.0 KB on the machine (less than the limit leaves)');
  assert.match(free.title, /^2,048 bytes: the machine's available memory: less than the limit/);
});
