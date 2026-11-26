// Node tests of the Database settings tab's model (ui/settings-api.js, round 3 B7): the
// requests (the revision as If-Match), the edit rules against the server's own texts
// (src/settings.rs, src/limits.rs), the rows of every section, and a round against a stateful
// mock of `/api/admin/databases/<name>/settings` (node:http on port 0, shaped after
// book/src/reference/admin_api.md "Database settings"): a description, a limit set and cleared, read-only, a
// stale revision refused (409) and read again.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync } from 'node:fs';
import {
  settingsRequest, createSettings, LIMITS, LIMIT_KEYS, MAX_LIMIT_TEXT, MAX_DESCRIPTION_CHARS, LIMITS_APPLY,
  limitValueReason, limitValueProblem, limitSetBody, limitClearBody, descriptionProblem, limitText,
  limitRows, limitsNote, identityRows, stateRows, storeRows, settingsGrantRows, settingsMcpRows, mcpText,
  backupsText, changedText, readOnlyLocked,
} from '../settings-api.js';
import { isConflict, refusalNotice } from '../admin-api.js';

const SETTINGS_RS = readFileSync(new URL('../../src/settings.rs', import.meta.url), 'utf8');
const LIMITS_RS = readFileSync(new URL('../../src/limits.rs', import.meta.url), 'utf8');

test('requests: the view and a change based on the revision', () => {
  assert.deepEqual(settingsRequest.view('a b'), { method: 'GET', path: 'api/admin/databases/a%20b/settings' });
  assert.deepEqual(settingsRequest.change('acme', { description: 'x' }), {
    method: 'PUT', path: 'api/admin/databases/acme/settings', body: { description: 'x' },
  });
  assert.deepEqual(settingsRequest.change('acme', { autoLoad: true }, 'r1'), {
    method: 'PUT', path: 'api/admin/databases/acme/settings', body: { autoLoad: true }, ifMatch: 'r1',
  });
});

test('the limit keys and the edit rules are the server\'s', () => {
  const rustKeys = [...LIMITS_RS.match(/QUERY_LIMIT_KEYS: \[&str; 8\] = \[([^\]]+)\]/)[1].matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
  assert.deepEqual(LIMIT_KEYS, rustKeys);
  assert.equal(LIMITS.length, 8);
  assert.ok(SETTINGS_RS.includes(`MAX_DESCRIPTION_CHARS: usize = ${MAX_DESCRIPTION_CHARS};`));
  assert.equal(MAX_LIMIT_TEXT, String(2n ** 63n - 1n));
  // The texts the server refuses with.
  assert.ok(SETTINGS_RS.includes('"0 = the engine\'s default of 128"'));
  assert.ok(SETTINGS_RS.includes('must be a whole number from 0 to {MAX_LIMIT_VALUE} ({zero}), or null to'));
  assert.equal(limitValueReason('timeout_ms'), `limits.timeout_ms must be a whole number from 0 to ${MAX_LIMIT_TEXT} (0 = no limit), or null to clear the override`);
  assert.ok(SETTINGS_RS.includes('"a description is one line of text: no line breaks, tabs or control characters"'));
  assert.ok(SETTINGS_RS.replace(/\s+\\\n\s+/g, ' ').includes(LIMITS_APPLY.charAt(0).toLowerCase() + LIMITS_APPLY.slice(1, -1)), 'the apply text');

  assert.equal(limitValueProblem('timeout_ms', '5000'), null);
  assert.equal(limitValueProblem('timeout_ms', '30_000'), null);
  assert.equal(limitValueProblem('timeout_ms', '0'), null);
  for (const bad of ['', '-1', '1.5', 'lots', ' ', '1e3']) {
    assert.equal(limitValueProblem('timeout_ms', bad), limitValueReason('timeout_ms'), bad);
  }
  assert.match(limitValueProblem('max_value_depth', 'x'), /default of 128/);
  assert.equal(limitValueProblem('timeout_ms', '9223372036854775808'), limitValueReason('timeout_ms'));
  assert.match(limitValueProblem('timeout_ms', '9007199254740992'), /console/);
  assert.match(limitValueProblem('timeout', '1'), /unknown query limit 'timeout'/);
  assert.deepEqual(limitSetBody('max_operations', ' 1_000 '), { limits: { max_operations: 1000 } });
  assert.deepEqual(limitClearBody('max_operations'), { limits: { max_operations: null } });

  assert.equal(descriptionProblem(''), null);
  assert.equal(descriptionProblem('Acme\'s graph, ünïcode'), null);
  assert.match(descriptionProblem('two\nlines'), /one line/);
  assert.match(descriptionProblem('tab\there'), /one line/);
  assert.match(descriptionProblem('x'.repeat(1001)), /at most 1000/);
  assert.equal(descriptionProblem('é'.repeat(1000)), null);
});

