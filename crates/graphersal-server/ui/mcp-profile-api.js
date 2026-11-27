// The MCP profile editor's model (no Vue; MCPPROF): the requests of the admin API's profile
// routes (`GET api/admin/mcp/builtin`, `GET|PUT|DELETE api/admin/mcp/profile` and
// `api/admin/mcp/databases/<name>/profile`, `POST api/admin/mcp/preview`;
// book/src/reference/admin_api.md "MCP profiles"), the form a profile is edited in and back
// (`formFromProfile` / `profileFromForm`: the JSON of graphersal's `--mcp-config` file, the
// instructions inline), the import of a configuration file with its instructions file
// (`importConfigFiles`) and the texts. Every built-in text (instructions, tool descriptions) comes
// from the server's `builtin` answer, so a field starts from the built-in text and an
// unchanged one is not written. Imports only ./admin-api.js.

import { sendAdmin } from './admin-api.js';

const seg = (name) => encodeURIComponent(String(name));
const based = (request, revision) => (typeof revision === 'string' && revision ? { ...request, ifMatch: revision } : request);
const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const strings = (v) => (Array.isArray(v) ? v.filter((s) => typeof s === 'string') : []);
const str = (v) => (typeof v === 'string' ? v : '');

/** The admin path of a profile: the server default (`null`) or a name's. */
export function profilePath(name) {
  return name ? `api/admin/mcp/databases/${seg(name)}/profile` : 'api/admin/mcp/profile';
}

/**
 * The requests: `builtin()` (the built-in texts), `get(name)` (the profile view; `null` = the
 * server default), `set(name, profile, revision?)`, `clear(name, revision?)` and
 * `preview(body)` (`{profile?, name?, endpoint?, level?}`: what an agent sees, nothing saved).
 */
export const profileRequest = Object.freeze({
  builtin: () => ({ method: 'GET', path: 'api/admin/mcp/builtin' }),
  get: (name) => ({ method: 'GET', path: profilePath(name) }),
  set: (name, profile, revision) => based({ method: 'PUT', path: profilePath(name), body: { profile } }, revision),
  clear: (name, revision) => based({ method: 'DELETE', path: profilePath(name) }, revision),
  preview: (body) => ({ method: 'POST', path: 'api/admin/mcp/preview', body }),
});

/** A profile client over `sendAdmin`'s options: one method per request of `profileRequest`. */
export function createProfiles(options) {
  return {
    builtin: () => sendAdmin(options, profileRequest.builtin()),
    get: (name) => sendAdmin(options, profileRequest.get(name)),
    set: (name, profile, revision) => sendAdmin(options, profileRequest.set(name, profile, revision)),
    clear: (name, revision) => sendAdmin(options, profileRequest.clear(name, revision)),
    preview: (body) => sendAdmin(options, profileRequest.preview(body)),
  };
}

/** The keys of a profile the form edits; every other key (a `_comment`, ...) is kept as it is. */
export const PROFILE_KEYS = Object.freeze([
  'server_name', 'server_title', 'instructions', 'descriptions', 'tools', 'saved_query_tools', 'resources', 'answer_limit_chars',
]);

/** The instructions modes and what each means. */
export const INSTRUCTIONS_MODES = Object.freeze({
  builtin: 'Built-in: the agent gets Graphersal\'s own instructions.',
  append: 'Append: the built-in instructions, then your notes (under "Notes from the operator of this server about this graph").',
  replace: 'Replace: only your text (start from the built-in text below and change it).',
});

/** Where the profile that applies comes from (`source` of a profile view or an MCP state row). */
export const PROFILE_SOURCES = Object.freeze({
  own: 'its own profile',
  database: "its database's profile",
  default: 'the server default profile',
  builtin: 'the built-in profile',
});

/** The short badge of a source. */
export const sourceBadge = (source) => ({ own: 'own', database: 'database', default: 'default' }[source] || 'built-in');

/** The tooltip of a source. */
export function sourceTitle(source) {
  switch (source) {
    case 'own': return 'This name has its own MCP profile.';
    case 'database': return 'An alias without a profile of its own: its database\'s profile applies.';
    case 'default': return 'No profile of its own: the server default profile applies.';
    default: return 'No profile: every tool with its built-in description and the built-in instructions.';
  }
}

