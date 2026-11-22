// Node tests of the `server` UI profile (ui/profile.json): its shape, its tab modules on disk,
// the rule that every module imports only `vue` and the profile's own files (the playground's
// files are not part of the profile's contract), and the engine's own check of the profile:
// the playground's `validateProfile` (js/ui-profiles.js) and its check of the root info
// (`isEngineInfo`, js/engine/http-engine.js) from GRAPHERSAL_PLAYGROUND, else from the
// graphersal checkout beside this repository (`../graphersal/playground`, the path
// dependency's), skipped when neither exists.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const UI = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const profile = JSON.parse(readFileSync(join(UI, 'profile.json'), 'utf8'));
const PLAYGROUND = process.env.GRAPHERSAL_PLAYGROUND || resolve(UI, '..', '..', '..', '..', 'graphersal', 'playground');

/** The playground module `rel` (below PLAYGROUND), or null (the test skipped) when absent. */
async function playgroundModule(t, rel) {
  const file = join(PLAYGROUND, ...rel.split('/'));
  if (!existsSync(file)) {
    t.skip(`no playground at ${PLAYGROUND} (set GRAPHERSAL_PLAYGROUND)`);
    return null;
  }
  return import(pathToFileURL(file).href);
}

test('the profile: name, files, tabs of its own, no workspace', () => {
  assert.equal(profile.name, 'server');
  assert.equal(profile.files, 'server-ui/', 'relative to the page at the server root');
  assert.deepEqual(profile.tabs.map((t) => t.id), ['databases', 'backups', 'import', 'export', 'models']);
  assert.equal(profile.startTab, 'databases');
  for (const tab of profile.tabs) {
    assert.equal(tab.builtin, undefined, `${tab.id}: no built-in workspace (no graph at the root)`);
    assert.ok(existsSync(join(UI, tab.module)), `${tab.module} exists`);
  }
  assert.equal(profile.version, undefined, 'the binary adds its version');
});

test('every style sheet the profile names (styles) exists below its files', () => {
  assert.deepEqual(profile.styles, ['server-ui.css']);
  for (const style of profile.styles) {
    assert.match(style, /^[a-z0-9-]+\.css$/, style);
    assert.ok(existsSync(join(UI, style)), `${style} exists`);
  }
});

test('every module imports only vue and the profile\'s own files', () => {
  const modules = readdirSync(UI).filter((f) => f.endsWith('.js'));
  assert.ok(modules.length >= 8);
  for (const file of modules) {
    const text = readFileSync(join(UI, file), 'utf8');
    const specifiers = [...text.matchAll(/^\s*import\s[^'"]*['"]([^'"]+)['"]/gm)].map((m) => m[1]);
    for (const spec of specifiers) {
      if (spec === 'vue') continue;
      assert.match(spec, /^\.\/[a-z0-9-]+\.js$/, `${file} imports ${spec}`);
      assert.ok(existsSync(join(UI, spec)), `${file}: ${spec} exists`);
    }
  }
});

test('the engine\'s playground accepts the profile (validateProfile), with a version', async (t) => {
  const mod = await playgroundModule(t, 'js/ui-profiles.js');
  if (!mod) return;
  const { validateProfile, createUi } = mod;
  const valid = validateProfile({ ...profile, version: '0.1.0' });
  assert.equal(valid.name, 'server');
  assert.equal(valid.version, '0.1.0');
  const ui = createUi(valid, { server: true });
  assert.equal(ui.hasWorkspace, false, 'the root has no graph: never the workspace');
  assert.equal(ui.can('theme'), true);
  for (const key of ['mcp', 'devBadge', 'settings', 'help']) assert.equal(ui.can(key), false, key);
  assert.deepEqual(valid.styles, ['server-ui.css']);
  assert.deepEqual(ui.styleUrls('http://h/'), ['http://h/server-ui/server-ui.css'], 'loaded from files, relative to the root');
});

test('the engine\'s playground takes the root info as its engine\'s (isEngineInfo)', async (t) => {
  const mod = await playgroundModule(t, 'js/engine/http-engine.js');
  if (!mod) return;
  const { isEngineInfo } = mod;
  // The root /api/info as src/router.rs `info` answers it: root-info.json holds its fixed
  // fields (the Rust test `the_node_tests_root_info_is_the_answer` checks them against the
  // real answer), the profile is added as the binary adds it.
  const root = JSON.parse(readFileSync(join(UI, 'tests', 'root-info.json'), 'utf8'));
  assert.equal(root.protocol, 'graphersal/1');
  const uiProfile = { ...profile, version: '0.1.0' };
  assert.equal(isEngineInfo({ ...root, uiProfile }), true);
  assert.equal(isEngineInfo({ ...root, uploadBytes: 1024, mcp: null, uiProfile }), true);
  // The playground recognises a Graphersal host only by its protocol marker: a named host
  // without it is not taken, even with a profile.
  const { protocol, ...unmarked } = root;
  assert.equal(isEngineInfo({ ...unmarked, uiProfile }), false, 'no protocol: not a Graphersal host');
  assert.equal(isEngineInfo({ server: 'graphersal-server', uiProfile }), false, 'a name and a profile are not enough');
});
