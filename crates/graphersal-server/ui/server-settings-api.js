// The model of the server settings (no Vue, round 4 B-SETTINGS): the Settings tab's "Server"
// entry (every key of server.toml) and its "Server defaults" entry (what every database
// inherits or starts with: the query limits of [limits.query], the [defaults] of new
// databases and mcp.toml's expose_new). Requests of `GET/PUT api/admin/config` and `PUT
// api/admin/mcp {exposeNew}` (book/src/reference/admin_api.md "Server settings"), the rows of
// the view, the check of an input before it is sent (the server checks again with the same
// parser it starts with) and the texts. Imports only ./admin-api.js.

import { sendAdmin } from './admin-api.js';

const arr = (v) => (Array.isArray(v) ? v : []);
const str = (v) => (typeof v === 'string' ? v : '');

/** `request` based on `revision` (sent as `If-Match`): unchanged without one. */
const based = (request, revision) => (typeof revision === 'string' && revision ? { ...request, ifMatch: revision } : request);

/**
 * The requests: `view()` (every key of server.toml), `change(set, revision?)` (`set`: key ->
 * value or null = back to its default; a stale revision is refused, 409) and
 * `exposeNew(on, revision?)` (the MCP default of new names, kept in mcp.toml).
 */
export const configRequest = Object.freeze({
  view: () => ({ method: 'GET', path: 'api/admin/config' }),
  change: (set, revision) => based({ method: 'PUT', path: 'api/admin/config', body: { set } }, revision),
  exposeNew: (on, revision) => based({ method: 'PUT', path: 'api/admin/mcp', body: { exposeNew: !!on } }, revision),
});

/** A client over `sendAdmin`'s options: one method per request of `configRequest`. */
export function createConfig(options) {
  return {
    view: () => sendAdmin(options, configRequest.view()),
    change: (set, revision) => sendAdmin(options, configRequest.change(set, revision)),
    exposeNew: (on, revision) => sendAdmin(options, configRequest.exposeNew(on, revision)),
  };
}

/** The sections of server.toml in file order, with their titles. */
export const SECTIONS = Object.freeze([
  { id: '', title: 'Listener' },
  { id: 'mcp', title: 'MCP endpoint' },
  { id: 'defaults', title: 'New databases' },
  { id: 'limits', title: 'Limits' },
  { id: 'limits.query', title: 'Query limits of every database' },
  { id: 'sessions', title: 'Browser sessions' },
  { id: 'access', title: 'Access control' },
  { id: 'http', title: 'HTTP' },
  { id: 'jobs', title: 'Backup jobs' },
]);

/** When a change of each class applies, in words, and the badge's tone. */
export const APPLIES = Object.freeze({
  now: { text: 'applies now', tone: 'ok', title: 'The running server applies a change at once.' },
  load: { text: 'at next load', tone: '', title: "Read at every load of a database: a loaded database keeps its value until it is unloaded and loaded again." },
  create: { text: 'new databases', tone: '', title: 'Read when a database is created or imported; existing databases keep theirs.' },
  restart: { text: 'after restart', tone: 'warn', title: 'Saved now; the running server applies it at its next start.' },
});

/** The keys the "Server defaults" entry shows: what every database inherits or starts with. */
export const isDefaultsKey = (key) => key.startsWith('defaults.') || key.startsWith('limits.query.');

/** The keys applied when a database is created and fixed for its life afterwards. */
export const CREATION_KEYS = Object.freeze(['defaults.chunk_bytes', 'defaults.damage_policy']);

/** What a key's `null` value (not set) means. */
export function unsetText(key) {
  if (key.startsWith('limits.query.')) return 'not limited';
  if (key.startsWith('sessions.')) return 'no limit';
  if (key === 'limits.memory_limit') return "auto (the engine's default)";
  if (key === 'limits.memory_headroom') return 'a tenth of the cgroup limit';
  if (key === 'jobs.backup_root') return 'backups/jobs';
  return 'not set';
}

