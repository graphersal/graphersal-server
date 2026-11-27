// Node tests of the MCP profile editor's model (ui/mcp-profile-api.js, MCPPROF): the requests
// (with the revision as If-Match), the form of a profile and back (only what differs from the
// built-in profile is written; the KB-like --mcp-config file round-trips), the import of a
// configuration with its instructions file, the saved-query folders, and a round against a
// stateful mock of the profile routes (node:http on port 0, shaped after
// book/src/reference/admin_api.md "MCP profiles"); the editor's and the tab's templates.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync } from 'node:fs';
import {
  profileRequest, profilePath, createProfiles, builtinOf, formFromProfile, profileFromForm, isOverridden,
  defaultTools, importConfigFiles, folderChoices, selectedQueries, exportFileName, profileText, previewTools,
  clearQuestion, sourceBadge, sourceTitle, listOf, PROFILE_SOURCES, INSTRUCTIONS_MODES,
  previewBody, directoryBlockOf, previewDatabases,
} from '../mcp-profile-api.js';
import { mcpRows, defaultProfileOf } from '../mcp-api.js';
import { isConflict } from '../admin-api.js';

/** A built-in answer shaped like the server's `GET api/admin/mcp/builtin`. */
const BUILTIN = builtinOf({
  instructions: 'Graphersal built-in instructions.',
  directoryInstructions: 'Pick a graph first.',
  tools: [
    { name: 'query', title: 'Query', description: 'Run a script (writes too).', readDescription: 'Run a script.', writes: true, default: true },
    { name: 'gql', title: 'GQL', description: 'Run GQL.', readDescription: 'Run GQL.', writes: true, default: true },
    { name: 'get_schema', title: 'Schema', description: 'The schema.', readDescription: 'The schema.', writes: false, default: true },
    { name: 'run_saved_query', title: 'Run', description: 'Run a saved query.', readDescription: 'Run a saved query.', writes: true, default: true },
    { name: 'set_schema', title: 'Set schema', description: 'Replace the schema.', readDescription: null, writes: true, default: true },
    { name: 'usage_statistics', title: 'Usage', description: 'Usage.', readDescription: 'Usage.', writes: false, default: false },
  ],
  resources: [
    { name: 'schema', title: 'Schema', description: '', uri: 'graphersal://schema' },
    { name: 'saved-queries', title: 'Saved queries', description: '', uri: 'graphersal://saved-queries' },
    { name: 'gql-reference', title: 'GQL', description: '', uri: 'graphersal://gql-reference' },
  ],
  serverName: 'graphersal', serverTitle: 'Graphersal', answerLimitChars: 100000, minAnswerChars: 1000, maxInstructionsChars: 20000,
});

test('requests: builtin, the profile of a name or the server default, set/clear with the revision, preview', () => {
  assert.equal(profilePath(null), 'api/admin/mcp/profile');
  assert.equal(profilePath('a b'), 'api/admin/mcp/databases/a%20b/profile');
  assert.deepEqual(profileRequest.builtin(), { method: 'GET', path: 'api/admin/mcp/builtin' });
  assert.deepEqual(profileRequest.get('kb'), { method: 'GET', path: 'api/admin/mcp/databases/kb/profile' });
  assert.deepEqual(profileRequest.set(null, { a: 1 }, 'r1'), { method: 'PUT', path: 'api/admin/mcp/profile', body: { profile: { a: 1 } }, ifMatch: 'r1' });
  assert.deepEqual(profileRequest.clear('kb'), { method: 'DELETE', path: 'api/admin/mcp/databases/kb/profile' });
  assert.deepEqual(profileRequest.preview({ name: 'kb', level: 'read' }), { method: 'POST', path: 'api/admin/mcp/preview', body: { name: 'kb', level: 'read' } });
});