/** The built-in texts of the server's `builtin` answer, defensively shaped. */
export function builtinOf(answer = {}) {
  const a = isObject(answer) ? answer : {};
  return {
    instructions: str(a.instructions),
    directoryInstructions: str(a.directoryInstructions),
    tools: (Array.isArray(a.tools) ? a.tools : []).filter((t) => isObject(t) && typeof t.name === 'string').map((t) => ({
      name: t.name,
      title: str(t.title),
      description: str(t.description),
      readDescription: typeof t.readDescription === 'string' ? t.readDescription : null,
      writes: t.writes === true,
      default: t.default !== false,
    })),
    resources: (Array.isArray(a.resources) ? a.resources : []).filter((r) => isObject(r) && typeof r.name === 'string').map((r) => ({
      name: r.name, title: str(r.title), description: str(r.description), uri: str(r.uri),
    })),
    serverName: str(a.serverName),
    serverTitle: str(a.serverTitle),
    answerLimitChars: typeof a.answerLimitChars === 'number' ? a.answerLimitChars : 0,
    minAnswerChars: typeof a.minAnswerChars === 'number' ? a.minAnswerChars : 1000,
    maxInstructionsChars: typeof a.maxInstructionsChars === 'number' ? a.maxInstructionsChars : 20000,
  };
}

/** A comma- or line-separated list typed into a field, trimmed, without empty entries. */
export const listOf = (text) => String(text || '').split(/[,\n]/).map((s) => s.trim()).filter(Boolean);

/**
 * The form of `profile` (`null`: the built-in profile) over the built-in texts `builtin`
 * (`builtinOf`): every built-in tool with its checkbox (offered or not) and its description
 * (the profile's override, else the built-in text), the instructions mode with a text per mode
 * (the replace text starts from the built-in instructions), saved queries as tools, the
 * resources, the server name and title, the answer limit, and the keys the form does not edit
 * (`extra`).
 */
export function formFromProfile(profile, builtin) {
  const p = isObject(profile) ? profile : {};
  const b = builtin;
  const tools = isObject(p.tools) ? p.tools : {};
  const include = Array.isArray(tools.include) ? strings(tools.include) : null;
  const exclude = strings(tools.exclude);
  const overrides = isObject(tools.overrides) ? tools.overrides : {};
  const instructions = isObject(p.instructions) ? p.instructions : {};
  const mode = ['append', 'replace'].includes(instructions.mode) ? instructions.mode : 'builtin';
  const text = str(instructions.text);
  const sq = isObject(p.saved_query_tools) ? p.saved_query_tools : null;
  const note = sq ? str(sq.write_note) : '';
  const extra = {};
  for (const [key, value] of Object.entries(p)) {
    if (!PROFILE_KEYS.includes(key)) extra[key] = value;
  }
  const resources = Array.isArray(p.resources) ? strings(p.resources) : null;
  return {
    serverName: str(p.server_name),
    serverTitle: str(p.server_title),
    instructionsMode: mode,
    appendText: mode === 'append' ? text : '',
    replaceText: mode === 'replace' ? text : b.instructions,
    descriptions: typeof p.descriptions === 'string' ? p.descriptions : '',
    tools: b.tools.map((t) => {
      const override = isObject(overrides[t.name]) ? str(overrides[t.name].description) : '';
      return {
        name: t.name,
        title: t.title,
        builtin: t.description,
        readers: t.readDescription !== null,
        writes: t.writes,
        optIn: !t.default,
        on: (include ? include.includes(t.name) : t.default) && !exclude.includes(t.name),
        text: override || t.description,
      };
    }),
    savedQueries: {
      on: !!sq,
      scope: sq && (Array.isArray(sq.folders) || Array.isArray(sq.names)) ? 'some' : 'all',
      folders: sq ? strings(sq.folders) : [],
      names: sq ? strings(sq.names).join(', ') : '',
      exclude: sq ? strings(sq.exclude).join(', ') : '',
      paging: !!(sq && sq.paging_params === true),
      writeNote: note === 'none' ? 'none' : note && note !== 'default' ? 'custom' : 'default',
      writeNoteText: note && !['none', 'default'].includes(note) ? note : '',
    },
    resources: b.resources.map((r) => ({ name: r.name, title: r.title, on: resources ? resources.includes(r.name) : true })),
    answerLimit: typeof p.answer_limit_chars === 'number' ? String(p.answer_limit_chars) : '',
    extra,
  };
}

/** Whether a tool's description in the form differs from its built-in text (an override). */
export const isOverridden = (tool) => tool.text.trim() !== tool.builtin.trim();