/** A settings answer as the server gives it (book/src/reference/admin_api.md). */
function answerOf({ loaded = true, override = null, inEffect = null, reloadNeeded = false } = {}) {
  return {
    name: 'acme',
    revision: 'rev0',
    database: {
      name: 'acme', state: loaded ? 'loaded' : 'unloaded', kind: 'plain', role: 'normal', readOnly: false, autoLoad: false,
      description: 'Acme', created: '2026-11-26T10:00:00Z', sizeBytes: 2048, memoryBytes: loaded ? 4096 : null,
    },
    aliases: ['prod'],
    layers: [],
    limits: {
      loaded,
      reloadNeeded,
      applies: 'x',
      serverError: null,
      keys: LIMIT_KEYS.map((key) => {
        const row = { key, override: null, server: key === 'timeout_ms' ? 30000 : null };
        if (key === 'max_operations') row.override = override;
        row.effective = row.override ?? row.server;
        row.source = row.override !== null ? 'database' : row.server !== null ? 'server' : 'none';
        if (loaded) row.inEffect = key === 'max_operations' ? inEffect : row.server;
        return row;
      }),
    },
    mcp: { enabled: true, endpointBuilt: true, serving: true, revision: 'm', names: [
      { name: 'acme', kind: 'database', effectiveExposed: true, effectiveReadOnly: false, sessions: 1, calls: 3 },
      { name: 'prod', kind: 'alias', target: 'acme', effectiveExposed: false, effectiveReadOnly: true },
    ] },
    grants: [
      { subject: 'user:bob', database: 'acme', level: 'write', via: 'database' },
      { subject: 'user:carol', database: 'prod', level: 'read', via: 'alias' },
      { subject: 'anonymous', database: '*', level: 'read', via: 'all' },
    ],
    admins: ['admin'],
    backups: { count: 2, bytes: 4096, latest: { backup: '2026-11-26T10-00-00Z' } },
    store: {
      storeId: '0123456789abcdef0123456789abcdef', graphId: 'f'.repeat(32), damagePolicy: 'maintenance', chunkBytes: 1048576,
      createdAt: '2026-11-26T09:00:00Z', formatVersion: 4, backend: 'directory', commitSeq: 7, lastCommitAt: '2026-11-26T10:00:00Z',
      layerBase: null, snapshots: 1, walBytes: 512, clean: true,
    },
  };
}

test('limit rows: override, server default, next load, source, in effect now', () => {
  const rows = limitRows(answerOf({ override: 1000, inEffect: null, reloadNeeded: true }));
  assert.deepEqual(rows.map((r) => r.key), LIMIT_KEYS);
  const ops = rows.find((r) => r.key === 'max_operations');
  assert.equal(ops.override, 1000);
  assert.equal(ops.effective, 1000);
  assert.equal(ops.sourceText, 'this database');
  assert.equal(ops.inEffect, null);
  assert.equal(ops.differs, true, 'the next load differs from now');
  const timeout = rows.find((r) => r.key === 'timeout_ms');
  assert.equal(timeout.sourceText, 'server default');
  assert.equal(timeout.differs, false);
  assert.equal(rows.find((r) => r.key === 'max_map_size').sourceText, 'not limited');
  const unloaded = limitRows(answerOf({ loaded: false, override: 5 }));
  assert.ok(unloaded.every((r) => !r.loaded && !r.differs && r.inEffect === null));
  assert.equal(limitsNote(answerOf({ reloadNeeded: true })).tone, 'warn');
  assert.equal(limitsNote(answerOf()).text, LIMITS_APPLY);
  assert.equal(limitText('timeout_ms', 0), '0 (no limit)');
  assert.equal(limitText('max_value_depth', 0), '0 (the default 128)');
  assert.equal(limitText('timeout_ms', null), '—');
  assert.equal(limitText('timeout_ms', 5000), '5000');
  assert.deepEqual(limitRows({}).map((r) => r.source), LIMIT_KEYS.map(() => 'none'));
});

