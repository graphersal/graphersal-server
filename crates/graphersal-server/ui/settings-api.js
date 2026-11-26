// The Database settings tab's model (no Vue, round 3 B7): the requests of the admin API's
// settings route (`GET/PUT api/admin/databases/<name>/settings`; docs/admin-api.md "Database
// settings"), the rules of an edit (the same refusals and texts as the server's
// src/settings.rs), and the rows of the tab's sections: identity, state and sizes, query
// limits (the database's own override, the server's default, what the next load uses, what
// the loaded session runs under now), MCP, grants, backups and the Store's creation
// parameters (read-only). Imports only ./admin-api.js.

import { sendAdmin, bytesText } from './admin-api.js';

const seg = (name) => encodeURIComponent(String(name));
const str = (v) => (typeof v === 'string' ? v : '');
const arr = (v) => (Array.isArray(v) ? v : []);
const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/** `request` based on `revision` (sent as `If-Match`): unchanged without one. */
const based = (request, revision) => (typeof revision === 'string' && revision ? { ...request, ifMatch: revision } : request);

/**
 * The requests: `view(name)` (everything about the database or the database an alias names)
 * and `change(name, body, revision?)` (description, readOnly, autoLoad and/or limits of the
 * database itself; the view's `revision`: a change someone else made meanwhile refuses it, 409).
 */
