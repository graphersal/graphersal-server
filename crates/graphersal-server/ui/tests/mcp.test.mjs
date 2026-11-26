// Node tests of the MCP tab's model (ui/mcp-api.js, round 3 B6): the requests (with the
// revision as If-Match), the status, the per-database rows, the usage tables, how to connect,
// and a round against a stateful mock of `/api/admin/mcp` (node:http on port 0, shaped after
// book/src/reference/admin_api.md "MCP"): switch off, hide a database, make it read-only, a stale revision
// refused (409) and read again.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync } from 'node:fs';
import {
  mcpRequest, createMcp, statusOf, mcpRows, usageOf, usageText, connectInfo, exposedTitle,
  readOnlyTitle, TOKEN_PLACEHOLDER, MCP_ROW_KINDS,
} from '../mcp-api.js';
import { isConflict, refusalNotice } from '../admin-api.js';

test('requests: the state, the global switch and a name, each change with its revision', () => {
  assert.deepEqual(mcpRequest.state(), { method: 'GET', path: 'api/admin/mcp' });
  assert.deepEqual(mcpRequest.setEnabled(false), { method: 'PUT', path: 'api/admin/mcp', body: { enabled: false } });
  assert.deepEqual(mcpRequest.setEnabled(1, 'r1'), { method: 'PUT', path: 'api/admin/mcp', body: { enabled: true }, ifMatch: 'r1' });
  assert.deepEqual(mcpRequest.setName('a b', { exposed: false }, 'r2'), {
    method: 'PUT', path: 'api/admin/mcp/databases/a%20b', body: { exposed: false }, ifMatch: 'r2',
  });
  assert.deepEqual(mcpRequest.setName('acme', { readOnly: true, other: 1 }).body, { readOnly: true });
});

test('status: on, switched off, or no endpoint because server.toml turns MCP off', () => {
  assert.equal(statusOf({ enabled: true, endpointBuilt: true }).tone, 'ok');
  const off = statusOf({ enabled: false, endpointBuilt: true });
  assert.equal(off.text, 'MCP is switched off');
  assert.match(off.help, /503/);
  assert.match(off.help, /kept/);
  const none = statusOf({ enabled: true, endpointBuilt: false });
  assert.equal(none.text, 'MCP is off in server.toml');
  assert.match(none.help, /restart/);
});

test('rows: databases, aliases with their target, names left by deleted databases; titles', () => {
  const rows = mcpRows({ databases: [
    { name: 'acme', kind: 'database', exposed: true, readOnly: false, effectiveExposed: true, effectiveReadOnly: true, sessions: 2, calls: 5, errors: 1 },
    { name: 'prod', kind: 'alias', target: 'acme', exposed: true, readOnly: true, effectiveExposed: false, effectiveReadOnly: true },
    { name: 'gone', kind: 'missing', exposed: false, readOnly: false },
    { kind: 'database' },
    null,
  ] });
  assert.deepEqual(MCP_ROW_KINDS, ['database', 'alias', 'missing']);
  assert.deepEqual(rows.map((r) => r.nameText), ['acme', 'prod → acme', 'gone (no such database)']);
  assert.equal(rows[0].sessions, 2);
  assert.equal(rows[1].sessions, 0, 'a missing count is 0');
  assert.equal(rows[2].exposed, false);
  assert.match(exposedTitle(rows[2]), /Hidden from MCP/);
  assert.match(exposedTitle(rows[1]), /its database acme is hidden/);
  assert.match(exposedTitle(rows[0]), /Offered/);
  assert.match(readOnlyTitle(rows[1]), /only read it/);
  assert.match(readOnlyTitle(rows[0]), /read-only for MCP anyway/i);
  assert.deepEqual(mcpRows({}), []);
});

test('usage: sessions, tool calls, users and the recent errors; null offline', () => {
  assert.equal(usageOf({ usage: null }), null);
  assert.equal(usageText(null), 'The statistics exist only while the server runs.');
  const usage = usageOf({ usage: {
    startedAtMs: 1,
    sessions: { open: 1, byUser: [{ user: 'bob', sessions: 1 }], byDatabase: [], withoutDatabase: 0 },
    calls: { total: 3, errors: 1, refused: 0 },
    tools: [{ tool: 'gql', calls: 2, errors: 0 }, { tool: 'use_graph', calls: 1, errors: 1 }],
    users: [{ user: 'bob', calls: 3, errors: 1 }],
    recentErrors: [{ atMs: Date.UTC(2026, 10, 26, 12, 0, 0), tool: 'use_graph', user: 'bob', database: 'zz', message: "no database or alias 'zz'" }],
    recentErrorsMax: 50,
  } });
  assert.deepEqual(usage.tools.map((t) => [t.name, t.calls]), [['gql', 2], ['use_graph', 1]]);
  assert.deepEqual(usage.sessionsByUser, [{ name: 'bob', sessions: 1 }]);
  assert.equal(usage.recentErrors[0].time, '2026-11-26 12:00:00 UTC');
  assert.equal(usage.recentErrors[0].database, 'zz');
  assert.equal(usageText(usage), '1 open session, 3 tool calls (1 error, 0 requests refused while off) since the server started');
});