test('sections: identity, state and sizes, store, grants, MCP, backups', () => {
  const a = answerOf();
  const identity = Object.fromEntries(identityRows(a).map(([k, v]) => [k, v]));
  assert.equal(identity.Name, 'acme');
  assert.equal(identity.Kind, 'plain');
  assert.equal(identity.Aliases, 'prod');
  assert.equal(identity.Created, '2026-11-26T10:00:00Z');
  const base = identityRows({ ...a, database: { ...a.database, role: 'base' }, layers: ['l1', 'l2'] });
  assert.deepEqual(base.find(([k]) => k === 'Layers over it'), ['Layers over it', 'l1, l2']);
  assert.equal(readOnlyLocked({ database: { role: 'base' } }), true);
  assert.equal(readOnlyLocked(a), false);
  const state = Object.fromEntries(stateRows(a).map(([k, v]) => [k, v]));
  assert.equal(state.State, 'loaded');
  assert.equal(state['On disk'], '2.0 KB');
  assert.equal(state['In memory'], '4.0 KB');
  assert.equal(Object.fromEntries(stateRows(answerOf({ loaded: false })).map(([k, v]) => [k, v]))['In memory'], '— (not loaded)');
  const store = Object.fromEntries(storeRows(a).map(([k, v]) => [k, v]));
  assert.equal(store['Store id'], '0123456789abcdef0123456789abcdef');
  assert.equal(store['Damage policy'], 'maintenance');
  assert.equal(store['Chunk size'], '1.0 MB');
  assert.equal(store.Backend, 'directory');
  assert.equal(store['Format version'], '4');
  assert.equal(store['Last commit'], '7 at 2026-11-26T10:00:00Z');
  assert.deepEqual(storeRows({ storeError: 'broken' }), [['Error', 'broken']]);
  const grants = settingsGrantRows(a);
  assert.deepEqual(grants.map((g) => [g.subject, g.on, g.via]), [
    ['user:bob', 'acme', 'database'], ['user:carol', 'prod', 'alias'], ['anonymous', '* (every database)', 'all'],
  ]);
  assert.match(grants[1].viaText, /only by that name/);
  const mcp = settingsMcpRows(a);
  assert.deepEqual(mcp.map((r) => [r.name, r.kind, r.offered, r.readOnly, r.sessions]), [
    ['acme', 'database', true, false, 1], ['prod', 'alias', false, true, 0],
  ]);
  assert.equal(mcpText(a), 'MCP is on.');
  assert.equal(mcpText({ mcp: { enabled: false, endpointBuilt: true } }), 'MCP is switched off for the whole server.');
  assert.equal(mcpText({ mcp: { error: 'mcp.toml: bad' } }), 'mcp.toml: bad');
  assert.equal(backupsText(a), '2 backups (4.0 KB), latest 2026-11-26T10-00-00Z');
  assert.equal(backupsText({ backups: { count: 0 } }), 'No backups yet.');
  assert.equal(changedText('acme', { changed: ['description'] }), 'acme: changed description.');
  assert.equal(changedText('acme', { changed: ['limits.timeout_ms'], limits: { loaded: true } }), 'acme: changed limits.timeout_ms. The limits apply at its next load.');
});