/**
 * The profile of the form `form` (`formFromProfile`): `{profile, problems}`. Only what differs
 * from the built-in profile is written: `tools.include` when the offered tools are not the
 * default ones, an override only for a description that differs from its built-in text (and
 * only for an offered tool: graphersal-mcp refuses others), `resources` when not all are listed,
 * `instructions` unless built-in. `problems`: what the server would refuse anyway, said in the
 * form's words (an empty instructions text, an empty description, an answer limit below the
 * minimum, a saved-query selection naming nothing).
 */
export function profileFromForm(form, builtin) {
  const problems = [];
  const out = { ...(isObject(form.extra) ? form.extra : {}) };
  if (form.serverName.trim()) out.server_name = form.serverName.trim();
  if (form.serverTitle.trim()) out.server_title = form.serverTitle.trim();
  if (form.instructionsMode === 'append' || form.instructionsMode === 'replace') {
    const text = (form.instructionsMode === 'append' ? form.appendText : form.replaceText).trim();
    if (!text) problems.push(`Instructions: the ${form.instructionsMode === 'append' ? 'notes are' : 'text is'} empty (or choose "Built-in").`);
    if (text.length > builtin.maxInstructionsChars) problems.push(`Instructions: ${text.length} characters, at most ${builtin.maxInstructionsChars}.`);
    out.instructions = { mode: form.instructionsMode, text };
  }
  if (form.descriptions) out.descriptions = form.descriptions;
  const on = form.tools.filter((t) => t.on).map((t) => t.name);
  const defaults = builtin.tools.filter((t) => t.default).map((t) => t.name);
  const tools = {};
  if (on.length !== defaults.length || on.some((n) => !defaults.includes(n))) tools.include = on;
  const overrides = {};
  for (const t of form.tools) {
    if (!t.on) continue;
    if (!t.text.trim()) problems.push(`Tool ${t.name}: the description is empty (Reset gives the built-in one back).`);
    else if (isOverridden(t)) overrides[t.name] = { description: t.text.trim() };
  }
  if (Object.keys(overrides).length) tools.overrides = overrides;
  if (Object.keys(tools).length) out.tools = tools;
  if (!on.length) problems.push('Tools: no built-in tool is offered (an agent would have nothing but saved-query tools).');
  const sq = form.savedQueries;
  if (sq.on) {
    const s = {};
    if (sq.scope === 'some') {
      const names = listOf(sq.names);
      if (sq.folders.length) s.folders = [...sq.folders];
      if (names.length) s.names = names;
      if (!sq.folders.length && !names.length) problems.push('Saved queries as tools: pick a folder or name a query (or choose "every saved query").');
    }
    const exclude = listOf(sq.exclude);
    if (exclude.length) s.exclude = exclude;
    if (sq.paging) s.paging_params = true;
    if (sq.writeNote === 'none') s.write_note = 'none';
    if (sq.writeNote === 'custom') {
      if (!sq.writeNoteText.trim()) problems.push('Saved queries as tools: the note of writing queries is empty.');
      s.write_note = sq.writeNoteText.trim();
    }
    out.saved_query_tools = s;
  }
  const listed = form.resources.filter((r) => r.on).map((r) => r.name);
  if (listed.length !== builtin.resources.length) out.resources = listed;
  const limit = String(form.answerLimit || '').trim();
  if (limit) {
    const n = Number(limit);
    if (!Number.isInteger(n) || n < builtin.minAnswerChars) problems.push(`Answer limit: a whole number of characters, at least ${builtin.minAnswerChars}.`);
    else out.answer_limit_chars = n;
  }
  return { profile: out, problems };
}

/** Puts every offered/not offered tool of the form to its built-in default. */
export function defaultTools(form) {
  for (const t of form.tools) t.on = !t.optIn;
}

/**
 * The profile of a graphersal `--mcp-config` file chosen in the browser, `files` being `[{name,
 * text}]`: the one JSON file, with the text of the instructions `file` it names put into
 * `instructions.text` when that file is among `files` (matched by its file name). Throws an
 * Error saying what to choose otherwise.
 */