test('connect: the endpoint URL and the claude mcp add command with a placeholder token', () => {
  const info = connectInfo('http://127.0.0.1:8484/', '/mcp');
  assert.equal(info.url, 'http://127.0.0.1:8484/mcp');
  assert.equal(info.command, `claude mcp add --transport http --header "Authorization: Bearer ${TOKEN_PLACEHOLDER}" graphersal http://127.0.0.1:8484/mcp`);
  assert.ok(info.database('acme').endsWith(' acme http://127.0.0.1:8484/db/acme/mcp'));
  assert.equal(connectInfo('https://db.example.com/proxy').url, 'https://db.example.com/proxy/mcp');
});

/** A stateful mock of the MCP routes (acme and the alias prod). */
async function mockServer() {
  const state = { enabled: true, names: {}, n: 0 };
  const revision = () => `rev${state.n}`;
  const answer = () => ({
    enabled: state.enabled, endpointBuilt: true, serving: state.enabled, online: true, endpoint: '/mcp',
    revision: revision(),
    databases: [['acme', 'database', null], ['prod', 'alias', 'acme']].map(([name, kind, target]) => {
      const own = state.names[name] || { exposed: true, readOnly: false };
      return { name, kind, target, ...own, effectiveExposed: own.exposed, effectiveReadOnly: own.readOnly, sessions: 0, calls: 0, errors: 0 };
    }),
    usage: { sessions: { open: 0, byUser: [] }, calls: { total: 0, errors: 0, refused: 0 }, tools: [], users: [], recentErrors: [] },
  });
  const log = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const send = (status, value) => {
        res.writeHead(status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(value));
      };
      const url = new URL(req.url, 'http://x');
      log.push({ method: req.method, path: url.pathname, ifMatch: req.headers['if-match'] || '' });
      if (req.method === 'GET' && url.pathname === '/api/admin/mcp') return send(200, answer());
      if (req.method !== 'PUT') return send(405, { error: 'method' });
      const expected = (req.headers['if-match'] || '').replace(/"/g, '');
      if (expected && expected !== revision()) {
        return send(409, { error: 'the MCP settings changed', help: 'changed by someone else: reload', revision: revision() });
      }
      const parsed = JSON.parse(body || '{}');
      if (url.pathname === '/api/admin/mcp') {
        state.enabled = parsed.enabled;
      } else {
        const name = decodeURIComponent(url.pathname.split('/').pop());
        const own = state.names[name] || { exposed: true, readOnly: false };
        if (typeof parsed.exposed === 'boolean') own.exposed = parsed.exposed;
        if (typeof parsed.readOnly === 'boolean') own.readOnly = parsed.readOnly;
        state.names[name] = own;
      }
      state.n++;
      return send(200, answer());
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    base: `http://127.0.0.1:${server.address().port}/`,
    log,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

test('a round against the mock: off and on, hide, read-only, a stale revision refused and read again', async () => {
  const srv = await mockServer();
  try {
    const mcp = createMcp({ base: srv.base, fetch });
    let state = await mcp.state();
    assert.equal(statusOf(state).tone, 'ok');
    const first = state.revision;
    state = await mcp.setEnabled(false, first);
    assert.equal(statusOf(state).text, 'MCP is switched off');
    // Another browser's change based on the old revision: refused, a warning that reads again.
    await assert.rejects(mcp.setName('acme', { exposed: false }, first), (err) => {
      assert.ok(isConflict(err));
      assert.equal(refusalNotice(err).tone, 'warn');
      return true;
    });
    state = await mcp.state();
    state = await mcp.setName('acme', { exposed: false }, state.revision);
    state = await mcp.setName('prod', { readOnly: true }, state.revision);
    const rows = mcpRows(state);
    assert.equal(rows.find((r) => r.name === 'acme').exposed, false);
    assert.equal(rows.find((r) => r.name === 'prod').readOnly, true);
    state = await mcp.setEnabled(true, state.revision);
    assert.equal(state.enabled, true);
    assert.deepEqual(srv.log.filter((e) => e.method === 'PUT').map((e) => e.ifMatch), ['"rev0"', '"rev0"', '"rev1"', '"rev2"', '"rev3"']);
  } finally {
    await srv.close();
  }
});

test('the tab: reads its state on every visit, sorts every table, sends the revision', () => {
  const src = readFileSync(new URL('../mcp-tab.js', import.meta.url), 'utf8');
  assert.ok(src.includes('useFreshData(refresh)'), 'fresh on every visit');
  assert.ok(src.includes('s.revision'), 'changes carry the revision');
  for (const id of ['data-mcp-switch', 'data-mcp-exposed', 'data-mcp-read-only', 'data-mcp-status']) {
    assert.ok(src.includes(id), id);
  }
  const profile = JSON.parse(readFileSync(new URL('../profile.json', import.meta.url), 'utf8'));
  assert.deepEqual(profile.tabs.find((t) => t.id === 'mcp'), { id: 'mcp', title: 'MCP', module: 'mcp-tab.js' });
});