export const settingsRequest = Object.freeze({
  view: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}/settings` }),
  change: (name, body, revision) => based({ method: 'PUT', path: `api/admin/databases/${seg(name)}/settings`, body }, revision),
});

/** A settings client over `sendAdmin`'s options: one method per request of `settingsRequest`. */
export function createSettings(options) {
  return {
    view: (name) => sendAdmin(options, settingsRequest.view(name)),
    change: (name, body, revision) => sendAdmin(options, settingsRequest.change(name, body, revision)),
  };
}

/**
 * The keys of `[limits.query]` in file order (src/limits.rs `QUERY_LIMIT_KEYS`) with their
 * meaning; `zero` is what 0 means.
 */
export const LIMITS = Object.freeze([
  { key: 'timeout_ms', label: 'Timeout', unit: 'ms', zero: 'no limit', help: 'The evaluationTimeout of every traversal and the budget of a whole run, in milliseconds.' },
  { key: 'max_operations', label: 'Script operations', unit: '', zero: 'no limit', help: 'Rhai operations one script may run (a loop {} stops here).' },
  { key: 'max_string_size', label: 'String size', unit: 'bytes', zero: 'no limit', help: 'The longest string a script or a step may build, in bytes.' },
  { key: 'max_array_size', label: 'Array size', unit: 'items', zero: 'no limit', help: 'The largest array a script may build, in items.' },
  { key: 'max_map_size', label: 'Map size', unit: 'entries', zero: 'no limit', help: 'The largest map a script may build, in entries.' },
  { key: 'max_traversers', label: 'Traversers', unit: '', zero: 'no limit', help: 'Traversers one step may leave alive (engine default 10M when no limit is set anywhere).' },
  { key: 'max_materialized_bytes', label: 'Materialized memory', unit: 'bytes', zero: 'no limit', help: 'The memory one query holds at once, in bytes (engine default 1 GiB when no limit is set anywhere).' },
  { key: 'max_value_depth', label: 'Value depth', unit: 'levels', zero: 'the default 128', help: 'The deepest nesting of arrays and maps in one value; never unlimited.' },
]);

/** The keys of `LIMITS`. */
export const LIMIT_KEYS = Object.freeze(LIMITS.map((l) => l.key));

/** The largest limit value the server takes (2^63 - 1, src/limits.rs `MAX_LIMIT_VALUE`). */
export const MAX_LIMIT_TEXT = '9223372036854775807';

/** The largest value this page sends (a JSON number keeps integers exactly only up to here):
 * set a larger one with the console (`db set NAME limit KEY VALUE`). */
export const MAX_UI_LIMIT = Number.MAX_SAFE_INTEGER;

/** When a limit change applies (src/settings.rs `LIMITS_APPLY_TEXT`; the server's answer carries it too). */
export const LIMITS_APPLY = "A query limit change applies at the database's next load: unload and load it (or restart the server); a loaded database keeps the limits it was loaded with until then.";

/** The longest description (src/settings.rs `MAX_DESCRIPTION_CHARS`). */
export const MAX_DESCRIPTION_CHARS = 1000;

/** The server's refusal of a bad limit value (src/settings.rs `limit_value_problem`). */
export function limitValueReason(key) {
  const zero = key === 'max_value_depth' ? "0 = the engine's default of 128" : '0 = no limit';
  return `limits.${key} must be a whole number from 0 to ${MAX_LIMIT_TEXT} (${zero}), or null to clear the override`;
}

/**
 * The problem with `text` as the value of the limit `key`, or null: a whole number (`_` allowed
 * between digits, as in TOML), at most `MAX_UI_LIMIT` on this page.
 */
export function limitValueProblem(key, text) {
  if (!LIMIT_KEYS.includes(key)) return `unknown query limit '${key}': the keys are ${LIMIT_KEYS.join(', ')}`;
  const digits = String(text ?? '').trim().replace(/_/g, '');
  if (!/^\d+$/.test(digits)) return limitValueReason(key);
  if (BigInt(digits) > BigInt(MAX_LIMIT_TEXT)) return limitValueReason(key);
  if (BigInt(digits) > BigInt(MAX_UI_LIMIT)) return `This page sets values up to ${MAX_UI_LIMIT}; set a larger one with the console: graphersal-server db set NAME limit ${key} VALUE --dir DIR`;
  return null;
}

/** The body of setting the limit `key` to `text` (checked first: `limitValueProblem`). */
export function limitSetBody(key, text) {
  return { limits: { [key]: Number(String(text).trim().replace(/_/g, '')) } };
}

/** The body of clearing the override of the limit `key` (the server's default applies again). */
export const limitClearBody = (key) => ({ limits: { [key]: null } });

/** The problem with `text` as a description, or null (the server's texts). */
export function descriptionProblem(text) {
  const t = String(text ?? '');
  if ([...t].length > MAX_DESCRIPTION_CHARS) return `a description has at most ${MAX_DESCRIPTION_CHARS} characters`;
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(t)) return 'a description is one line of text: no line breaks, tabs or control characters';
  return null;
}

/** A limit value for people: `5000`, `0 (no limit)`, `—` (not set). */
export function limitText(key, value) {
  if (value === null || value === undefined) return '—';
  if (value === 0) return `0 (${(LIMITS.find((l) => l.key === key) || {}).zero || 'no limit'})`;
  return String(value);
}

/** Where a limit comes from. */
export const SOURCE_TEXT = Object.freeze({
  database: 'this database',
  server: 'server default',
  none: 'not limited',
});

/**
 * The limit rows of a settings answer: `{key, label, unit, help, override, server, effective,
 * source, sourceText, inEffect, loaded, differs}` (`differs`: loaded and the next load would use
 * another value than the running session).
 */
export function limitRows(answer = {}) {
  const limits = answer.limits || {};
  const loaded = limits.loaded === true;
  const byKey = new Map(arr(limits.keys).filter((r) => r && typeof r.key === 'string').map((r) => [r.key, r]));
  return LIMITS.map((meta) => {
    const r = byKey.get(meta.key) || {};
    const source = ['database', 'server', 'none'].includes(r.source) ? r.source : 'none';
    const override = num(r.override);
    const server = num(r.server);
    const effective = num(r.effective);
    const inEffect = loaded ? num(r.inEffect) : null;
    return {
      ...meta,
      override,
      server,
      effective,
      source,
      sourceText: SOURCE_TEXT[source],
      inEffect,
      loaded,
      differs: loaded && inEffect !== effective,
    };
  });
}

/** The note under the limits: when a change applies, and whether one waits for the next load. */
export function limitsNote(answer = {}) {
  const limits = answer.limits || {};
  if (limits.reloadNeeded === true) {
    return { tone: 'warn', text: 'The limits changed since the database was loaded: they apply at its next load.', help: LIMITS_APPLY };
  }
  return { tone: 'ok', text: LIMITS_APPLY, help: '' };
}

/** `[label, value, title?]` rows of the identity section. */
export function identityRows(answer = {}) {
  const db = answer.database || {};
  const kind = db.role === 'base' ? 'base (frozen, read-only)' : db.kind === 'layer' ? `layer over ${str(db.base)}` : str(db.kind) || '—';
  const rows = [
    ['Name', str(answer.name)],
    ['Kind', kind],
    ['Role', str(db.role) || '—'],
  ];
  if (db.kind === 'layer') rows.push(['Base', str(db.base)]);
  const layers = arr(answer.layers);
  if (db.role === 'base') rows.push(['Layers over it', layers.length ? layers.join(', ') : 'none']);
  const aliases = arr(answer.aliases);
  rows.push(['Aliases', aliases.length ? aliases.join(', ') : 'none', 'Aliases naming this database (Databases tab: set, flip, rename)']);
  rows.push(['Created', str(db.created) || '—']);
  return rows;
}

/** `[label, value, title?]` rows of the state and sizes section. */
export function stateRows(answer = {}) {
  const db = answer.database || {};
  const rows = [['State', str(db.state) || '—']];
  if (db.reason) rows.push(['Reason', str(db.reason)]);
  rows.push(['On disk', bytesText(db.sizeBytes) || '—', 'The database directory on disk (a shared model blob counts in full)']);
  let memory = '— (not loaded)';
  if (typeof db.memoryBytes === 'number') {
    memory = bytesText(db.memoryBytes);
    if (typeof db.sharedMemoryBytes === 'number') memory += ` (+ ${bytesText(db.sharedMemoryBytes)} of its base, shared)`;
  }
  rows.push(['In memory', memory, "The engine's footprint of the loaded graph"]);
  if (db.pooled) rows.push(['Pooled', arr(db.pooledBy).length ? `by ${arr(db.pooledBy).join(', ')}` : 'yes']);
  return rows;
}

/** Whether the read-only switch can be turned off (a base stays read-only). */
export function readOnlyLocked(answer = {}) {
  return (answer.database || {}).role === 'base';
}

/** `[label, value, title?]` rows of the Store's creation parameters and position (read-only). */
export function storeRows(answer = {}) {
  if (typeof answer.storeError === 'string') return [['Error', answer.storeError]];
  const s = answer.store || {};
  const text = (v) => (v === null || v === undefined || v === '' ? '—' : String(v));
  const rows = [
    ['Store id', text(s.storeId), "The Store's identity, made at its creation: a fork or repair is another store; a restored backup keeps it"],
    ['Damage policy', text(s.damagePolicy), 'What the Store does when damage is found while it is open: maintenance (turns read-only) or continue'],
    ['Chunk size', typeof s.chunkBytes === 'number' ? bytesText(s.chunkBytes) : '—', 'The snapshot chunk size'],
    ['Created', text(s.createdAt)],
    ['Backend', text(s.backend)],
    ['Format version', text(s.formatVersion)],
    ['Last commit', s.commitSeq ? `${s.commitSeq} at ${text(s.lastCommitAt)}` : 'none yet'],
    ['Snapshots', text(s.snapshots)],
    ['Journal (WAL)', typeof s.walBytes === 'number' ? bytesText(s.walBytes) : '—'],
  ];
  if (s.layerBase) rows.push(['Layer base', String(s.layerBase), 'The identity of the base this layer Store binds to']);
  return rows;
}

/**
 * The "Damage and recovery" part of the view (round 3 B-CRASH: `recovery`): `{maintenance,
 * damage, help, unclean}` with `unclean` = `{when, pid, outcome, rows: [[label, value]]}` when the
 * last unclean shutdown's check covered this database, else null.
 */
export function recoveryView(answer = {}) {
  const r = answer.recovery && typeof answer.recovery === 'object' ? answer.recovery : {};
  const u = r.uncleanShutdown && typeof r.uncleanShutdown === 'object' ? r.uncleanShutdown : null;
  let unclean = null;
  if (u && u.check && typeof u.check === 'object') {
    const c = u.check;
    const rows = [['Outcome', typeof c.outcome === 'string' ? c.outcome : '—']];
    if (typeof c.state === 'string') rows.push(['State afterwards', c.state]);
    const rec = c.recovery && typeof c.recovery === 'object' ? c.recovery : null;
    if (rec) {
      rows.push(['Closed cleanly', rec.cleanClose ? 'yes' : 'no (the server was killed)']);
      rows.push(['Journal replayed', `${rec.commitsReplayed || 0} commit(s) onto snapshot ${rec.snapshotCommitSeq}, now at commit ${rec.commitSeq}`]);
      if (rec.tornTail) rows.push(['Torn tail cut', rec.tornTail]);
      if (rec.intentCompleted) rows.push(['Completed', rec.intentCompleted]);
      if (rec.maintenance) rows.push(['Opened', 'in maintenance mode (damage found)']);
    }
    const v = c.verify && typeof c.verify === 'object' ? c.verify : null;
    if (v) rows.push(['Verify', v.ok ? 'no problem found' : `${Array.isArray(v.problems) ? v.problems.length : 0} problem(s)`]);
    if (typeof c.message === 'string' && c.message) rows.push(['Note', c.message]);
    unclean = {
      when: typeof u.detectedAtMs === 'number' ? new Date(u.detectedAtMs).toISOString().replace(/\.\d{3}Z$/, 'Z') : '',
      pid: typeof u.previousPid === 'number' ? u.previousPid : null,
      outcome: typeof c.outcome === 'string' ? c.outcome : '',
      rows,
    };
  }
  return {
    maintenance: r.maintenance === true,
    damage: typeof r.damage === 'string' ? r.damage : '',
    help: typeof r.help === 'string' ? r.help : '',
    unclean,
  };
}

/** The grant rows: `{key, subject, on, level, via, viaText}`. */
export function settingsGrantRows(answer = {}) {
  return arr(answer.grants)
    .filter((g) => g && typeof g.subject === 'string')
    .map((g) => {
      const via = ['database', 'alias', 'all'].includes(g.via) ? g.via : 'database';
      const on = str(g.database);
      return {
        key: `${g.subject}|${on}`,
        subject: g.subject,
        on: via === 'all' ? '* (every database)' : on,
        level: str(g.level),
        via,
        viaText: via === 'alias' ? 'through the alias (only by that name)' : via === 'all' ? 'every database' : 'the database',
      };
    });
}

/** The MCP rows of the database and its aliases. */
export function settingsMcpRows(answer = {}) {
  const mcp = answer.mcp || {};
  return arr(mcp.names).filter((r) => r && typeof r.name === 'string').map((r) => ({
    name: r.name,
    kind: r.kind === 'alias' ? 'alias' : 'database',
    offered: r.effectiveExposed !== false,
    readOnly: r.effectiveReadOnly === true,
    sessions: num(r.sessions) || 0,
    calls: num(r.calls) || 0,
  }));
}

/** The MCP line: on, switched off, or no endpoint (or the error). */
export function mcpText(answer = {}) {
  const mcp = answer.mcp || {};
  if (typeof mcp.error === 'string') return mcp.error;
  if (mcp.endpointBuilt === false) return 'MCP is off in server.toml (no endpoint).';
  if (mcp.enabled === false) return 'MCP is switched off for the whole server.';
  return 'MCP is on.';
}

/** The backups line: count, size, the latest. */
export function backupsText(answer = {}) {
  const b = answer.backups || {};
  const count = num(b.count) || 0;
  if (!count) return 'No backups yet.';
  const latest = b.latest && typeof b.latest.backup === 'string' ? `, latest ${b.latest.backup}` : '';
  return `${count} backup${count === 1 ? '' : 's'} (${bytesText(num(b.bytes) || 0)})${latest}`;
}

/** The outcome text of a change answer (`changed`: the fields). */
export function changedText(name, answer = {}) {
  const fields = arr(answer.changed);
  const limits = fields.some((f) => String(f).startsWith('limits.'));
  const what = fields.length ? fields.join(', ') : 'nothing';
  const later = limits && (answer.limits || {}).loaded === true ? ' The limits apply at its next load.' : '';
  return `${name}: changed ${what}.${later}`;
}