/** A value for people: `null` -> what not set means; a list -> names separated by commas. */
export function valueText(key, value) {
  if (value === null || value === undefined) return unsetText(key);
  if (Array.isArray(value)) return value.length ? value.join(', ') : 'none';
  if (typeof value === 'boolean') return value ? 'on' : 'off';
  return String(value);
}

/**
 * The rows of a view answer: `{key, name, section, value, default, set, kind, choices, applies,
 * appliesText, appliesTone, appliesTitle, running, pending, help, text, defaultText, creation}`
 * (`name`: the key without its section; `text`: the value in words, `(default)` marked when
 * the file does not set it). A malformed row is skipped.
 */
export function settingRows(answer = {}) {
  return arr(answer.keys)
    .filter((r) => r && typeof r.key === 'string')
    .map((r) => {
      const section = str(r.section);
      const applies = APPLIES[r.applies] ? r.applies : 'restart';
      const set = r.set === true;
      const value = r.value === undefined ? null : r.value;
      return {
        key: r.key,
        name: section ? r.key.slice(section.length + 1) : r.key,
        section,
        value,
        default: r.default === undefined ? null : r.default,
        set,
        kind: ['bool', 'count', 'text', 'memory', 'hosts'].includes(r.kind) ? r.kind : 'text',
        choices: Array.isArray(r.choices) ? r.choices.filter((c) => typeof c === 'string') : null,
        applies,
        appliesText: APPLIES[applies].text,
        appliesTone: APPLIES[applies].tone,
        appliesTitle: APPLIES[applies].title,
        running: r.running === undefined ? null : r.running,
        pending: r.pending === true,
        help: str(r.help),
        text: set ? valueText(r.key, value) : `${valueText(r.key, r.default === undefined ? null : r.default)} (default)`,
        defaultText: valueText(r.key, r.default === undefined ? null : r.default),
        creation: CREATION_KEYS.includes(r.key),
      };
    });
}

/**
 * The rows grouped by section in file order: `[{id, title, rows}]`, empty sections left out;
 * `only` filters the keys (the "Server defaults" entry: `isDefaultsKey`).
 */
export function settingSections(answer = {}, only = null) {
  const rows = settingRows(answer).filter((r) => (typeof only === 'function' ? only(r.key) : true));
  return SECTIONS.map((s) => ({ ...s, rows: rows.filter((r) => r.section === s.id) })).filter((s) => s.rows.length);
}

/** The text an edit field starts with: the file's value (a list: names separated by commas). */
export function inputText(row) {
  if (!row || row.value === null || row.value === undefined || !row.set) return '';
  return Array.isArray(row.value) ? row.value.join(', ') : String(row.value);
}

/**
 * The value to send for `text` typed into the row's field: `{value}` or `{problem}` (the
 * server checks the range again with the parser it starts with). A whole number for counts
 * (`_` allowed between digits, up to 2^53-1 on this page), a choice of `row.choices`, bytes or
 * auto/none for the memory limit, names separated by commas for host lists, any non-empty text
 * else. A bool is set with its checkbox (`{value: true|false}` directly).
 */
export function parseInput(row, text) {
  const t = String(text ?? '').trim();
  switch (row.kind) {
    case 'bool':
      if (/^(true|on|yes)$/i.test(t)) return { value: true };
      if (/^(false|off|no)$/i.test(t)) return { value: false };
      return { problem: `${row.key}: on or off` };
    case 'count': {
      const digits = t.replace(/_/g, '');
      if (!/^\d+$/.test(digits)) return { problem: `${row.key}: a whole number` };
      if (BigInt(digits) > BigInt(Number.MAX_SAFE_INTEGER)) return { problem: `${row.key}: this page sets values up to ${Number.MAX_SAFE_INTEGER}; set a larger one with graphersal-server config set ${row.key} VALUE --dir DIR` };
      return { value: Number(digits) };
    }
    case 'memory': {
      const digits = t.replace(/_/g, '');
      if (/^\d+$/.test(digits)) return { value: Number(digits) };
      if (/^(auto|none)$/i.test(t)) return { value: t.toLowerCase() };
      return { problem: `${row.key}: a number of bytes, auto or none` };
    }
    case 'hosts':
      return { value: t.split(',').map((s) => s.trim()).filter(Boolean) };
    default:
      if (!t) return { problem: `${row.key}: a value (Reset to default removes the key)` };
      if (row.choices && row.choices.length && !row.choices.includes(t)) return { problem: `${row.key}: one of ${row.choices.join(', ')}` };
      return { value: t };
  }
}