/** A stateful mock of the settings route of `acme` (alias `prod`). */
async function mockServer() {
  const state = { n: 0, description: 'Acme', readOnly: false, autoLoad: false, limits: {} };
  const revision = () => `rev${state.n}`;
  const answer = (changed) => {
    const a = answerOf({ loaded: false });
    a.revision = revision();
    Object.assign(a.database, { description: state.description, readOnly: state.readOnly, autoLoad: state.autoLoad });
    a.limits.keys = LIMIT_KEYS.map((key) => {
      const override = key in state.limits ? state.limits[key] : null;
      return { key, override, server: null, effective: override, source: override === null ? 'none' : 'database' };
    });
    if (changed) a.changed = changed;
    return a;
  };
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
      log.push({ method: req.method, path: url.pathname, ifMatch: req.headers['if-match'] || '', body });
      if (url.pathname !== '/api/admin/databases/acme/settings') return send(404, { error: 'no such database' });
      if (req.method === 'GET') return send(200, answer());
      if (req.method !== 'PUT') return send(405, { error: 'method' });
      const expected = (req.headers['if-match'] || '').replace(/"/g, '');
      if (expected && expected !== revision()) {
        return send(409, { error: "the settings of database 'acme' changed", help: 'changed by someone else: reload', revision: revision() });
      }
      const parsed = JSON.parse(body || '{}');
      const changed = [];
      if ('description' in parsed) { state.description = parsed.description; changed.push('description'); }
      if ('readOnly' in parsed) { state.readOnly = parsed.readOnly; changed.push('readOnly'); }
      if ('autoLoad' in parsed) { state.autoLoad = parsed.autoLoad; changed.push('autoLoad'); }
      for (const [key, value] of Object.entries(parsed.limits || {})) {
        if (value === null) delete state.limits[key]; else state.limits[key] = value;
        changed.push(`limits.${key}`);
      }
      state.n++;
      return send(200, answer(changed));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    base: `http://127.0.0.1:${server.address().port}/`,
    log,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

test('a round against the mock: description, a limit set and cleared, read-only, a stale revision', async () => {
  const srv = await mockServer();
  try {
    const settings = createSettings({ base: srv.base, fetch });
    let view = await settings.view('acme');
    const first = view.revision;
    view = await settings.change('acme', { description: 'Acme graph' }, first);
    assert.equal(view.database.description, 'Acme graph');
    assert.deepEqual(view.changed, ['description']);
    // Another browser's change based on the old revision: refused, a warning, read again.
    await assert.rejects(settings.change('acme', { autoLoad: true }, first), (err) => {
      assert.ok(isConflict(err));
      assert.equal(refusalNotice(err).tone, 'warn');
      return true;
    });
    view = await settings.view('acme');
    assert.equal(view.database.autoLoad, false, 'nothing changed');
    view = await settings.change('acme', limitSetBody('max_operations', '1_000'), view.revision);
    assert.equal(limitRows(view).find((r) => r.key === 'max_operations').override, 1000);
    view = await settings.change('acme', limitClearBody('max_operations'), view.revision);
    assert.equal(limitRows(view).find((r) => r.key === 'max_operations').source, 'none');
    view = await settings.change('acme', { readOnly: true }, view.revision);
    assert.equal(view.database.readOnly, true);
    const puts = srv.log.filter((e) => e.method === 'PUT');
    assert.deepEqual(puts.map((e) => e.ifMatch), ['"rev0"', '"rev0"', '"rev1"', '"rev2"', '"rev3"']);
    assert.equal(puts[2].body, '{"limits":{"max_operations":1000}}');
    assert.equal(puts[3].body, '{"limits":{"max_operations":null}}');
  } finally {
    await srv.close();
  }
});

test('the tab: fresh on every visit, sortable tables, revisions, links; the Databases row opens it', () => {
  const src = readFileSync(new URL('../settings-tab.js', import.meta.url), 'utf8');
  assert.ok(src.includes('useFreshData('), 'fresh on every visit');
  assert.ok(src.includes('s.revision'), 'changes carry the revision');
  for (const id of ['data-settings-pick', 'data-settings-description', 'data-settings-read-only', 'data-settings-auto-load', 'data-settings-limits-note', 'data-settings-store', 'data-settings-reload']) {
    assert.ok(src.includes(id), id);
  }
  for (const tab of ["navigate('mcp')", "navigate('users')", "navigate('backups'"]) assert.ok(src.includes(tab), tab);
  const profile = JSON.parse(readFileSync(new URL('../profile.json', import.meta.url), 'utf8'));
  assert.deepEqual(profile.tabs.find((t) => t.id === 'settings'), { id: 'settings', title: 'Database settings', module: 'settings-tab.js' });
  const databases = readFileSync(new URL('../databases-tab.js', import.meta.url), 'utf8');
  assert.ok(databases.includes("navigate('settings', { db: row.aliasOf || row.name })"), 'Settings… on every row');
  assert.ok(databases.includes('data-settings-open'));
});