test('the built-in profile is the empty profile: nothing differs, nothing is written', () => {
  const form = formFromProfile(null, BUILTIN);
  assert.equal(form.instructionsMode, 'builtin');
  assert.equal(form.replaceText, BUILTIN.instructions, 'the replace text starts from the built-in instructions');
  assert.deepEqual(form.tools.filter((t) => t.on).map((t) => t.name), ['query', 'gql', 'get_schema', 'run_saved_query', 'set_schema']);
  assert.equal(form.tools.find((t) => t.name === 'gql').text, 'Run GQL.', 'a description starts from the built-in text');
  assert.ok(form.tools.find((t) => t.name === 'usage_statistics').optIn);
  assert.deepEqual(profileFromForm(form, BUILTIN), { profile: {}, problems: [] });
});

test('the knowledge-base --mcp-config file round-trips through the form', () => {
  const kb = {
    _comment: 'Profile of the live KB server',
    server_name: 'graphersal-kb',
    instructions: { mode: 'replace', text: 'graphersal-kb: the knowledge graph.' },
    descriptions: 'short',
    tools: { include: ['gql', 'query', 'get_schema', 'run_saved_query'] },
    saved_query_tools: { folders: ['kb', 'memory'], paging_params: false },
    resources: ['schema', 'saved-queries', 'gql-reference'],
  };
  const form = formFromProfile(kb, BUILTIN);
  assert.equal(form.serverName, 'graphersal-kb');
  assert.equal(form.instructionsMode, 'replace');
  assert.deepEqual(form.savedQueries.folders, ['kb', 'memory']);
  assert.equal(form.savedQueries.scope, 'some');
  assert.deepEqual(form.extra, { _comment: 'Profile of the live KB server' });
  const { profile, problems } = profileFromForm(form, BUILTIN);
  assert.deepEqual(problems, []);
  assert.deepEqual(profile, {
    _comment: 'Profile of the live KB server',
    server_name: 'graphersal-kb',
    instructions: { mode: 'replace', text: 'graphersal-kb: the knowledge graph.' },
    descriptions: 'short',
    tools: { include: ['query', 'gql', 'get_schema', 'run_saved_query'] },
    saved_query_tools: { folders: ['kb', 'memory'] },
  }, 'every resource listed: the key is left out; paging false is the default');
});

test('descriptions: an override only when it differs, never for a tool that is not offered; reset', () => {
  const form = formFromProfile({ tools: { overrides: { gql: { description: 'Ask in GQL.' } } } }, BUILTIN);
  const gql = form.tools.find((t) => t.name === 'gql');
  assert.equal(gql.text, 'Ask in GQL.');
  assert.ok(isOverridden(gql));
  const query = form.tools.find((t) => t.name === 'query');
  query.text = 'Run a script (writes too).  ';
  assert.ok(!isOverridden(query), 'only trailing blanks: the built-in text');
  form.tools.find((t) => t.name === 'set_schema').on = false;
  form.tools.find((t) => t.name === 'set_schema').text = 'changed but not offered';
  let { profile } = profileFromForm(form, BUILTIN);
  assert.deepEqual(profile.tools, {
    include: ['query', 'gql', 'get_schema', 'run_saved_query'],
    overrides: { gql: { description: 'Ask in GQL.' } },
  });
  gql.text = '';
  assert.match(profileFromForm(form, BUILTIN).problems[0], /gql: the description is empty/);
  gql.text = gql.builtin;
  form.tools.find((t) => t.name === 'set_schema').text = 'Replace the schema.';
  defaultTools(form);
  ({ profile } = profileFromForm(form, BUILTIN));
  assert.equal(profile.tools, undefined, 'the default selection with built-in texts writes no tools key');
  form.tools.find((t) => t.name === 'usage_statistics').on = true;
  assert.deepEqual(profileFromForm(form, BUILTIN).profile.tools.include.at(-1), 'usage_statistics');
  form.tools.forEach((t) => { t.on = false; });
  assert.ok(profileFromForm(form, BUILTIN).problems.some((p) => /no built-in tool/.test(p)));
});