/** The `set` body of setting the row's key to `value` (null: back to its default). */
export const changeSet = (key, value) => ({ [key]: value === undefined ? null : value });

/** The keys waiting for a restart in an answer (`restartPending`). */
export const pendingKeys = (answer = {}) => arr(answer.restartPending).filter((k) => typeof k === 'string');

/** The restart-pending note of an answer: `''` when nothing waits. */
export function pendingText(answer = {}) {
  const keys = pendingKeys(answer);
  if (!keys.length) return '';
  return `Restart pending: ${keys.join(', ')}. Saved in server.toml; the running server applies ${keys.length === 1 ? 'it' : 'them'} at its next start (graphersal-server stop, then start).`;
}

/** The toast of a saved change. */
export function savedNotice(answer = {}) {
  const changed = arr(answer.changed);
  const applied = arr(answer.applied);
  const pending = pendingKeys(answer).filter((k) => changed.includes(k));
  if (!changed.length) return { tone: 'ok', text: 'server.toml unchanged: the value was set already.', help: '' };
  const parts = [];
  if (applied.length) parts.push(`Applied now: ${applied.join(', ')}.`);
  if (pending.length) parts.push(`Restart pending: ${pending.join(', ')}.`);
  const later = changed.filter((k) => !applied.includes(k) && !pending.includes(k));
  if (later.length) parts.push(`Applies when used: ${later.join(', ')} (query limits at each database's next load, [defaults] for new databases).`);
  if (answer.rewritten === true) parts.push('server.toml was rewritten from its keys: its own comments could not be kept.');
  return { tone: pending.length || answer.rewritten === true ? 'warn' : 'ok', text: `Saved ${changed.join(', ')}.`, help: parts.join(' ') };
}

/** What the "Server defaults" entry says about inheritance. */
export const DEFAULTS_HELP = 'Every database inherits the query limits below, key by key, unless it overrides a key in its own settings (Reset to inherited there removes the override); a change applies at each database\'s next load. The new-database values are copied into a database when it is created or imported: changing them never touches an existing database. The creation parameters (chunk size, damage policy) are fixed for a database\'s life.';

/** What the "Server" entry says about saving. */
export const SERVER_HELP = 'Every key of server.toml. Saving writes the file at once (its comments are kept; an unusual layout is rewritten, which the notice says). "applies now" keys take effect at once; query limits at each database\'s next load; [defaults] for new databases; the rest after a restart, marked "restart pending" until then. A change made from a stale page is refused: the page reads the file again.';

/**
 * What a new database gets from the server defaults, in words, from a database settings view's
 * `serverDefaults` (`part`: 'flags' = auto-load and read-only, 'store' = the creation
 * parameters); `''` when the view has none.
 */
export function newDatabaseText(defaults, part = 'flags') {
  const d = defaults && typeof defaults === 'object' && !defaults.error ? defaults : null;
  if (!d) return '';
  const onOff = (v) => (v === true ? 'on' : 'off');
  if (part === 'store') {
    const chunk = typeof d.chunkBytes === 'number' ? `${d.chunkBytes} bytes` : 'the default';
    return `A new database gets the server defaults: chunk size ${chunk}, damage policy ${str(d.damagePolicy) || 'maintenance'}.`;
  }
  return `A new database starts with the server defaults: auto-load ${onOff(d.autoLoad)}, read-only ${onOff(d.readOnly)}, ${d.mcpExposeNew === true ? 'offered' : 'not offered'} to MCP.`;
}