export function importConfigFiles(files) {
  const list = Array.isArray(files) ? files : [];
  const configs = [];
  for (const f of list) {
    try {
      const value = JSON.parse(f.text);
      if (isObject(value)) configs.push({ name: f.name, value });
    } catch {
      // Not JSON: an instructions file, maybe.
    }
  }
  if (!configs.length) throw new Error('Choose the configuration file (JSON), e.g. mcp-config.json.');
  if (configs.length > 1) throw new Error(`Choose one configuration file, not ${configs.length} (${configs.map((c) => c.name).join(', ')}).`);
  const profile = configs[0].value;
  const instructions = isObject(profile.instructions) ? profile.instructions : null;
  if (instructions && typeof instructions.file === 'string') {
    const wanted = instructions.file.split(/[\\/]/).pop();
    const found = list.find((f) => f.name === wanted && f.name !== configs[0].name);
    if (!found) {
      throw new Error(`The configuration names the instructions file "${instructions.file}": choose it together with the configuration (both files at once).`);
    }
    const { file, ...rest } = instructions;
    profile.instructions = { ...rest, text: found.text };
  }
  return profile;
}

/** The folders of a loaded database's saved queries (each folder and its parents), sorted, with how many queries each holds (subfolders included). */
export function folderChoices(savedQueries) {
  const counts = new Map();
  for (const q of Array.isArray(savedQueries) ? savedQueries : []) {
    const folder = q && typeof q.folder === 'string' ? q.folder : '';
    if (!folder) continue;
    const parts = folder.split('/');
    for (let i = 1; i <= parts.length; i += 1) {
      const path = parts.slice(0, i).join('/');
      counts.set(path, (counts.get(path) || 0) + 1);
    }
  }
  return [...counts.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([folder, count]) => ({ folder, count }));
}

/** The saved queries a selection makes tools of (folders with subfolders, names, minus exclude), for the form's hint. */
export function selectedQueries(savedQueries, sq) {
  const all = Array.isArray(savedQueries) ? savedQueries.filter((q) => q && typeof q.name === 'string') : [];
  if (!sq.on) return [];
  const exclude = listOf(sq.exclude);
  const names = listOf(sq.names);
  const inFolder = (q) => sq.folders.some((f) => q.folder === f || (typeof q.folder === 'string' && q.folder.startsWith(`${f}/`)));
  return all.filter((q) => (sq.scope === 'all' || inFolder(q) || names.includes(q.name)) && !exclude.includes(q.name)).map((q) => q.name);
}

/** The file name of an exported profile. */
export const exportFileName = (name) => `mcp-profile-${name || 'server-default'}.json`;

/** The JSON text of a profile as exported (and shown in the JSON view). */
export const profileText = (profile) => `${JSON.stringify(profile, null, 2)}\n`;

/** The tools of a preview answer: `[{name, description, readOnly}]`. */
export function previewTools(answer = {}) {
  return (Array.isArray(answer.tools) ? answer.tools : []).filter((t) => isObject(t) && typeof t.name === 'string').map((t) => ({
    name: t.name,
    description: str(t.description),
    readOnly: !!(t.annotations && t.annotations.readOnlyHint === true),
  }));
}

/**
 * The body of a preview of the profile in the form (`profile`): of the name `name` on its own
 * endpoint (`/db/<name>/mcp`) at `level`, or (`name` null) of the server default on `/mcp`, with
 * `database` (a name, or empty) chosen for the block "Instructions for this database" that
 * `/mcp` gives at `level` after use_graph (MCPDB, answered as `directoryBlock`).
 */
export function previewBody(name, level, profile, database = '') {
  if (name) return { name, endpoint: 'database', level, profile };
  const body = { endpoint: 'server', level, profile };
  if (typeof database === 'string' && database.trim()) body.name = database.trim();
  return body;
}

/** The block "Instructions for this database" of a preview answer (`''`: none). */
export const directoryBlockOf = (answer = {}) => str(isObject(answer) ? answer.directoryBlock : '');

/** The names a server-default preview may choose a database from: the databases and aliases of the MCP state's rows. */
export function previewDatabases(rows) {
  return (Array.isArray(rows) ? rows : [])
    .filter((r) => isObject(r) && typeof r.name === 'string' && (r.kind === 'database' || r.kind === 'alias'))
    .map((r) => r.name)
    .sort((a, b) => a.localeCompare(b));
}

/** The question before a profile is cleared. */
export function clearQuestion(name, kind) {
  const next = kind === 'alias' ? "its database's profile, else the server default, else the built-in one" : name ? 'the server default profile, else the built-in one' : 'the built-in profile';
  return `Clear the MCP profile of ${name ? `'${name}'` : 'the server'}?\n\nIts agents get ${next} from their next request on.`;
}

/** When a saved profile reaches agents (shown under the Save button). */
export const APPLIES_TEXT = 'A saved profile applies from the next request of every agent; an agent that keeps the tool list of its session sees new tools after it reconnects.';