test('instructions: append and replace need a text; the modes are explained', () => {
  assert.deepEqual(Object.keys(INSTRUCTIONS_MODES), ['builtin', 'append', 'replace']);
  const form = formFromProfile(null, BUILTIN);
  form.instructionsMode = 'append';
  assert.match(profileFromForm(form, BUILTIN).problems[0], /notes are empty/);
  form.appendText = '  Use the kb folder.  ';
  assert.deepEqual(profileFromForm(form, BUILTIN).profile.instructions, { mode: 'append', text: 'Use the kb folder.' });
  form.instructionsMode = 'replace';
  assert.deepEqual(profileFromForm(form, BUILTIN).profile.instructions, { mode: 'replace', text: BUILTIN.instructions });
  form.replaceText = 'x'.repeat(20001);
  assert.ok(profileFromForm(form, BUILTIN).problems.some((p) => /at most 20000/.test(p)));
});

test('saved queries as tools, resources, server name and the answer limit', () => {
  const form = formFromProfile(null, BUILTIN);
  form.savedQueries.on = true;
  assert.deepEqual(profileFromForm(form, BUILTIN).profile.saved_query_tools, {}, 'every saved query');
  form.savedQueries.scope = 'some';
  assert.match(profileFromForm(form, BUILTIN).problems[0], /pick a folder or name a query/);
  form.savedQueries.folders = ['kb'];
  form.savedQueries.names = 'one, two';
  form.savedQueries.exclude = 'two';
  form.savedQueries.paging = true;
  form.savedQueries.writeNote = 'custom';
  form.savedQueries.writeNoteText = 'Writes {effects}.';
  assert.deepEqual(profileFromForm(form, BUILTIN).profile.saved_query_tools, {
    folders: ['kb'], names: ['one', 'two'], exclude: ['two'], paging_params: true, write_note: 'Writes {effects}.',
  });
  form.resources[2].on = false;
  form.serverName = ' kb ';
  form.answerLimit = '999';
  let built = profileFromForm(form, BUILTIN);
  assert.deepEqual(built.profile.resources, ['schema', 'saved-queries']);
  assert.equal(built.profile.server_name, 'kb');
  assert.ok(built.problems.some((p) => /at least 1000/.test(p)));
  form.answerLimit = '20000';
  built = profileFromForm(form, BUILTIN);
  assert.equal(built.profile.answer_limit_chars, 20000);
  const back = formFromProfile(built.profile, BUILTIN);
  assert.equal(back.savedQueries.writeNote, 'custom');
  assert.equal(back.savedQueries.names, 'one, two');
  assert.deepEqual(listOf('a,\n b ,,c'), ['a', 'b', 'c']);
});

test('import: a configuration file with its instructions file inlined; what to choose otherwise', () => {
  const config = { name: 'mcp-config.json', text: JSON.stringify({ server_name: 'kb', instructions: { mode: 'replace', file: 'mcp-instructions.md' } }) };
  const notes = { name: 'mcp-instructions.md', text: 'Use kb.\n' };
  assert.deepEqual(importConfigFiles([config, notes]), { server_name: 'kb', instructions: { mode: 'replace', text: 'Use kb.\n' } });
  assert.throws(() => importConfigFiles([config]), /choose it together with the configuration/);
  assert.throws(() => importConfigFiles([notes]), /Choose the configuration file/);
  assert.throws(() => importConfigFiles([config, { ...config, name: 'b.json' }]), /Choose one configuration file/);
  assert.deepEqual(importConfigFiles([{ name: 'p.json', text: '{"tools": {"include": ["gql"]}}' }]), { tools: { include: ['gql'] } });
});

