// Node tests of the server settings (round 4 B-SETTINGS): the model ui/server-settings-api.js
// (rows and sections of `GET /api/admin/config`, the input checks, the texts, the requests with
// the revision as If-Match), a round against a stateful mock of `/api/admin/config` (shaped
// after book/src/reference/admin_api.md "Server settings": a key set, reset, a stale revision
// refused), the database settings' inherited/override marks, the create form's defaults, and
// the templates (the Settings tab's "Server defaults" and "Server" entries, the top bar's
// restart-pending badge).
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync } from 'node:fs';
import {
  configRequest, createConfig, SECTIONS, APPLIES, isDefaultsKey, settingRows, settingSections, inputText,
  parseInput, changeSet, pendingKeys, pendingText, savedNotice, valueText, unsetText, newDatabaseText,
  runningNote, placeholderText,
} from '../server-settings-api.js';
import { limitRows } from '../settings-api.js';
import { createBody, isConflict } from '../admin-api.js';
import { newDatabaseDefaults } from '../databases.js';

const SETTINGS_RS = readFileSync(new URL('../../src/server_settings.rs', import.meta.url), 'utf8');
const src = (file) => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');

/** The keys of the server's table (src/server_settings.rs `KEYS`), in file order. */
const RUST_KEYS = [...SETTINGS_RS.split('pub const KEYS')[1].split('];')[0].matchAll(/key\(\s*"([a-z_.]+)"/g)].map((m) => m[1]);

/** A view answer as the server builds it (`server_settings::view_json`). */
function viewOf({ set = {}, pending = [], running = true } = {}) {
  const kinds = { listen: 'text', 'mcp.enabled': 'bool', 'defaults.damage_policy': 'text', 'defaults.auto_load': 'bool', 'defaults.read_only': 'bool', 'limits.memory_limit': 'memory', 'access.enabled': 'bool', 'http.public_scheme': 'text', 'http.allowed_hosts': 'hosts', 'jobs.backup_root': 'text' };
  const defaults = { listen: '127.0.0.1:8484', 'mcp.enabled': true, 'defaults.auto_load': false, 'limits.upload_bytes': 4294967296, 'http.public_scheme': 'http', 'http.allowed_hosts': [], 'defaults.damage_policy': 'maintenance' };
  const applies = (key) => {
    if (key.startsWith('limits.query.')) return 'load';
    if (key.startsWith('defaults.')) return 'create';
    if (['limits.upload_bytes', 'limits.memory_limit', 'limits.memory_headroom', 'sessions.lifetime_secs', 'sessions.idle_timeout_secs', 'http.public_scheme'].includes(key)) return 'now';
    return 'restart';
  };
  return {
    path: '/srv/server.toml',
    revision: 'rev0',
    running,
    restartPending: pending,
    mcpExposeNew: false,
    keys: RUST_KEYS.map((key) => {
      const section = key.includes('.') ? key.slice(0, key.lastIndexOf('.')) : '';
      const def = key in defaults ? defaults[key] : null;
      return {
        key,
        section,
        value: key in set ? set[key] : def,
        default: def,
        set: key in set,
        kind: kinds[key] || 'count',
        choices: key === 'defaults.damage_policy' ? ['maintenance', 'continue'] : key === 'http.public_scheme' ? ['http', 'https'] : null,
        applies: applies(key),
        appliesText: '',
        running: def,
        pending: pending.includes(key),
        help: `help of ${key}`,
      };
    }),
  };
}

test('the sections cover every key of the server, in file order', () => {
  assert.ok(RUST_KEYS.length >= 27, RUST_KEYS.join(','));
  const sections = settingSections(viewOf());
  assert.deepEqual(sections.map((s) => s.id), SECTIONS.map((s) => s.id));
  assert.deepEqual(sections.flatMap((s) => s.rows.map((r) => r.key)), RUST_KEYS);
  // The Server defaults entry: the [defaults] and the query limits only.
  const defaults = settingSections(viewOf(), isDefaultsKey);
  assert.deepEqual(defaults.map((s) => s.id), ['defaults', 'limits.query']);
  assert.equal(defaults[1].rows.length, 8);
  // Every class the server sends has its words.
  for (const cls of ['now', 'load', 'create', 'restart']) assert.ok(SETTINGS_RS.includes(`"${cls}"`) && APPLIES[cls].text, cls);
});

test('rows: value or default, the class, pending with the running value, creation parameters', () => {
  const rows = settingRows(viewOf({ set: { 'limits.upload_bytes': 1024, 'http.allowed_hosts': ['a', 'b'], listen: '0.0.0.0:1' }, pending: ['listen'] }));
  const row = (key) => rows.find((r) => r.key === key);
  assert.equal(row('limits.upload_bytes').text, '1024');
  assert.equal(row('limits.upload_bytes').name, 'upload_bytes');
  assert.equal(row('limits.upload_bytes').appliesText, 'applies now');
  assert.equal(row('limits.max_connections').text, 'not set (default)');
  assert.equal(row('mcp.enabled').text, 'on (default)');
  assert.equal(row('http.allowed_hosts').text, 'a, b');
  assert.equal(row('limits.query.timeout_ms').text, 'not limited (default)');
  assert.equal(row('limits.query.timeout_ms').appliesText, 'at next load');
  assert.equal(row('listen').pending, true);
  assert.equal(row('listen').running, '127.0.0.1:8484');
  assert.equal(row('defaults.chunk_bytes').creation, true);
  assert.equal(row('defaults.auto_load').creation, false);
  assert.deepEqual(row('http.public_scheme').choices, ['http', 'https']);
  assert.equal(inputText(row('http.allowed_hosts')), 'a, b');
  assert.equal(inputText(row('limits.max_connections')), '');
  assert.equal(unsetText('limits.memory_limit'), "auto (the engine's default)");
  assert.equal(valueText('sessions.lifetime_secs', null), 'no limit');
  assert.deepEqual(settingRows({ keys: [null, { nokey: 1 }] }), []);
});

test('input checks before sending (the server checks again)', () => {
  const row = (kind, extra = {}) => ({ key: 'k', kind, choices: null, ...extra });
  assert.deepEqual(parseInput(row('count'), '1_024'), { value: 1024 });
  assert.match(parseInput(row('count'), '-1').problem, /whole number/);
  assert.match(parseInput(row('count'), '99999999999999999999').problem, /config set k VALUE/);
  assert.deepEqual(parseInput(row('memory'), 'Auto'), { value: 'auto' });
  assert.deepEqual(parseInput(row('memory'), '4096'), { value: 4096 });
  assert.match(parseInput(row('memory'), 'lots').problem, /auto or none/);
  assert.deepEqual(parseInput(row('hosts'), ' a.example, ,b.example '), { value: ['a.example', 'b.example'] });
  assert.deepEqual(parseInput(row('hosts'), ''), { value: [] });
  assert.deepEqual(parseInput(row('bool'), 'off'), { value: false });
  assert.deepEqual(parseInput(row('text'), ' 127.0.0.1:0 '), { value: '127.0.0.1:0' });
  assert.match(parseInput(row('text'), '').problem, /Reset to default/);
  assert.match(parseInput(row('text', { choices: ['http', 'https'] }), 'ftp').problem, /one of http, https/);
  assert.deepEqual(changeSet('limits.upload_bytes', 5), { 'limits.upload_bytes': 5 });
  assert.deepEqual(changeSet('limits.upload_bytes', null), { 'limits.upload_bytes': null });
});

test('texts: restart pending and the outcome of a save', () => {
  assert.equal(pendingText(viewOf()), '');
  assert.deepEqual(pendingKeys(viewOf({ pending: ['listen', 7] })), ['listen']);
  assert.match(pendingText(viewOf({ pending: ['listen'] })), /^Restart pending: listen\. .*next start/);
  const now = savedNotice({ changed: ['limits.upload_bytes'], applied: ['limits.upload_bytes'], restartPending: [] });
  assert.equal(now.tone, 'ok');
  assert.match(now.help, /Applied now: limits.upload_bytes/);
  const later = savedNotice({ changed: ['listen', 'limits.query.timeout_ms'], applied: [], restartPending: ['listen'], rewritten: true });
  assert.equal(later.tone, 'warn');
  assert.match(later.help, /Restart pending: listen\./);
  assert.match(later.help, /Applies when used: limits.query.timeout_ms/);
  assert.match(later.help, /comments could not be kept/);
  assert.equal(savedNotice({ changed: [] }).text, 'server.toml unchanged: the value was set already.');
});

test('a database: inherited or override per key, and what new databases get', () => {
  const rows = limitRows({ limits: { loaded: false, keys: [
    { key: 'timeout_ms', override: null, server: 5000, effective: 5000, source: 'server' },
    { key: 'max_operations', override: 7, server: 5000, effective: 7, source: 'database' },
  ] } });
  assert.equal(rows.find((r) => r.key === 'timeout_ms').inherited, true);
  assert.equal(rows.find((r) => r.key === 'max_operations').inherited, false);
  assert.equal(rows.find((r) => r.key === 'max_map_size').inherited, true);
  const defaults = { autoLoad: true, readOnly: false, chunkBytes: 65536, damagePolicy: 'continue', mcpExposeNew: false };
  assert.equal(newDatabaseText(defaults), 'A new database starts with the server defaults: auto-load on, read-only off, not offered to MCP.');
  assert.equal(newDatabaseText(defaults, 'store'), 'A new database gets the server defaults: chunk size 65536 bytes, damage policy continue.');
  assert.equal(newDatabaseText({ error: 'x' }), '');
  assert.equal(newDatabaseText(undefined), '');
});

test('the create form starts from the server defaults and says no when a default is on', () => {
  assert.deepEqual(newDatabaseDefaults({ newDatabases: { autoLoad: true, readOnly: null, mcp: true } }), { autoLoad: true, readOnly: false, mcp: true });
  assert.deepEqual(newDatabaseDefaults({}), { autoLoad: false, readOnly: false, mcp: false });
  const on = { autoLoad: true, readOnly: true, mcp: true };
  assert.deepEqual(createBody({ name: 'a', autoLoad: false, readOnly: false, mcp: false }, on).body, { name: 'a', autoLoad: false, readOnly: false, mcp: false });
  assert.deepEqual(createBody({ name: 'a', autoLoad: true, readOnly: true, mcp: true }, on).body, { name: 'a', autoLoad: true, readOnly: true, mcp: true });
  // A base is read-only anyway: no flag.
  assert.deepEqual(createBody({ name: 'b', role: 'base' }, on).body, { name: 'b', role: 'base', autoLoad: false, mcp: false });
  // Without defaults (an older server): only what is checked.
  assert.deepEqual(createBody({ name: 'a' }).body, { name: 'a' });
});

test('requests: the view, a change based on the revision, the MCP default', () => {
  assert.deepEqual(configRequest.view(), { method: 'GET', path: 'api/admin/config' });
  assert.deepEqual(configRequest.change({ listen: '127.0.0.1:0' }, 'r1'), {
    method: 'PUT', path: 'api/admin/config', body: { set: { listen: '127.0.0.1:0' } }, ifMatch: 'r1',
  });
  assert.deepEqual(configRequest.exposeNew(1), { method: 'PUT', path: 'api/admin/mcp', body: { exposeNew: true } });
});

async function mockServer() {
  const state = { n: 0, set: {} };
  const log = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const send = (status, value) => {
        res.writeHead(status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(value));
      };
      log.push({ method: req.method, ifMatch: req.headers['if-match'] || '', body });
      const view = () => ({ ...viewOf({ set: state.set, pending: 'listen' in state.set ? ['listen'] : [] }), revision: `rev${state.n}` });
      if (req.url !== '/api/admin/config') return send(404, { error: 'unknown endpoint' });
      if (req.method === 'GET') return send(200, view());
      const expected = (req.headers['if-match'] || '').replace(/"/g, '');
      if (expected && expected !== `rev${state.n}`) return send(409, { error: 'server.toml changed', help: 'changed by someone else: reload', revision: `rev${state.n}` });
      const parsed = JSON.parse(body);
      const changed = [];
      for (const [key, value] of Object.entries(parsed.set)) {
        if (key === 'limits.upload_bytes' && value === 0) return send(400, { error: "'server.toml': limits.upload_bytes: must be at least 1 (default 4294967296)" });
        if (value === null) delete state.set[key]; else state.set[key] = value;
        changed.push(key);
      }
      state.n++;
      return send(200, { ...view(), changed, applied: changed.filter((k) => k === 'limits.upload_bytes'), rewritten: false });
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { base: `http://127.0.0.1:${server.address().port}/`, log, close: () => new Promise((resolve) => server.close(resolve)) };
}

test('a round against the mock: set, a refused value, a stale revision, a restart key, reset', async () => {
  const srv = await mockServer();
  try {
    const config = createConfig({ base: srv.base, fetch });
    let view = await config.view();
    const first = view.revision;
    view = await config.change(changeSet('limits.upload_bytes', 1024), first);
    assert.deepEqual(view.applied, ['limits.upload_bytes']);
    assert.equal(settingRows(view).find((r) => r.key === 'limits.upload_bytes').text, '1024');
    await assert.rejects(config.change(changeSet('limits.upload_bytes', 0), view.revision), /must be at least 1/);
    await assert.rejects(config.change(changeSet('listen', '0.0.0.0:1'), first), (err) => isConflict(err));
    view = await config.change(changeSet('listen', '0.0.0.0:1'), view.revision);
    assert.deepEqual(pendingKeys(view), ['listen']);
    view = await config.change(changeSet('listen', null), view.revision);
    assert.deepEqual(pendingKeys(view), []);
    const puts = srv.log.filter((e) => e.method === 'PUT');
    assert.deepEqual(puts.map((e) => e.ifMatch), ['"rev0"', '"rev1"', '"rev0"', '"rev1"', '"rev2"']);
    assert.equal(puts[4].body, '{"set":{"listen":null}}');
  } finally {
    await srv.close();
  }
});

test('the templates: the Settings tab\'s server entries, the view and the top bar badge', () => {
  const tab = src('settings-tab.js');
  assert.ok(tab.includes('<option value=":defaults">Server defaults (every database)</option>'));
  assert.ok(tab.includes('<option value=":server">Server (server.toml)</option>'));
  assert.ok(tab.indexOf(':defaults') < tab.indexOf('v-for="n in names"'), 'the server entries come first');
  assert.ok(tab.includes('<server-settings v-if="special" :mode="special" :tick="s.tick"></server-settings>'));
  assert.ok(tab.includes('Reset to inherited'));
  assert.ok(tab.includes("r.inherited ? 'inherited' : 'override'"));
  const view = src('server-settings-view.js');
  assert.ok(view.includes('data-config-pending'));
  assert.ok(view.includes('Reset to default'));
  assert.ok(view.includes('data-config-expose-new'));
  assert.ok(view.includes("'at create only'"));
  assert.ok(view.includes('<form-error :error="s.formErrors[r.key] || null"></form-error>'));
  const header = src('header.js');
  assert.ok(header.includes("'restart pending'") && header.includes("navigate('settings', { db: ':server' })"));
  const profile = JSON.parse(src('profile.json'));
  assert.equal(profile.tabs.find((t) => t.id === 'settings').title, 'Settings');
  const assets = readFileSync(new URL('../../src/ui_assets.rs', import.meta.url), 'utf8');
  for (const file of ['server-settings-api.js', 'server-settings-view.js']) assert.ok(assets.includes(`"${file}"`), file);
});

test('running line: a pending key, a --listen from the command line (not pending), nothing else', () => {
  const rows = settingRows({ keys: [
    { key: 'listen', section: '', value: '127.0.0.1:8484', set: true, applies: 'restart', running: '127.0.0.1:18731', runningSource: 'command line', pending: false },
    { key: 'http.allowed_hosts', section: 'http', value: ['db.example.com'], set: true, applies: 'restart', running: [], pending: true },
    { key: 'limits.upload_bytes', section: 'limits', value: 99, set: true, applies: 'now', running: 99, pending: false },
    { key: 'limits.memory_limit', section: 'limits', value: null, set: false, kind: 'memory', applies: 'now', running: null, pending: false },
    { key: 'limits.memory_headroom', section: 'limits', value: null, set: false, applies: 'now', running: null, pending: false },
  ] });
  const by = Object.fromEntries(rows.map((r) => [r.key, r]));
  assert.equal(by.listen.runningSource, 'command line');
  assert.equal(by.listen.pending, false);
  assert.equal(runningNote(by.listen).text, 'running: 127.0.0.1:18731 (command line)');
  assert.match(runningNote(by.listen).title, /--listen/);
  assert.equal(runningNote(by['http.allowed_hosts']).text, 'running: none');
  assert.equal(runningNote(by['limits.upload_bytes']), null);
  assert.equal(runningNote({ ...by.listen, runningSource: 'picked port' }).text, 'running: 127.0.0.1:18731 (picked port)');
  // Short placeholders: the help says the rest.
  assert.equal(placeholderText(by['limits.memory_limit']), 'auto');
  assert.equal(placeholderText(by['limits.memory_headroom']), 'a tenth');
  assert.equal(placeholderText(by['limits.upload_bytes']), '99');
  for (const r of rows) assert.ok(placeholderText(r).length <= 16, `${r.key}: ${placeholderText(r)}`);
  const view = src('server-settings-view.js');
  assert.match(view, /:placeholder="placeholderText\(r\)"/);
  assert.match(view, /v-if="runningNote\(r\)"/);
});