test('folders of the saved queries, the queries a selection makes tools of, texts', () => {
  const saved = [
    { name: 'recall', folder: 'kb' }, { name: 'outline', folder: 'kb/code' }, { name: 'memories', folder: 'memory' }, { name: 'loose', folder: null },
  ];
  assert.deepEqual(folderChoices(saved), [{ folder: 'kb', count: 2 }, { folder: 'kb/code', count: 1 }, { folder: 'memory', count: 1 }]);
  const sq = { on: true, scope: 'some', folders: ['kb'], names: 'loose', exclude: 'outline' };
  assert.deepEqual(selectedQueries(saved, sq), ['recall', 'loose']);
  assert.deepEqual(selectedQueries(saved, { ...sq, scope: 'all', exclude: '' }), ['recall', 'outline', 'memories', 'loose']);
  assert.deepEqual(selectedQueries(saved, { ...sq, on: false }), []);
  assert.equal(exportFileName(null), 'mcp-profile-server-default.json');
  assert.equal(exportFileName('kb'), 'mcp-profile-kb.json');
  assert.equal(profileText({ a: 1 }), '{\n  "a": 1\n}\n');
  assert.deepEqual(previewTools({ tools: [{ name: 'gql', description: 'd', annotations: { readOnlyHint: true } }, { x: 1 }] }), [{ name: 'gql', description: 'd', readOnly: true }]);
  assert.match(clearQuestion('cur', 'alias'), /database's profile/);
  assert.match(clearQuestion(null, 'default'), /built-in profile/);
  assert.deepEqual(Object.keys(PROFILE_SOURCES), ['own', 'database', 'default', 'builtin']);
  assert.equal(sourceBadge('builtin'), 'built-in');
  assert.match(sourceTitle('default'), /server default/);
});

test('the MCP state: each row names its profile source, and whether a default is set', () => {
  const rows = mcpRows({ databases: [{ name: 'kb', kind: 'database', profile: 'own' }, { name: 'x', kind: 'database', profile: 'weird' }] });
  assert.deepEqual(rows.map((r) => r.profile), ['own', 'builtin']);
  assert.equal(defaultProfileOf({ defaultProfile: true }), true);
  assert.equal(defaultProfileOf({}), false);
});

/** A stateful mock of the profile routes. */
async function mockServer() {
  const state = { profile: null, n: 0 };
  const revision = () => `rev${state.n}`;
  const view = () => ({ name: 'kb', kind: 'database', profile: state.profile, source: state.profile ? 'own' : 'builtin', effective: state.profile, readOnly: false, loaded: true, savedQueries: [], warnings: [], revision: revision() });
  const log = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const send = (status, value) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
      const url = new URL(req.url, 'http://x');
      log.push({ method: req.method, path: url.pathname, ifMatch: req.headers['if-match'] || '', body });
      if (url.pathname === '/api/admin/mcp/builtin') return send(200, { instructions: 'built-in', tools: [], resources: [] });
      if (url.pathname === '/api/admin/mcp/preview') return send(200, { instructions: 'Only kb.', tools: [{ name: 'gql', description: 'd' }], endpoint: '/db/kb/mcp' });
      if (url.pathname !== '/api/admin/mcp/databases/kb/profile') return send(404, { error: 'unknown' });
      if (req.method === 'GET') return send(200, view());
      const expected = (req.headers['if-match'] || '').replace(/"/g, '');
      if (expected && expected !== revision()) return send(409, { error: 'the MCP settings changed', help: 'changed by someone else: reload', revision: revision() });
      if (req.method === 'PUT') {
        const parsed = JSON.parse(body);
        if (parsed.profile.tools && parsed.profile.tools.inclde) return send(400, { error: 'the MCP profile is not valid: unknown key "tools.inclde" (did you mean "tools.include"?)' });
        state.profile = parsed.profile;
      } else {
        state.profile = null;
      }
      state.n += 1;
      return send(200, { ...view(), changed: true });
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = new URL(`http://127.0.0.1:${server.address().port}/`);
  return { server, base, log, state };
}

test('a round against the mock: read, save with the revision, a stale save refused, an invalid profile, clear, preview', async () => {
  const mock = await mockServer();
  try {
    const profiles = createProfiles({ base: mock.base, fetch, headers: {} });
    const builtin = builtinOf(await profiles.builtin());
    assert.equal(builtin.instructions, 'built-in');
    const view = await profiles.get('kb');
    assert.equal(view.source, 'builtin');
    const saved = await profiles.set('kb', { server_name: 'kb' }, view.revision);
    assert.equal(saved.source, 'own');
    assert.equal(mock.log.at(-1).ifMatch, `"${view.revision}"`);
    await assert.rejects(profiles.set('kb', { server_name: 'x' }, view.revision), (err) => isConflict(err));
    await assert.rejects(profiles.set('kb', { tools: { inclde: [] } }), /did you mean "tools.include"/);
    const cleared = await profiles.clear('kb', saved.revision);
    assert.equal(cleared.source, 'builtin');
    const preview = await profiles.preview({ name: 'kb', level: 'read', profile: {} });
    assert.equal(previewTools(preview)[0].name, 'gql');
    assert.equal(JSON.parse(mock.log.at(-1).body).level, 'read');
  } finally {
    mock.server.close();
  }
});

test('templates: the editor offers save, clear, previews, JSON, import and export; the tab opens it', () => {
  const editor = readFileSync(new URL('../mcp-profile-editor.js', import.meta.url), 'utf8');
  for (const marker of ['data-profile-save', 'data-profile-clear', 'data-profile-preview-read', 'data-profile-preview-write', 'data-profile-preview-server',
    'data-profile-json-edit', 'data-profile-json-apply', 'data-profile-import', 'data-profile-export', 'data-profile-tool-reset', 'data-profile-reset-instructions',
    'data-profile-folder', 'data-profile-sq-on', 'data-profile-resource', 'data-profile-answer-limit', '<form-error']) {
    assert.ok(editor.includes(marker), marker);
  }
  assert.ok(!/class="notice[^"]*" role="status"/.test(editor), 'outcomes are toasts');
  const tab = readFileSync(new URL('../mcp-tab.js', import.meta.url), 'utf8');
  for (const marker of ['data-mcp-default-profile-edit', 'data-mcp-profile-edit', 'data-mcp-profile-source', '<profile-editor', 'k="profile"']) {
    assert.ok(tab.includes(marker), marker);
  }
});

test('the /mcp block in previews: the body for a name or the server default with a database, the block, the names to choose from', () => {
  const profile = { tools: { include: ['gql'] } };
  assert.deepEqual(previewBody('kb', 'read', profile, 'ignored'), { name: 'kb', endpoint: 'database', level: 'read', profile });
  assert.deepEqual(previewBody(null, 'write', profile, ''), { endpoint: 'server', level: 'write', profile });
  assert.deepEqual(previewBody(null, 'read', profile, ' kb '), { endpoint: 'server', level: 'read', profile, name: 'kb' });
  assert.equal(directoryBlockOf({ directoryBlock: '--- Instructions for this database "kb" ---' }), '--- Instructions for this database "kb" ---');
  assert.equal(directoryBlockOf({ directoryBlock: null }), '');
  assert.equal(directoryBlockOf(null), '');
  assert.deepEqual(previewDatabases([
    { name: 'w', kind: 'database' }, { name: 'cur', kind: 'alias' }, { name: 'gone', kind: 'missing' }, { x: 1 },
  ]), ['cur', 'w']);
  const editor = readFileSync(new URL('../mcp-profile-editor.js', import.meta.url), 'utf8');
  for (const marker of ['data-profile-preview-db', 'data-profile-preview-level', 'data-profile-preview-block']) {
    assert.ok(editor.includes(marker), marker);
  }
  const tab = readFileSync(new URL('../mcp-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes(':databases="previewDatabases(rows)"'));
});
