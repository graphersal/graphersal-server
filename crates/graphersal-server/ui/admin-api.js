// The admin API of graphersal-server (pure, tested under Node by ui/tests/admin-*.test.mjs):
// request builders, the client, error parsing, form checks, the actions a row offers and the
// poll of a load. The views are the `server` UI profile's tabs (ui/databases-tab.js,
// ui/backups-tab.js, ui/export-tab.js, ui/import-tab.js, ui/models-tab.js); moved here from the
// engine's playground, where it was the landing view's admin panel.
//
// The contract is book/src/reference/admin_api.md (EXPERIMENTAL): every route below `api/admin/`, JSON bodies
// with `Content-Type: application/json` (unknown fields are refused, so the builders send only
// the documented ones), errors `{"error": <sentence>, "help"?: <text>}` shown verbatim
// (`AdminError`). Actions take REAL database names; an alias is changed only through the alias
// routes. Paths are relative to the server root (the base the client is created with).

/**
 * The rule of a NEW database, alias or user name, mirrored from the server's `names` module
 * (`validate_new_name` / `validate_new_user_name`; a change there changes it here): `[a-z0-9]`
 * first, `[a-z0-9._-]` inside, a letter or digit last, no `..`, at most `NAME_MAX` characters,
 * no Windows device name (also before an extension), and for databases and aliases not a
 * reserved name (`RESERVED_NAMES`). `newNameReason` runs the checks in the server's order with
 * the server's texts; `NAME_RULE` is the shape alone (lower case; names are answered in lower
 * case).
 */
export const NAME_RULE = /^[a-z0-9](?:[a-z0-9._-]{0,30}[a-z0-9])?$/;

/** The longest new name (bytes = characters: names are ASCII). */
export const NAME_MAX = 32;

/** The longest EXISTING name (made by a server before 0.3.0; still served, granted, restored). */
export const EXISTING_NAME_MAX = 63;

/** The rule of a new name in one sentence, the hint under every field where a name is typed. */
export const NAME_HINT = "Up to 32 characters: lower-case letters, digits, '.', '_' and '-'; starts and ends with a letter or digit.";

/** The names a new database or alias cannot take (the server's `RESERVED`). */
export const RESERVED_NAMES = Object.freeze(['jobs']);

/** The device names Windows reserves (the server's `WINDOWS_RESERVED`). */
const WINDOWS_DEVICES = Object.freeze([
  'con', 'prn', 'aux', 'nul', 'com0', 'com1', 'com2', 'com3', 'com4', 'com5', 'com6', 'com7',
  'com8', 'com9', 'lpt0', 'lpt1', 'lpt2', 'lpt3', 'lpt4', 'lpt5', 'lpt6', 'lpt7', 'lpt8', 'lpt9',
]);

/** A name's length as the server counts it (UTF-8 bytes). */
function byteLength(name) {
  return new TextEncoder().encode(name).length;
}

/** The first character and the character set, as the server checks both rules. */
function characterReason(name) {
  if (!/^[a-z0-9]/.test(name)) return 'a name starts with a lower-case letter or a digit';
  if (!/^[a-z0-9._-]*$/.test(name)) return "a name holds only lower-case letters, digits, '.', '_' and '-'";
  return null;
}

/** A Windows device name, also before an extension. */
function deviceReason(name) {
  return WINDOWS_DEVICES.includes(name.split('.')[0]) ? 'the name is reserved for a device on Windows' : null;
}

/**
 * Why the server refuses `name` (already lower case) as a NEW name, word for word (the message
 * of its `invalid name` error), or null. `reserved: false` for a user name (`RESERVED_NAMES`
 * holds database names only).
 */
export function newNameReason(name, { reserved = true } = {}) {
  const n = typeof name === 'string' ? name : '';
  if (!n) return 'a name cannot be empty';
  if (byteLength(n) > NAME_MAX) return 'a name has at most 32 characters';
  const chars = characterReason(n);
  if (chars) return chars;
  if (!/[a-z0-9]$/.test(n)) return 'a name ends with a lower-case letter or a digit';
  if (n.includes('..')) return "a name cannot hold '..'";
  const device = deviceReason(n);
  if (device) return device;
  if (reserved && RESERVED_NAMES.includes(n)) {
    return "the name is reserved: backup jobs write into backups/jobs/ by default, where the admin backups of a database named 'jobs' would go; pick another name, such as 'jobs-db'";
  }
  return null;
}

/**
 * Why the server refuses `name` (already lower case) as an EXISTING name (the rule before
 * 0.3.0, which names made then keep: at most 63 characters, not ending in '.'), or null.
 */
export function existingNameReason(name) {
  const n = typeof name === 'string' ? name : '';
  if (!n) return 'a name cannot be empty';
  if (byteLength(n) > EXISTING_NAME_MAX) return 'a name has at most 63 characters';
  const chars = characterReason(n);
  if (chars) return chars;
  if (n.endsWith('.')) return "a name cannot end with '.'";
  return deviceReason(n);
}

/** The server's error text for a refused name: `invalid name "x": <reason>`. */
function invalidNameText(name, reason) {
  return `invalid name ${JSON.stringify(name)}: ${reason}`;
}

/** How often a started load is polled (milliseconds). */
export const LOAD_POLL_MS = 1000;

/** How long a load is polled before the panel stops watching it (it keeps running). */
export const LOAD_POLL_LIMIT_MS = 10 * 60 * 1000;

/**
 * An admin API refusal or failure: `message` = the server's `error`, `help` its `help`, `login`
 * the login page a `401` names (`{"error", "login": "/"}`), else ''.
 */
export class AdminError extends Error {
  constructor(message, { status = 0, help = '', state = '', login = '', revision = '' } = {}) {
    super(message);
    this.name = 'AdminError';
    this.status = status;
    this.help = help;
    this.state = state;
    this.login = login;
    this.revision = revision;
  }
}

/**
 * Whether `err` is a refused stale change (`409` with the object's current `revision`: someone
 * else changed it since the tab read it; book/src/reference/admin_api.md "Revisions"). The tab then reads its
 * data again.
 */
export function isConflict(err) {
  return !!err && err.status === 409 && typeof err.revision === 'string' && err.revision !== '';
}

/** What the notice of a refused stale change adds: the tab has read its data again. */
export const CONFLICT_RELOADED = 'The tab shows the current state now: check it, then repeat the change if it is still wanted.';

/**
 * The notice of a failed action `{tone, text, help}`: the server's `error` and `help` verbatim;
 * a refused stale change (`isConflict`) is a warning whose help also says the data was read again.
 */
export function refusalNotice(err) {
  const text = err && err.message ? err.message : String(err);
  const help = err && typeof err.help === 'string' ? err.help : '';
  if (isConflict(err)) return { tone: 'warn', text, help: help ? `${help}. ${CONFLICT_RELOADED}` : CONFLICT_RELOADED };
  return { tone: 'error', text, help };
}

/**
 * The AdminError of a non-2xx answer: `{error, help?}` verbatim; a body without `error` names
 * the HTTP status. `text` is the raw body (maybe not JSON: a proxy's page).
 */
export function parseAdminError(status, text) {
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch (e) {
    data = null;
  }
  const error = data && typeof data.error === 'string' && data.error ? data.error : `HTTP ${status}`;
  const help = data && typeof data.help === 'string' ? data.help : '';
  const state = data && typeof data.state === 'string' ? data.state : '';
  const login = data && typeof data.login === 'string' ? data.login : '';
  const revision = data && typeof data.revision === 'string' ? data.revision : '';
  return new AdminError(error, { status, help, state, login, revision });
}

const seg = (name) => encodeURIComponent(String(name));

/** `request` based on `revision` (sent as `If-Match`): unchanged without one. */
const based = (request, revision) => (typeof revision === 'string' && revision ? { ...request, ifMatch: revision } : request);

/**
 * The request of one admin action: `{method, path, body?}` with `path` relative to the server
 * root. Actions: list, get, create(body), delete, clone(newName), load, unload,
 * readOnly(value), autoLoad(value), aliases, setAlias(alias, target, revision?, mcp?) (`mcp`
 * true: a NEW alias is offered to MCP; also `clone(name, newName, mcp?)`), removeAlias(alias),
 * renameAlias(alias, newName) (atomic; open MCP sessions that selected the alias move with it),
 * backup(name), backups(name), restore(name, backup, newName?) (no `name` field when restoring
 * over the database itself), importPath({path | inbox, name, base?, format?, description?, role?}),
 * imports (the import inbox `imports/` of the server directory), models,
 * addModel({path?, repo?, license?}), removeModel(repo or blob hash), promote(name) (no body;
 * one way: a base is frozen forever), runs(name?) (one loaded database's queued and running runs,
 * or every loaded database's without a name), cancelRun(name, id) (the run administration: a
 * run id is unique within its database only), the users and grants: users, addUser(name,
 * admin?) (its answer carries the new token, shown once), removeUser(name), newToken(name),
 * revokeToken(name), setAdmin(name, value), grant(subject, database, level), revokeGrant(subject,
 * database), server (the running server: version, uptime, the memory summary). The changes of a shared object take its `revision` last (optional: readOnly,
 * autoLoad, setAlias, removeAlias, renameAlias, removeUser, newToken, revokeToken, setAdmin,
 * grant, revokeGrant): the request then carries `ifMatch` and `sendAdmin` sends it as the
 * `If-Match` header, so a change based on a stale page is refused (`isConflict`). The transfers that are not JSON requests are
 * `exportPath` (a download) and `uploadImport` (the file as the body: an import, or a model
 * from `modelUploadRequest`).
 */
/**
 * `body` with `mcp: true` when `mcp` is true (round 4 B-MCPDEF: the new database or alias is
 * offered to MCP at birth); otherwise unchanged: the server's default applies (`mcp.toml`'s
 * `expose_new`, off unless an admin turned it on in the MCP tab).
 */
export const withMcp = (body, mcp) => (mcp === true ? { ...body, mcp: true } : body);

export const adminRequest = Object.freeze({
  list: () => ({ method: 'GET', path: 'api/admin/databases' }),
  get: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}` }),
  create: (body) => ({ method: 'POST', path: 'api/admin/databases', body }),
  delete: (name) => ({ method: 'DELETE', path: `api/admin/databases/${seg(name)}` }),
  clone: (name, newName, mcp) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/clone`, body: withMcp({ name: newName }, mcp) }),
  load: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/load` }),
  unload: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/unload` }),
  readOnly: (name, value, revision) => based({ method: 'PUT', path: `api/admin/databases/${seg(name)}/read-only`, body: { readOnly: !!value } }, revision),
  autoLoad: (name, value, revision) => based({ method: 'PUT', path: `api/admin/databases/${seg(name)}/auto-load`, body: { autoLoad: !!value } }, revision),
  aliases: () => ({ method: 'GET', path: 'api/admin/aliases' }),
  setAlias: (alias, target, revision, mcp) => based({ method: 'PUT', path: `api/admin/aliases/${seg(alias)}`, body: withMcp({ target }, mcp) }, revision),
  removeAlias: (alias, revision) => based({ method: 'DELETE', path: `api/admin/aliases/${seg(alias)}` }, revision),
  renameAlias: (alias, newName, revision) => based({ method: 'POST', path: `api/admin/aliases/${seg(alias)}/rename`, body: { name: newName } }, revision),
  backup: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/backup` }),
  backups: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}/backups` }),
  restore: (name, backup, newName) => ({
    method: 'POST',
    path: `api/admin/databases/${seg(name)}/restore`,
    body: newName && newName !== name ? { backup, name: newName } : { backup },
  }),
  importPath: (body) => ({ method: 'POST', path: 'api/admin/import', body }),
  imports: () => ({ method: 'GET', path: 'api/admin/imports' }),
  models: () => ({ method: 'GET', path: 'api/admin/models' }),
  addModel: (body) => ({ method: 'POST', path: 'api/admin/models', body }),
  removeModel: (id) => ({ method: 'DELETE', path: `api/admin/models/${modelSegs(id)}` }),
  promote: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/promote` }),
  users: () => ({ method: 'GET', path: 'api/admin/users' }),
  addUser: (name, isAdmin = false) => ({ method: 'POST', path: 'api/admin/users', body: isAdmin ? { name, admin: true } : { name } }),
  removeUser: (name, revision) => based({ method: 'DELETE', path: `api/admin/users/${seg(name)}` }, revision),
  newToken: (name, revision) => based({ method: 'POST', path: `api/admin/users/${seg(name)}/token` }, revision),
  revokeToken: (name, revision) => based({ method: 'DELETE', path: `api/admin/users/${seg(name)}/token` }, revision),
  setAdmin: (name, value, revision) => based({ method: 'PUT', path: `api/admin/users/${seg(name)}/admin`, body: { admin: !!value } }, revision),
  grant: (subject, database, level, revision) => based({ method: 'POST', path: 'api/admin/grants', body: { subject, database, level } }, revision),
  revokeGrant: (subject, database, revision) => based({ method: 'DELETE', path: `api/admin/grants/${seg(subject)}/${seg(database)}` }, revision),
  runs: (name) => ({ method: 'GET', path: name ? `api/admin/databases/${seg(name)}/runs` : 'api/admin/runs' }),
  server: () => ({ method: 'GET', path: 'api/admin/server' }),
  cancelRun: (name, id) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/runs/cancel`, body: { id } }),
  verify: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/verify` }),
  verifyAll: () => ({ method: 'POST', path: 'api/admin/verify' }),
  repair: (name, newName) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/repair`, body: newName ? { name: newName } : {} }),
  swap: (name, withName) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/swap`, body: { with: withName, confirm: true } }),
  recovery: () => ({ method: 'GET', path: 'api/admin/recovery' }),
});

/** A model's repository (`owner/name`: its slash stays part of the path) or blob hash as path segments. */
function modelSegs(id) {
  return String(id).split('/').map(seg).join('/');
}

/**
 * Sends one request (`adminRequest.*`) and resolves to the parsed answer; rejects with an
 * AdminError (the server's `error`/`help`, or the network failure). A body is sent as JSON.
 * `onRefusal(err)` (optional) sees every AdminError of a refused answer before it is thrown
 * (the profile sends a 401 to the login page: ./access-api.js `refusalAction`).
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>, onRefusal?: Function}} options
 */
export async function sendAdmin({ base, fetch, headers = {}, onRefusal = null }, request) {
  const init = {
    method: request.method,
    cache: 'no-store',
    headers: { ...headers, Accept: 'application/json' },
  };
  if (request.body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(request.body);
  }
  if (typeof request.ifMatch === 'string' && request.ifMatch) init.headers['If-Match'] = `"${request.ifMatch}"`;
  let r;
  try {
    r = await fetch(new URL(request.path, base), init);
  } catch (err) {
    throw new AdminError(`The server could not be reached: ${err && err.message ? err.message : err}`);
  }
  const text = await r.text();
  if (!r.ok) {
    const err = parseAdminError(r.status, text);
    if (typeof onRefusal === 'function') {
      try {
        onRefusal(err);
      } catch {
        // The hook only reacts; the request's own error is what the caller gets.
      }
    }
    throw err;
  }
  try {
    return text ? JSON.parse(text) : {};
  } catch (e) {
    throw new AdminError(`The server sent no JSON (HTTP ${r.status})`, { status: r.status });
  }
}

/** An admin client: one method per action of `adminRequest`, each answering `sendAdmin`'s promise. */
export function createAdmin(options) {
  const client = {};
  for (const [action, build] of Object.entries(adminRequest)) {
    client[action] = (...args) => sendAdmin(options, build(...args));
  }
  return client;
}

/**
 * The problem with the name of a NEW database or alias (trimmed, lower case as the server folds
 * it), or null: `Enter a <what>.` when empty, else the server's own refusal text
 * (`newNameReason`).
 */
export function nameProblem(name, what = 'name') {
  const n = typeof name === 'string' ? name.trim().toLowerCase() : '';
  if (!n) return `Enter a ${what}.`;
  const reason = newNameReason(n);
  return reason ? invalidNameText(n, reason) : null;
}

/**
 * The problem with the name of an EXISTING database or alias (one made before 0.3.0 may break
 * the rule of new names), or null; texts as `nameProblem`.
 */
export function existingNameProblem(name, what = 'name') {
  const n = typeof name === 'string' ? name.trim().toLowerCase() : '';
  if (!n) return `Enter a ${what}.`;
  const reason = existingNameReason(n);
  return reason ? invalidNameText(n, reason) : null;
}

/**
 * The problem with the new name of the alias `alias`, or null: a valid name other than its
 * current one (whether a database or another alias holds it is the server's answer).
 */
export function renameProblem(alias, newName) {
  const problem = nameProblem(newName, 'new alias name');
  if (problem) return problem;
  if (newName.trim().toLowerCase() === String(alias || '').toLowerCase()) return `The alias is named "${alias}" already.`;
  return null;
}

/**
 * The create body from the create form, or the problems: `{body}` | `{problems: [text]}`.
 * Form: `{name, kind: 'plain'|'layer', role: 'normal'|'base', base, description, schema (JSON
 * text, optional), readOnly, autoLoad, mcp (offer it to MCP)}`. Only the documented fields are sent, and only the ones
 * that differ from the server's defaults (a layer sends `kind` + `base`, no `role`). `defaults`: what the server
 * gives a new database when the request does not say (the listing's `newDatabases`, ./databases.js
 * `newDatabaseDefaults`): an unchecked box whose default is on sends `false`.
 */
export function createBody(form, defaults = {}) {
  const d = defaults || {};
  const problems = [];
  const f = form || {};
  const name = typeof f.name === 'string' ? f.name.trim().toLowerCase() : '';
  const np = nameProblem(name);
  if (np) problems.push(np);
  const body = { name };
  if (f.kind === 'layer') {
    body.kind = 'layer';
    const base = typeof f.base === 'string' ? f.base.trim() : '';
    if (!base) problems.push('Choose the base database of the layer.');
    else body.base = base;
  } else if (f.role === 'base') {
    body.role = 'base';
  }
  const description = typeof f.description === 'string' ? f.description.trim() : '';
  if (description) body.description = description;
  const schemaText = typeof f.schema === 'string' ? f.schema.trim() : '';
  if (schemaText) {
    let schema;
    try {
      schema = JSON.parse(schemaText);
    } catch (e) {
      problems.push(`The schema is not JSON: ${e.message}`);
    }
    if (schema !== undefined) {
      if (!schema || typeof schema !== 'object' || Array.isArray(schema)) problems.push('The schema must be a JSON object.');
      else body.schema = schema;
    }
  }
  if (f.autoLoad === true) body.autoLoad = true;
  else if (d.autoLoad === true) body.autoLoad = false;
  // Offered to MCP at birth only when asked (otherwise the server's default: hidden).
  if (f.mcp === true) body.mcp = true;
  else if (d.mcp === true) body.mcp = false;
  // A base is always read-only on the server; the flag is sent only for a normal database.
  if (f.readOnly === true && body.role !== 'base') body.readOnly = true;
  else if (f.readOnly !== true && d.readOnly === true && body.role !== 'base') body.readOnly = false;
  return problems.length ? { problems } : { body };
}

/** The listing's aliases as sorted rows `[{alias, target}]` (`aliases` of `GET api/admin/databases`). */
export function aliasRows(answer) {
  const map = answer && answer.aliases && typeof answer.aliases === 'object' ? answer.aliases : {};
  return Object.entries(map)
    .filter(([a, t]) => typeof a === 'string' && a && typeof t === 'string' && t)
    .map(([alias, target]) => ({ alias, target }))
    .sort((a, b) => (a.alias < b.alias ? -1 : a.alias > b.alias ? 1 : 0));
}

/** The real databases (no alias rows) of `databaseRows`' rows. */
export function realRows(rows) {
  return rows.filter((r) => !r.aliasOf);
}

/** The names of the databases a layer may be created over (`role` `base`, real rows). */
export function baseNames(rows) {
  return realRows(rows).filter((r) => r.role === 'base').map((r) => r.name);
}

/**
 * What a row offers, each `{enabled, title}` (title = why not, when disabled): load, unload,
 * clone, delete, readOnly (the toggle), autoLoad (the toggle). Alias rows offer nothing here
 * (they are changed in the aliases section). `busy` = an action of this row is running. A rule
 * the server checks anyway (delete while an alias names it, unload of a base in use) is left to
 * the server, whose error and help the panel shows.
 */
export function rowActions(row, { busy = false, aliasTargets = [] } = {}) {
  const off = (title) => ({ enabled: false, title });
  const on = (title) => ({ enabled: true, title });
  const all = (why) => ({ load: off(why), unload: off(why), clone: off(why), delete: off(why), readOnly: off(why), autoLoad: off(why) });
  if (row.aliasOf) return all(`An alias of ${row.aliasOf}: change it under Aliases`);
  if (busy) return all('Wait until the running action ends');
  const configless = !row.kind; // a failed database whose database.toml cannot be read
  const s = row.state;
  const open = isOpenState(s);
  return {
    load: open || s === 'loading'
      ? off(open ? 'Already loaded' : 'Loading…')
      : configless ? off('Its configuration cannot be read') : on('Load it into memory and serve it'),
    unload: open ? on('Close it; running requests get up to 30 s') : off('Not loaded'),
    clone: configless || s === 'failed'
      ? off('A failed database cannot be cloned')
      : on('Copy its latest state into a new database'),
    delete: open || s === 'loading'
      ? off('Unload it first')
      : aliasTargets.includes(row.name)
        ? off('An alias names it: remove or flip the alias first')
        : on("Move it to the server's trash/ directory"),
    readOnly: configless
      ? off('Its configuration cannot be read')
      : row.role === 'base'
        ? off('A base database is always read-only')
        : on(row.readOnly ? 'Allow writes again' : 'Refuse every write'),
    autoLoad: configless ? off('Its configuration cannot be read') : on('Load it when the server starts'),
  };
}

// ------------------------------------------------------------------ verify, repair, swap (B-CRASH)

/** Whether a database state is loaded and served: `loaded`, or `maintenance` (loaded read-only, damaged). */
export function isOpenState(state) {
  return state === 'loaded' || state === 'maintenance';
}

/** The tooltip of "Verify". */
export const VERIFY_HELP = 'Check every checksum, snapshot and journal record of its Store (a loaded database: its open Store; damage found makes it read-only)';

/** What a repair does, for its dialog. */
export const REPAIR_HELP = 'A repair builds a NEW database from the damaged one: damaged parts are rebuilt from older snapshots and the journal, and what cannot be rebuilt is reported as lost. The damaged database is not changed. Afterwards the copy can take its name (the damaged one moves to the trash/ directory), or both are kept.';

/** The hint after a crash or damage: a backup is the other way back. */
export const RESTORE_HINT = 'Or restore a backup instead: Backups… on its row.';

/**
 * The damage actions of a row (round 3 B-CRASH): `{verify, repair}`, each `{enabled, title}`.
 * Verify applies to every database but a loading one (a failed one's files are verified);
 * repair: `repairAction`.
 */
export function damageActions(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  if (row.aliasOf) {
    const why = `An alias of ${row.aliasOf}: use that database's row`;
    return { verify: off(why), repair: off(why) };
  }
  if (busy) {
    const why = 'Wait until the running action ends';
    return { verify: off(why), repair: off(why) };
  }
  return {
    verify: row.state === 'loading' ? off('Loading…: verify it once loaded') : { enabled: true, title: VERIFY_HELP },
    repair: repairAction(row),
  };
}

/**
 * Whether "Repair…" applies to a row: `{enabled, title}`. A plain normal database with a
 * readable configuration (the engine repairs plain Stores; a layer or base is restored from a
 * backup instead).
 */
export function repairAction(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  if (row.aliasOf) return off(`An alias of ${row.aliasOf}: use that database's row`);
  if (busy) return off('Wait until the running action ends');
  if (!row.kind) return off('Its configuration cannot be read');
  if (row.kind === 'layer') return off('A layer cannot be repaired: restore a backup of it');
  if (row.role === 'base') return off('A base cannot be repaired: restore a backup of it');
  if (row.state === 'loading') return off('Loading…: repair it once loaded');
  return { enabled: true, title: row.state === 'maintenance' ? 'Its Store is damaged: build a repaired copy as a new database' : 'Build a repaired copy as a new database (the database is not changed)' };
}

/** The default name of a repaired copy: `<name>-repaired`, cut to a valid new name like `restoredName`. */
export function repairedName(name) {
  const suffix = '-repaired';
  const n = String(name || '').toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/\.{2,}/g, '.')
    .slice(0, NAME_MAX - suffix.length).replace(/[._-]+$/, '');
  return n ? `${n}${suffix}` : 'repaired';
}

/**
 * The verdict of a verify result (`POST .../verify`, one of `POST verify`, or a failure
 * `{database, error}`): `ok`, `needs-model` (only embedding model blobs are missing: the graph
 * data is intact, add the model), `damaged` or `failed` (the verify itself failed). An older
 * server sends no `verdict`: `ok` decides.
 */
export function verifyVerdict(result) {
  if (!result || typeof result !== 'object') return 'failed';
  if (typeof result.error === 'string') return 'failed';
  if (result.state === 'maintenance') return 'damaged';
  if (['ok', 'needs-model', 'damaged'].includes(result.verdict)) return result.verdict;
  return result.ok === true ? 'ok' : 'damaged';
}

/** Whether a verify result reports something to fix (damage, a missing model, a failure). */
export function verifyDamaged(result) {
  return verifyVerdict(result) !== 'ok';
}

/** The short hash of a blob (8 hex digits). */
const shortHash = (hash) => (typeof hash === 'string' ? hash.slice(0, 8) : '?');

/**
 * One verify problem in words: `path at byte N: reason`; a missing model's blob: `the embedding
 * model NAME (blob f0ed7648) is missing`.
 */
export function problemText(p) {
  if (!p || typeof p !== 'object') return '';
  if (p.kind === 'model') {
    if (typeof p.model === 'string' && p.model) return `the embedding model ${p.model} (blob ${shortHash(p.blob)}) is missing`;
    const candidates = Array.isArray(p.candidates) ? p.candidates.filter((c) => typeof c === 'string') : [];
    return `the blob ${shortHash(p.blob)} of one of the embedding models ${candidates.join(', ')} is missing`;
  }
  return `${p.path}${typeof p.offset === 'number' ? ` at byte ${p.offset}` : ''}: ${p.reason}`;
}

/** The models a verify result (or a last verify) names missing. */
function modelsMissing(result) {
  const listed = result && Array.isArray(result.modelsMissing) ? result.modelsMissing.filter((m) => typeof m === 'string' && m) : [];
  if (listed.length) return listed;
  const problems = result && Array.isArray(result.problems) ? result.problems : [];
  return [...new Set(problems.filter((p) => p && p.kind === 'model' && typeof p.model === 'string').map((p) => p.model))];
}

/** What adds the missing models of a verify result, in words (the Models tab, the console). */
export function modelRemedy(result) {
  const problems = result && Array.isArray(result.problems) ? result.problems : [];
  const models = modelsMissing(result);
  const shared = problems.some((p) => p && p.kind === 'model' && p.shared === true);
  const which = models.length ? models.join(', ') : 'the missing model';
  return shared
    ? `The server's shared models hold ${which}: add it again on the Models tab (or graphersal-server models add ${models[0] || 'NAME'}); the database is then ready to load.`
    : `Download ${which} (a Model2Vec directory) and import it on the Models tab (a model directory as .tar/.tar.gz, or a .gsnap carrying it), or graphersal-server models add SOURCE; the database is then ready to load.`;
}

/** One verify result in words (a notice). */
export function verifyText(result) {
  if (!result || typeof result !== 'object') return 'The verify answered nothing.';
  const name = result.database || '';
  if (typeof result.error === 'string') return `"${name}": the verify failed: ${result.error}`;
  const problems = Array.isArray(result.problems) ? result.problems : [];
  const n = (k) => (typeof result[k] === 'number' ? result[k] : 0);
  const checked = `${counted(n('snapshotsChecked'), 'snapshot')}, ${counted(n('recordsChecked'), 'journal record')}`;
  const verdict = verifyVerdict(result);
  if (verdict === 'ok') return `"${name}" verifies: no problem found (${checked}).`;
  if (result.ok === true) return `"${name}" is in maintenance (read-only): its Store opened with damage.`;
  if (verdict === 'needs-model') return `"${name}" needs its embedding model: ${modelsMissing(result).join(', ') || 'a model'} is missing (the graph data is intact).`;
  const p0 = problems[0];
  const first = !p0 ? '' : p0.kind === 'model' ? `; first: ${problemText(p0)}` : `; first: ${p0.path}: ${p0.reason}`;
  return `"${name}" is DAMAGED: ${counted(problems.length, 'problem')}${first}.`;
}

/** The problems of a verify result as lines ([`problemText`]). */
export function problemLines(result) {
  const problems = result && Array.isArray(result.problems) ? result.problems : [];
  return problems.map(problemText);
}

/** What to do about one verify result: the help under its toast ('' when it is ok). */
export function verifyHelp(result, restoreHint = '') {
  const verdict = verifyVerdict(result);
  if (verdict === 'ok' || verdict === 'failed') return '';
  const lines = problemLines(result);
  const head = lines.length ? `${lines.join('\n')}\n` : '';
  if (verdict === 'needs-model') return `${head}${modelRemedy(result)}`;
  return `${head}Repair… builds a repaired copy as a new database. ${restoreHint}`.trim();
}

/**
 * The text of Verify all from its results (each a verify result or `{database, error}`):
 * `{tone, text, lines}`; `total`: how many databases were to be verified, `cancelled`: whether
 * the run stopped early. Accepts the answer of `POST verify` (`{results}`) too.
 */
export function verifyAllText(answer, { total, cancelled = false } = {}) {
  const results = Array.isArray(answer) ? answer : answer && Array.isArray(answer.results) ? answer.results : [];
  const of = typeof total === 'number' ? total : results.length;
  const stopped = cancelled ? ` Cancelled after ${results.length} of ${of}.` : '';
  if (!results.length) return { tone: cancelled ? 'warn' : 'ok', text: cancelled ? `Verify all cancelled before the first database.` : 'No databases to verify.', lines: [] };
  const bad = results.filter(verifyDamaged);
  const verified = cancelled ? `${results.length} of ${of}` : `${results.length}`;
  if (!bad.length) {
    const who = cancelled ? 'Every database verified so far verifies' : 'Every database verifies';
    return { tone: cancelled ? 'warn' : 'ok', text: `${who} (${verified}).${stopped}`, lines: [] };
  }
  const damaged = bad.filter((r) => ['damaged', 'failed'].includes(verifyVerdict(r)));
  const models = bad.filter((r) => verifyVerdict(r) === 'needs-model');
  const parts = [];
  if (damaged.length) parts.push(`${counted(damaged.length, 'database')} of ${verified} damaged or not verifiable: ${damaged.map((r) => r.database).join(', ')}. Repair one into a new database, or restore a backup.`);
  if (models.length) parts.push(`${counted(models.length, 'database')} of ${verified} need${models.length === 1 ? 's' : ''} an embedding model: ${models.map((r) => r.database).join(', ')}. Add the missing model on the Models tab.`);
  const lines = [];
  for (const r of bad) {
    lines.push(verifyText(r));
    for (const line of problemLines(r).slice(0, 5)) lines.push(`  ${line}`);
    if (verifyVerdict(r) === 'needs-model') lines.push(`  ${modelRemedy(r)}`);
  }
  return { tone: damaged.length ? 'error' : 'warn', text: `${parts.join(' ')}${stopped}`, lines };
}

/** The Verify all button's text: `Verify all`, `Verifying 3/10…`, `Cancelling…`. */
export function verifyAllLabel(run) {
  if (!run || !run.running) return 'Verify all';
  if (run.cancel) return 'Cancelling…';
  return `Verifying ${run.done}/${run.total}…`;
}

/**
 * Verifies the databases `names` one after the other through `verifyOne(name)` (the
 * per-database route): `progress(done, total)` after each; `cancelled()` is asked before each
 * next database (the one running finishes). A failure of one database is its result
 * `{database, error}`, the run goes on. Answers `{results, cancelled}`.
 */
export async function runVerifyAll(names, verifyOne, { progress = () => {}, cancelled = () => false } = {}) {
  const results = [];
  const total = names.length;
  progress(0, total);
  for (const name of names) {
    if (cancelled()) return { results, cancelled: true };
    try {
      results.push(await verifyOne(name));
    } catch (err) {
      results.push({ database: name, ok: false, error: (err && err.message) || String(err) });
    }
    progress(results.length, total);
  }
  return { results, cancelled: false };
}

/**
 * The small badge of a database row whose last verify found something (`lastVerify` of the
 * row): `{text, tone, title}`, or null (never verified, or the last verify was clean).
 */
export function verifyBadge(row) {
  const last = row && row.lastVerify;
  if (!last || last.verdict === 'ok') return null;
  const when = typeof last.atMs === 'number' ? new Date(last.atMs).toISOString().replace(/\.\d{3}Z$/, 'Z') : '';
  if (last.verdict === 'needs-model') {
    return { text: 'verify: needs model', tone: 'warn', title: `Last verify ${when}: the embedding model ${last.modelsMissing.join(', ')} is missing; add it on the Models tab` };
  }
  if (last.verdict === 'failed') return { text: 'verify failed', tone: 'danger', title: `Last verify ${when}: the verify failed` };
  return { text: `verify: ${counted(last.problemCount, 'problem')}`, tone: 'danger', title: `Last verify ${when}: ${counted(last.problemCount, 'problem')}; the Database settings show them` };
}

/**
 * The summary of a repair answer (`POST .../repair`): `{tone, text, lines}`; `lines` lists what
 * was rebuilt, lost and not carried.
 */
export function repairSummary(answer) {
  const r = answer && answer.repair && typeof answer.repair === 'object' ? answer.repair : {};
  const lines = [];
  for (const x of Array.isArray(r.repaired) ? r.repaired : []) lines.push(`rebuilt: ${x}`);
  for (const c of Array.isArray(r.lostCommits) ? r.lostCommits : []) {
    lines.push(c && typeof c.last === 'number' ? `lost: commits ${c.first}..${c.last}` : `lost: commits from ${c && c.first} on`);
  }
  for (const x of Array.isArray(r.lostRanges) ? r.lostRanges : []) lines.push(`lost: ${x}`);
  for (const x of Array.isArray(r.lostElements) ? r.lostElements : []) lines.push(`lost: ${x}`);
  if (typeof r.divergedCount === 'number' && r.divergedCount > 0) lines.push(`diverged: ${counted(r.divergedCount, 'element')}, check them by hand`);
  for (const x of Array.isArray(r.modelsNotCarried) ? r.modelsNotCarried : []) lines.push(`model not carried: ${x}`);
  if (typeof r.notCarried === 'string' && r.notCarried) lines.push(`never carried by a repair: ${r.notCarried}`);
  const lossless = r.lossless === true;
  return {
    tone: lossless ? 'ok' : 'warn',
    text: `Repaired "${answer && answer.repairedFrom}" into the new database "${answer && answer.name}" at commit ${typeof r.commitSeq === 'number' ? r.commitSeq : '?'}: ${lossless ? 'lossless, verified' : 'DATA LOST or diverged (below), verified'}.`,
    lines,
  };
}

/**
 * The rows of the last unclean shutdown (`GET api/admin/recovery` `uncleanShutdown`, or `GET
 * api/admin/server`'s): `{when, pid, done, known, rows: [{name, outcome, tone, text, damaged}]}`,
 * or null when there was none.
 */
export function uncleanRows(report) {
  if (!report || typeof report !== 'object' || !Array.isArray(report.databases)) return null;
  const tones = { ok: 'ok', damaged: 'danger', failed: 'danger', 'needs-model': 'warn', missing: 'warn', pending: 'accent', checking: 'accent' };
  const rows = report.databases
    .filter((d) => d && typeof d.name === 'string')
    .map((d) => {
      const parts = [];
      const rec = d.recovery && typeof d.recovery === 'object' ? d.recovery : null;
      if (rec) {
        parts.push(`${rec.cleanClose ? 'closed cleanly' : 'not closed cleanly'}; ${counted(rec.commitsReplayed || 0, 'journal commit')} replayed, at commit ${rec.commitSeq}`);
        if (rec.tornTail) parts.push(`torn tail cut (${rec.tornTail})`);
      }
      const v = d.verify && typeof d.verify === 'object' ? d.verify : null;
      if (v) parts.push(v.ok ? 'verify ok' : `verify found ${counted(Array.isArray(v.problems) ? v.problems.length : 0, 'problem')}`);
      if (typeof d.message === 'string' && d.message) parts.push(d.message);
      const outcome = typeof d.outcome === 'string' ? d.outcome : '';
      return { name: d.name, outcome, tone: tones[outcome] || '', text: parts.join('; '), damaged: outcome === 'damaged' };
    });
  return {
    when: typeof report.detectedAtMs === 'number' ? new Date(report.detectedAtMs).toISOString().replace(/\.\d{3}Z$/, 'Z') : '',
    pid: typeof report.previousPid === 'number' ? report.previousPid : null,
    done: report.done === true,
    known: report.databasesKnown !== false,
    rows,
  };
}

/** The note of the restore form: what a restore does not bring back. */
export const RESTORE_GRANTS_NOTE = 'Grants do not come back with a restore: a deleted database\'s grants went with it, and a restore into a new name starts without any. Grant access again on the Users tab. A restore over the database itself keeps its grants.';

/** The confirmation text of a destructive action, or null (no confirmation needed). */
export function confirmText(action, subject, extra = '') {
  switch (action) {
    case 'delete':
      return `Delete the database "${subject}"? Its directory moves to the server's trash/ directory (recoverable by hand only), and every grant on it is removed (a restore does not bring them back).`;
    case 'unload':
      return `Unload "${subject}"? Running requests and jobs get up to 30 s; its pages stop answering until it is loaded again.`;
    case 'removeAlias':
      return `Remove the alias "${subject}"? Links to db/${subject}/ stop working.`;
    case 'flipAlias':
      return `Point the alias "${subject}" to "${extra}"? Its pages serve the other database from now on.`;
    case 'restoreOver':
      return `Restore "${subject}" from the backup ${extra}? Its current directory moves to the server's trash/ directory and the backup takes its place.`;
    case 'removeModel':
      return `Remove the model "${subject}" from the server's models/ directory? Its blob files are deleted.`;
    case 'promote':
      return `Promote "${subject}" to a base? ${BASE_IS_FROZEN}\n\n${BASE_HELP}\n\n${PROMOTE_HELP}`;
    case 'cancelRun':
      return `Stop run ${extra} of "${subject}"? Its query ends at its next check and its changes roll back; its client gets an error.`;
    case 'swap':
      return `Let "${extra}" take the name "${subject}"? The current "${subject}" moves to the server's trash/ directory (recoverable by hand only) and the name "${extra}" goes away; the grants, aliases and settings of "${subject}" stay. Both are unloaded first; "${subject}" is loaded again if it was loaded.`;
    default:
      return null;
  }
}

/** `n` and `thing`, plural with an "s" unless `n` is 1. */
function counted(n, thing) {
  return `${n} ${thing}${n === 1 ? '' : 's'}`;
}

/** How many grants left on the name (from a deleted database or alias) the answer says were removed before the new database or alias took it. */
export function leftoverGrants(answer = {}) {
  const n = answer && answer.leftoverGrantsRemoved;
  return typeof n === 'number' && n > 0 ? n : 0;
}

/** The warning sentence for removed leftover grants ('' when none). */
function leftoverText(answer) {
  const n = leftoverGrants(answer);
  return n ? ` Removed ${counted(n, 'grant')} left on this name from a deleted database or alias; it does not inherit them.` : '';
}

/** The notice tone of a finished action: `warn` when grants left on the name were removed, else `tone`. */
export function outcomeTone(answer = {}, tone = 'ok') {
  return leftoverGrants(answer) ? 'warn' : tone;
}

/** The one-line outcome of a finished action (shown as a notice). */
export function outcomeText(action, subject, answer = {}) {
  const text = baseOutcomeText(action, subject, answer);
  return ['create', 'clone', 'setAlias', 'renameAlias', 'restore', 'import'].includes(action)
    ? text + leftoverText(answer) + mcpText(answer) : text;
}

/**
 * The tooltip of the "Expose through MCP" checkbox of the create, clone, new alias and import
 * forms (round 4 B-MCPDEF).
 */
export const MCP_NEW_HELP = 'Offer the new database or alias to MCP agents (granted access to it) right away. '
  + 'Unchecked: the server\'s default for new names applies (MCP tab, "Expose new databases and aliases"; off unless an admin turned it on), '
  + 'so it stays hidden from MCP until it is exposed on the MCP tab.';

/** The MCP part of a new name's outcome (`mcpExposed` of the answer), or ''. */
function mcpText(answer) {
  if (answer.mcpExposed === true) return ' Offered to MCP.';
  if (answer.mcpExposed === false) return ' Not offered to MCP (expose it on the MCP tab).';
  return '';
}

function baseOutcomeText(action, subject, answer) {
  switch (action) {
    case 'create': return `Created "${subject}" (unloaded).`;
    case 'clone': return `Cloned into "${subject}" (unloaded).`;
    case 'delete': {
      const g = typeof answer.grantsRemoved === 'number' ? answer.grantsRemoved : 0;
      const grants = g ? `; ${counted(g, 'grant')} on it removed (a restore does not bring them back)` : '';
      return `Deleted "${subject}"${answer.trash ? `; it was moved to ${answer.trash}` : ''}${grants}.`;
    }
    case 'load':
      if (answer.state === 'loaded') return `"${subject}" is loaded.`;
      if (answer.state === 'failed') return `"${subject}" failed to load${answer.reason ? `: ${answer.reason}` : ''}.`;
      if (answer.state === 'needs-model') return `"${subject}" needs a model${answer.model ? ` (${answer.model})` : ''}${answer.reason ? `: ${answer.reason}` : ''}.`;
      return `"${subject}" is ${answer.state || 'loading'}.`;
    case 'unload': return answer.unloaded === false ? `"${subject}" was not loaded.` : `"${subject}" is unloaded.`;
    case 'readOnly': return answer.readOnly ? `"${subject}" is read-only.` : `"${subject}" accepts writes.`;
    case 'autoLoad': return answer.autoLoad ? `"${subject}" loads when the server starts.` : `"${subject}" no longer loads at start.`;
    case 'setAlias': return `The alias "${answer.alias || subject}" names "${answer.target}".`;
    case 'removeAlias': return `Removed the alias "${subject}".`;
    case 'renameAlias': {
      const moved = typeof answer.mcpSessionsMoved === 'number' && answer.mcpSessionsMoved > 0
        ? `; ${answer.mcpSessionsMoved} open MCP session${answer.mcpSessionsMoved === 1 ? '' : 's'} moved with it` : '';
      return `Renamed the alias "${answer.renamedFrom || ''}" to "${answer.alias || subject}" (database "${answer.target || ''}")${moved}. Links to the old name stop working.`;
    }
    case 'backup': return `Backed up "${subject}" as ${answer.backup || 'a new backup'}${typeof answer.bytes === 'number' ? ` (${bytesText(answer.bytes)})` : ''}.`;
    case 'restore': {
      const from = answer.restoredFrom && answer.restoredFrom.backup ? ` from ${answer.restoredFrom.database || ''}/${answer.restoredFrom.backup}` : '';
      const was = answer.replaced ? `; the replaced directory moved to ${answer.trash || "the server's trash/ directory"}` : '';
      return `Restored "${answer.name || subject}"${from} (${answer.state || 'unloaded'})${was}.`;
    }
    case 'import': {
      const imp = answer.import && typeof answer.import === 'object' ? answer.import : {};
      const what = [imp.format, answer.kind, answer.role === 'base' ? 'base' : ''].filter(Boolean).join(', ');
      const state = answer.state === 'needs-model' ? `needs a model${answer.model ? ` (${answer.model})` : ''}` : answer.state || 'unloaded';
      return `Imported "${answer.name || subject}"${what ? ` (${what})` : ''}: ${state}.`;
    }
    case 'addModel': {
      const added = Array.isArray(answer.added) ? answer.added.length : 0;
      const resolved = Array.isArray(answer.resolved) ? answer.resolved : [];
      return `Added ${added} model blob${added === 1 ? '' : 's'}${resolved.length ? `; ready to load: ${resolved.join(', ')}` : ''}.`;
    }
    case 'removeModel': return `Removed the model "${answer.model || subject}"${typeof answer.bytes === 'number' ? ` (${bytesText(answer.bytes)} freed)` : ''}.`;
    case 'cancelRun': return answer.cancelled
      ? `Run ${answer.id} of "${subject}" is stopping.`
      : `Run ${answer.id} of "${subject}" had already ended.`;
    case 'promote': return `"${answer.name || subject}" is now a base (read-only, frozen); create layers over it with "New database…".`;
    case 'verify': return verifyText(answer);
    case 'swap': return `"${answer.swappedWith || ''}" took the name "${answer.name || subject}"; the old one moved to ${answer.trash || "the server's trash/ directory"}${answer.reloaded ? '; loaded again' : ''}.`;
    default: return '';
  }
}

/**
 * Starts a load and watches it: `POST load`, then `GET databases/<name>` every `intervalMs`
 * while the state is `loading`; `onState(entry)` gets every answer. Resolves to the last entry
 * (`loaded`, `failed`, `needs-model`, or still `loading` after `limitMs`: the panel stops
 * watching, the load goes on); rejects with the AdminError of a refused request. `sleep` and
 * `now` are injectable (tests).
 */
export async function loadAndWatch(admin, name, {
  onState = () => {}, intervalMs = LOAD_POLL_MS, limitMs = LOAD_POLL_LIMIT_MS,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), now = () => Date.now(),
  alive = () => true,
} = {}) {
  let entry = await admin.load(name);
  onState(entry);
  const start = now();
  while (entry && entry.state === 'loading' && alive() && now() - start < limitMs) {
    await sleep(intervalMs);
    if (!alive()) break;
    entry = await admin.get(name);
    onState(entry);
  }
  return entry;
}

/** A byte count exactly (`1,572,864 bytes`, the tooltip of a `bytesText`), or '' for a missing value. */
export function exactBytes(n) {
  if (typeof n !== 'number' || !Number.isFinite(n) || n < 0) return '';
  const digits = String(Math.round(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${digits} byte${n === 1 ? '' : 's'}`;
}

/** A byte count as text (`1.5 MB`), or '' for a missing value. */
export function bytesText(n) {
  if (typeof n !== 'number' || !Number.isFinite(n) || n < 0) return '';
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}


// ------------------------------------------------------------------ backups, export, import, models (P3)

/**
 * The extra row actions of the transfers, each `{enabled, title}`: backup, backups (the list,
 * with restore), export. Kept apart from `rowActions` (its set is the P2 contract). A failed
 * database whose config cannot be read offers only its backups; a failed one cannot be backed up
 * or exported.
 */
export function transferActions(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  const on = (title) => ({ enabled: true, title });
  if (row.aliasOf) {
    const why = `An alias of ${row.aliasOf}: use that database's row`;
    return { backup: off(why), backups: off(why), export: off(why) };
  }
  if (busy) {
    const why = 'Wait until the running action ends';
    return { backup: off(why), backups: off(why), export: off(why) };
  }
  const failed = row.state === 'failed' || !row.kind;
  return {
    backup: failed ? off('A failed database cannot be backed up') : on("Write a verified ZIP backup into the server's backups/ directory"),
    backups: on('List its backups and restore one'),
    export: failed ? off('A failed database cannot be exported') : on('Download a packed snapshot (.gsnap)'),
  };
}

/** The backups of a `GET .../backups` answer as rows, oldest first (as sent); malformed ones skipped. */
export function backupRows(answer) {
  const list = answer && Array.isArray(answer.backups) ? answer.backups : [];
  return list
    .filter((b) => b && typeof b === 'object' && typeof b.backup === 'string' && b.backup)
    .map((b) => ({
      backup: b.backup,
      database: typeof b.database === 'string' ? b.database : '',
      file: typeof b.file === 'string' ? b.file : '',
      bytes: typeof b.bytes === 'number' && Number.isFinite(b.bytes) && b.bytes >= 0 ? b.bytes : null,
      created: typeof b.created === 'string' ? b.created : '',
      kind: typeof b.kind === 'string' ? b.kind : '',
      role: typeof b.role === 'string' ? b.role : '',
      base: typeof b.base === 'string' ? b.base : '',
      layerBase: typeof b.layerBase === 'string' ? b.layerBase : '',
    }));
}

/**
 * Whether a backup of `name` may be restored over the database itself: `{enabled, title}`. The
 * database must be unloaded (or no longer exist: the backup becomes a new database of that
 * name). `row` is the database's listing row, or null when it does not exist.
 */
export function restoreOverAction(row) {
  if (!row) return { enabled: true, title: 'The database no longer exists: the backup becomes a new database of this name' };
  if (isOpenState(row.state) || row.state === 'loading') return { enabled: false, title: 'Unload it first (or restore into a new name)' };
  return { enabled: true, title: "Replace it; its current directory moves to the server's trash/ directory" };
}

/**
 * The export download: `{path, fileName}` with `path` relative to the server root
 * (`api/admin/databases/<name>/export?models=true|false[&materialize=true]`) and the file name the
 * server sends in its Content-Disposition (`<name>.gsnap`, `<name>.layer.gsnap` for a layer
 * snapshot). `materialize` is sent only for a layer (a standalone full snapshot instead of the
 * layer's changes); a layer snapshot never carries models.
 */
export function exportPath(name, { models = true, materialize = false, layer = false } = {}) {
  const query = [`models=${models ? 'true' : 'false'}`];
  if (layer && materialize) query.push('materialize=true');
  const layerFile = layer && !materialize;
  return {
    path: `api/admin/databases/${seg(name)}/export?${query.join('&')}`,
    fileName: `${name}${layerFile ? '.layer' : ''}.gsnap`,
  };
}

/**
 * The warning shown beside every server backup download: the archive holds the users' token
 * hashes (and every setting), so it is a secret.
 */
export const SERVER_BACKUP_WARNING = "The archive holds the users' token hashes, the grants and every setting: keep it like a password (an encrypted disk or a password manager), never in a shared folder.";

/**
 * The server backup's download (`GET api/admin/server-backup`, admin only): the configuration,
 * or with `withDatabases` every database's data too (a full backup). `fileName` is the fallback
 * when the answer names none (`Content-Disposition`).
 */
export function serverBackupPath({ withDatabases = false } = {}) {
  return {
    path: `api/admin/server-backup${withDatabases ? '?withDatabases=1' : ''}`,
    fileName: withDatabases ? 'server-backup-full.zip' : 'server-backup.zip',
  };
}

/** The file name of a `Content-Disposition: attachment; filename="..."` header, else `fallback`. */
export function fileNameOf(disposition, fallback) {
  const m = /filename="([^"\\/]+)"/.exec(disposition || '');
  return m ? m[1] : fallback;
}

/**
 * Fetches the server backup (with the host `headers`, through `fetch`, against `base`): `{blob,
 * fileName, skipped}` (`skipped`: the databases a full backup left out, from
 * `X-Graphersal-Skipped`); a refusal throws the server's `AdminError` (`error`/`help`).
 */
export async function fetchServerBackup({ base, fetch = globalThis.fetch, headers = {}, withDatabases = false }) {
  const target = serverBackupPath({ withDatabases });
  let r;
  try {
    r = await fetch(new URL(target.path, base).href, { cache: 'no-store', headers: { ...headers } });
  } catch (err) {
    throw new AdminError(`The server could not be reached: ${err && err.message ? err.message : err}`);
  }
  if (!r.ok) throw parseAdminError(r.status, await r.text());
  const skipped = (r.headers.get('X-Graphersal-Skipped') || '').split(',').filter(Boolean);
  return { blob: await r.blob(), fileName: fileNameOf(r.headers.get('Content-Disposition'), target.fileName), skipped };
}

/** The upload size limit (`uploadBytes` of the root `/api/info`), or null when not announced. */
export function uploadLimit(info) {
  const n = info && info.uploadBytes;
  return typeof n === 'number' && Number.isFinite(n) && n > 0 ? n : null;
}

/** What to do instead of an upload that is too large, per upload kind (`import`, `model`). */
const PATH_ADVICE = Object.freeze({
  import: { instead: 'Import it from a path on the server instead.', larger: 'import a larger file from a path on the server' },
  model: { instead: 'Add it from a path on the server instead.', larger: 'add a larger model from a path on the server' },
});
const advice = (kind) => PATH_ADVICE[kind] || PATH_ADVICE.import;

/**
 * The problem with uploading `size` bytes under `limit` (null: none announced), or null; `kind`
 * (`import` | `model`) words the advice.
 */
export function uploadSizeProblem(size, limit, kind = 'import') {
  if (typeof size !== 'number' || !Number.isFinite(size)) return 'Choose a file.';
  if (size === 0) return 'The file is empty.';
  if (limit !== null && limit !== undefined && size > limit) {
    return `The file has ${bytesText(size)}; the server takes uploads up to ${bytesText(limit)} ([limits] upload_bytes in server.toml). ${advice(kind).instead}`;
  }
  return null;
}

/** The formats an import names (`format`); '' = detected from the content. */
export const IMPORT_FORMATS = Object.freeze(['', 'snapshot', 'store', 'graphson', 'graphml']);

/**
 * The import request from the import form, or the problems: `{upload: {path, size}}` (the file
 * as the body of `POST api/admin/import?<query>`), `{request}` (the JSON path form) or
 * `{problems: [text]}`. Form: `{source: 'upload'|'path'|'inbox', file ({size}), path, inbox,
 * name, base, format, description, role: 'normal'|'base', mcp (true: offer it to MCP; sent
 * only then, otherwise the server's default applies)}`; `limit` = `uploadLimit(info)`.
 * `inbox` is one entry name of the server's import inbox (`inboxRows`): the JSON form with
 * `{inbox}` instead of `{path}` (the server refuses anything but one entry of `imports/`). Only documented
 * parameters are sent, and only when set: `role` only as `base` (the server's default is
 * `normal`), and then without `base` (a base is made only from a plain source, for which the
 * server refuses `base`; a layer source or a missing model is the server's refusal, shown as sent).
 */
export function importRequest(form, limit = null) {
  const f = form || {};
  const problems = [];
  const name = typeof f.name === 'string' ? f.name.trim().toLowerCase() : '';
  const np = nameProblem(name, 'name for the new database');
  if (np) problems.push(np);
  const fields = { name };
  const asBase = f.role === 'base';
  const base = typeof f.base === 'string' ? f.base.trim() : '';
  if (base && !asBase) fields.base = base;
  const format = typeof f.format === 'string' ? f.format : '';
  if (format) {
    if (!IMPORT_FORMATS.includes(format)) problems.push(`Unknown format "${format}".`);
    else fields.format = format;
  }
  const description = typeof f.description === 'string' ? f.description.trim() : '';
  if (description) fields.description = description;
  if (asBase) fields.role = 'base';
  if (f.mcp === true) fields.mcp = true;
  if (f.source === 'inbox') {
    const inbox = typeof f.inbox === 'string' ? f.inbox : '';
    if (!inbox) problems.push('Pick a file of the import inbox.');
    else if (inboxNameProblem(inbox)) problems.push(inboxNameProblem(inbox));
    if (problems.length) return { problems };
    return { request: adminRequest.importPath({ inbox, ...fields }) };
  }
  if (f.source === 'path') {
    const path = typeof f.path === 'string' ? f.path.trim() : '';
    if (!path) problems.push("Enter the source's path on the server.");
    else if (!isAbsolutePath(path)) problems.push('The path must be absolute (it is read on the server).');
    if (problems.length) return { problems };
    return { request: adminRequest.importPath({ path, ...fields }) };
  }
  const size = f.file && typeof f.file.size === 'number' ? f.file.size : NaN;
  const sp = uploadSizeProblem(size, limit);
  if (sp) problems.push(sp);
  if (problems.length) return { problems };
  const query = Object.entries(fields).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
  return { upload: { path: `api/admin/import?${query}`, size } };
}

/** The problem with an inbox entry name (one name in `imports/`, the server's rule), or null. */
export function inboxNameProblem(name) {
  if (typeof name !== 'string' || !name) return 'Pick a file of the import inbox.';
  if (name.startsWith('.') || /[\/\\:\0]/.test(name)) return `"${name}" is not a file of the import inbox.`;
  return null;
}

/** The names of the import kinds an inbox listing guesses (`kind`). */
const INBOX_KINDS = Object.freeze({
  snapshot: 'snapshot (.gsnap)',
  store: 'Store',
  graphson: 'GraphSON',
  graphml: 'GraphML',
});

/**
 * The rows of the import inbox from `GET api/admin/imports` (`{dir, imports: [{name, kind,
 * directory, bytes, modified}]}`): `{dir, rows: [{name, label, kind, kindText, bytes, sizeText,
 * modified, directory, importable}]}`, sorted by name; `label` adds `/` to a directory, a
 * missing kind reads "unknown (detected on import)" (the server still detects the content);
 * malformed entries are left out. A file name suggests the database name (`suggestName`).
 */
export function inboxRows(answer) {
  const a = answer && typeof answer === 'object' ? answer : {};
  const list = Array.isArray(a.imports) ? a.imports : [];
  const rows = list
    .filter((e) => e && typeof e.name === 'string' && !inboxNameProblem(e.name))
    .map((e) => {
      const kind = typeof e.kind === 'string' ? e.kind : null;
      const directory = e.directory === true;
      return {
        name: e.name,
        label: directory ? `${e.name}/` : e.name,
        kind,
        kindText: kind ? (INBOX_KINDS[kind] || kind) : (directory ? 'directory (no Store)' : 'unknown (detected on import)'),
        bytes: typeof e.bytes === 'number' && Number.isFinite(e.bytes) && e.bytes >= 0 ? e.bytes : null,
        sizeText: typeof e.bytes === 'number' ? bytesText(e.bytes) || '0 B' : '',
        modified: typeof e.modified === 'string' ? e.modified : '',
        directory,
        importable: !(directory && !kind),
      };
    })
    .sort((x, y) => (x.name < y.name ? -1 : x.name > y.name ? 1 : 0));
  return { dir: typeof a.dir === 'string' ? a.dir : '', rows };
}

/** Whether a server path is absolute (unix `/...`, Windows `C:\...`, `C:/...` or `\\server\...`). */
export function isAbsolutePath(path) {
  return /^\//.test(path) || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path);
}

/** The text of an upload's progress: `12 MB of 40 MB (30 %)`, or the bytes sent when the total is unknown. */
export function progressText(loaded, total) {
  if (typeof total === 'number' && total > 0) {
    const pct = Math.min(100, Math.floor((loaded / total) * 100));
    return `${bytesText(loaded)} of ${bytesText(total)} (${pct} %)`;
  }
  return `${bytesText(loaded) || '0 B'} sent`;
}

/**
 * Uploads `file` (a Blob/File, or anything `XMLHttpRequest.send` takes) to `upload.path`
 * (`importRequest`'s `upload`) as the raw body with `Content-Type: application/octet-stream`
 * and the host headers, through `XMLHttpRequest` (the only browser API with upload progress).
 * Answers `{promise, abort}`: `promise` resolves to the parsed answer (the import entry) and
 * rejects with an AdminError: the server's `{error, help}`; a `413` without `help` gets the
 * limit's advice; a connection the server closed (it answers an upload declared far above its
 * limit without reading it) names the limit; `abort()` rejects with `aborted: true`.
 * `onProgress(loaded, total)` follows `xhr.upload.onprogress`; `onSent()` fires when the body is
 * sent (the import then runs within the request). `XHR` is injectable (tests). `kind`
 * (`import` | `model`: a model upload, `modelUploadRequest`'s `upload`) words the advice.
 * @param {{base: URL|string, headers?: Object<string,string>, XHR?: Function, limit?: number|null, kind?: string}} options
 */
export function uploadImport({ base, headers = {}, XHR = globalThis.XMLHttpRequest, limit = null, kind = 'import' }, upload, file, { onProgress = () => {}, onSent = () => {} } = {}) {
  const words = advice(kind);
  const capital = (t) => t.charAt(0).toUpperCase() + t.slice(1);
  let xhr = null;
  const promise = new Promise((resolve, reject) => {
    if (typeof XHR !== 'function') {
      reject(new AdminError('This browser cannot upload files (no XMLHttpRequest).'));
      return;
    }
    xhr = new XHR();
    xhr.open('POST', String(new URL(upload.path, base)));
    for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v);
    xhr.setRequestHeader('Accept', 'application/json');
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    if (xhr.upload) {
      xhr.upload.onprogress = (e) => onProgress(e.loaded, e.lengthComputable ? e.total : upload.size);
      xhr.upload.onload = () => onSent();
    }
    xhr.onload = () => {
      const text = typeof xhr.responseText === 'string' ? xhr.responseText : '';
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(text ? JSON.parse(text) : {});
        } catch (e) {
          reject(new AdminError(`The server sent no JSON (HTTP ${xhr.status})`, { status: xhr.status }));
        }
        return;
      }
      const err = parseAdminError(xhr.status, text);
      if (xhr.status === 413 && !err.help) {
        err.help = limit ? `The server takes uploads up to ${bytesText(limit)}; ${words.larger}.` : `${capital(words.larger)}.`;
      }
      reject(err);
    };
    xhr.onerror = () => {
      const tooBig = limit && upload.size > limit;
      reject(new AdminError(
        tooBig
          ? `The server closed the connection: the file is larger than its upload limit (${bytesText(limit)}).`
          : 'The upload failed: the connection was closed before the server answered.',
        { help: tooBig ? words.instead : '' },
      ));
    };
    xhr.onabort = () => {
      const err = new AdminError('The upload was cancelled.');
      err.aborted = true;
      reject(err);
    };
    xhr.send(file);
  });
  return {
    promise,
    abort: () => {
      if (xhr) xhr.abort();
    },
  };
}

/** The model rows of `GET api/admin/models` (`models`), malformed ones skipped. */
export function modelRows(answer) {
  const list = answer && Array.isArray(answer.models) ? answer.models : [];
  const strings = (v) => (Array.isArray(v) ? v.filter((x) => typeof x === 'string' && x) : []);
  return list
    .filter((m) => m && typeof m === 'object' && typeof m.blob === 'string' && m.blob)
    .map((m) => ({
      repo: typeof m.repo === 'string' && m.repo ? m.repo : null,
      identity: typeof m.identity === 'string' ? m.identity : '',
      license: typeof m.license === 'string' ? m.license : '',
      blob: m.blob,
      bytes: typeof m.bytes === 'number' && Number.isFinite(m.bytes) && m.bytes >= 0 ? m.bytes : null,
      users: strings(m.users),
      waiting: strings(m.waiting),
      copied: m.copied === true,
      copiedIn: strings(m.copiedIn),
      linkWarning: typeof m.linkWarning === 'string' ? m.linkWarning : '',
      links: typeof m.links === 'number' ? m.links : null,
      // What `DELETE models/<id>` takes: the repository, else the blob hash.
      id: typeof m.repo === 'string' && m.repo ? m.repo : m.blob,
    }));
}

/** The `needs-model` databases of `GET api/admin/models` (`missing`) as `[{database, model}]`. */
export function missingRows(answer) {
  const list = answer && Array.isArray(answer.missing) ? answer.missing : [];
  return list
    .filter((m) => m && typeof m.database === 'string' && m.database && typeof m.model === 'string' && m.model)
    .map((m) => ({ database: m.database, model: m.model }));
}

/** Whether a model row may be removed: `{enabled, title}` (the server checks the catalog too). */
export function removeModelAction(model) {
  if (model.users.length) return { enabled: false, title: `Used by ${model.users.join(', ')}: delete those databases first` };
  if (model.waiting.length) return { enabled: false, title: `Waited for by ${model.waiting.join(', ')}` };
  return { enabled: true, title: "Delete its blob from the server's models/ directory" };
}

/**
 * The add-model body from the form, or the problems: `{body}` | `{problems}`. Form `{source:
 * 'path'|'repo', path, repo, license}`: a path (absolute, on the server: a Model2Vec model
 * directory or a `.gsnap` carrying model blobs; `repo`/`license` name a directory's model when it
 * does not), or a repository alone (a model `models/` already holds).
 */
export function addModelBody(form) {
  const f = form || {};
  const repo = typeof f.repo === 'string' ? f.repo.trim() : '';
  const license = typeof f.license === 'string' ? f.license.trim() : '';
  if (f.source === 'repo') {
    if (!repo) return { problems: ['Enter the repository (owner/name).'] };
    return { body: { repo } };
  }
  const path = typeof f.path === 'string' ? f.path.trim() : '';
  if (!path) return { problems: ['Enter the path of a model directory or .gsnap on the server.'] };
  if (!isAbsolutePath(path)) return { problems: ['The path must be absolute (it is read on the server).'] };
  const body = { path };
  if (repo) body.repo = repo;
  if (license) body.license = license;
  return { body };
}

/** The formats a model upload names (`format`); '' = detected from the content. */
export const MODEL_UPLOAD_FORMATS = Object.freeze(['', 'gsnap', 'tar']);

/**
 * The model upload format a file name suggests: `gsnap`, `tar` (also a gzip-compressed
 * `.tar.gz`/`.tgz`: the server tells the compression by the content), '' (another name: the
 * server detects it), or null for an archive the server does not take (`.zip`, `.tar.zst`,
 * `.tar.xz`, ...).
 */
export function modelFileFormat(fileName) {
  const name = String(fileName || '').toLowerCase();
  if (name.endsWith('.gsnap')) return 'gsnap';
  if (name.endsWith('.tar') || name.endsWith('.tar.gz') || name.endsWith('.tgz')) return 'tar';
  if (/\.(tar\.[a-z0-9]+|tgz|tbz2?|txz|zip|7z|rar|gz|bz2|xz|zst)$/.test(name)) return null;
  return '';
}

/**
 * The model upload from the add-model form, or the problems: `{upload: {path, size}}` (the
 * file as the body of `POST api/admin/models?<query>`, for `uploadImport` with `kind: 'model'`)
 * or `{problems: [text]}`. Form: `{file ({name, size}), repo, license}`: a `.gsnap` carrying
 * model blobs, or a `.tar`/`.tar.gz` of a Model2Vec model directory (`repo`/`license` name
 * its model when the directory does not); `limit` = `uploadLimit(info)`. `format` is sent when
 * the file name says it; only set parameters are sent.
 */
export function modelUploadRequest(form, limit = null) {
  const f = form || {};
  const problems = [];
  const size = f.file && typeof f.file.size === 'number' ? f.file.size : NaN;
  const sp = uploadSizeProblem(size, limit, 'model');
  if (sp) problems.push(sp);
  const format = f.file ? modelFileFormat(f.file.name) : '';
  if (format === null) problems.push('Upload a .gsnap, or a .tar or .tar.gz of the model directory (other archives are not taken).');
  if (problems.length) return { problems };
  const fields = {};
  const repo = typeof f.repo === 'string' ? f.repo.trim() : '';
  const license = typeof f.license === 'string' ? f.license.trim() : '';
  if (repo) fields.repo = repo;
  if (license) fields.license = license;
  if (format) fields.format = format;
  const query = Object.entries(fields).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
  return { upload: { path: `api/admin/models${query ? `?${query}` : ''}`, size } };
}

/** The hint of a `needs-model` row: how to get it loadable (empty for other states). */
export function needsModelHint(row) {
  if (!row || row.state !== 'needs-model') return '';
  const model = row.model ? `"${row.model}"` : 'its model';
  return `Add ${model} under Models (upload a .gsnap or a .tar of the model directory, or name one on the server; when models/ already holds it: "Give to waiting"); the database then becomes unloaded and can be loaded.`;
}

/**
 * The state text of a listing row with the pool: an unloaded base held in the shared pool by
 * its loaded layers reads `in pool (layers: a, b)`; otherwise the row's `stateText`.
 */
export function poolText(row) {
  if (!row || !row.pooled) return row ? row.stateText : '';
  if (isOpenState(row.state)) return row.stateText;
  return row.pooledBy && row.pooledBy.length ? `in pool (layers: ${row.pooledBy.join(', ')})` : 'in pool';
}

/**
 * The default name of a backup restored into a new database: `<name>-restored`, the database's
 * name cut (and its `..` made one dot) so that the result is a valid new name even for a name
 * made before 0.3.0 (`NAME_MAX`).
 */
export function restoredName(name) {
  const suffix = '-restored';
  const n = String(name || '').toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/\.{2,}/g, '.')
    .slice(0, NAME_MAX - suffix.length).replace(/[._-]+$/, '');
  return n ? `${n}${suffix}` : 'restored';
}

/**
 * A database name suggested from an upload's file name: the extensions of the import sources
 * dropped (`acme.layer.gsnap` -> `acme`), lower case, every other character `-`, `..` made one
 * dot, cut to `NAME_MAX` characters and then to a letter or digit at the end: a valid new name,
 * or '' when nothing usable is left (a Windows device or reserved name). The form still checks
 * it with `nameProblem`.
 */
export function suggestName(fileName) {
  let n = typeof fileName === 'string' ? fileName : '';
  n = n.replace(/^.*[\\/]/, '').toLowerCase();
  n = n.replace(/(\.layer)?\.(gsnap|gstore|graphson|jsonl|json|graphml|xml)$/, '');
  n = n.replace(/[^a-z0-9._-]+/g, '-').replace(/\.{2,}/g, '.').replace(/^[^a-z0-9]+/, '')
    .slice(0, NAME_MAX).replace(/[._-]+$/, '');
  return newNameReason(n) ? '' : n;
}


// ------------------------------------------------------------------ bases with data (P4)

/** What promotion means, said in every confirmation of it (one way: no route turns a base back). */
export const BASE_IS_FROZEN = 'A base is frozen forever: it cannot be written or turned back; layers are built over it.';

/** What a base and its layers are (the Databases tab's help, the promote confirmation). */
export const BASE_HELP = 'A base is a frozen, read-only database that layers are built over. Each layer stores only its own changes; '
  + "the base's data shows through, so many layers share one copy of it (on disk and, while they are loaded, in memory).";

/** When to promote, said in the confirmation and the Databases tab's help. */
export const PROMOTE_HELP = 'Promotion is one way and forever: no action turns a base back into an ordinary database, and nothing writes to it again. '
  + 'Use it when this database is a finished starting point that several databases should build on (reference data, a release, a template): '
  + 'then create them with "New database…", kind "Layer over a base". Clone it first if you still need a copy you can change.';

/** The tooltip of each kind and role the tables show. */
export const KIND_HELP = Object.freeze({
  plain: 'Plain: an ordinary database that stores all its data itself.',
  layer: "Layer: a database over a base; it stores only its own changes, the base's data shows through.",
  base: `Base: ${BASE_HELP}`,
});

/** The tooltip of a row's kind (`kind`, `role`; a database row, a backup or an import), '' when unknown. */
export function kindHelp(row) {
  if (!row || typeof row !== 'object') return '';
  const parts = [];
  if (Object.prototype.hasOwnProperty.call(KIND_HELP, row.kind)) parts.push(KIND_HELP[row.kind]);
  if (row.role === 'base') parts.push(KIND_HELP.base);
  return parts.join('\n');
}

/**
 * Whether a row may be promoted to a base (`POST .../promote`): `{enabled, title}`. Only an
 * UNLOADED plain normal database whose config is readable; every other row is disabled with the
 * reason (the server refuses the same cases with `409` and a help, shown verbatim when reached).
 */
export function promoteAction(row, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  if (row.aliasOf) return off(`An alias of ${row.aliasOf}: use that database's row`);
  if (busy) return off('Wait until the running action ends');
  if (row.state === 'failed' || !row.kind) return off('A failed database cannot be promoted');
  if (row.kind === 'layer') return off('A layer cannot become a base: export it with "materialize" and import the file as a base');
  if (row.role === 'base') return off('Already a base');
  if (isOpenState(row.state)) return off('Unload it first: only an unloaded database can be promoted');
  if (row.state === 'loading') return off('Loading…: unload it once loaded, then promote it');
  if (row.state === 'needs-model') return off('Add its model first: a base cannot get a model later');
  if (row.state !== 'unloaded') return off(`Only an unloaded database can be promoted (it is ${row.state})`);
  return { enabled: true, title: `Turn it into a base. ${BASE_HELP} ${BASE_IS_FROZEN}` };
}

// ------------------------------------------------------------------ the run administration

/** How often the Runs tab reads the runs again while it is shown (milliseconds). */
export const RUNS_POLL_MS = 2000;

/**
 * The runs of a `GET api/admin/runs` or `GET api/admin/databases/<name>/runs` answer as rows
 * `{database, id, client, alias, kind, state, cancelled, startedAtMs, durationMs, text}`, in the
 * order sent (by database, oldest first); malformed ones skipped.
 */
export function runRows(answer) {
  const runs = answer && Array.isArray(answer.runs) ? answer.runs : [];
  return runs
    .filter((r) => r && typeof r === 'object' && typeof r.database === 'string' && Number.isInteger(r.id))
    .map((r) => ({
      database: r.database,
      id: r.id,
      client: typeof r.client === 'string' ? r.client : '',
      alias: typeof r.alias === 'string' ? r.alias : '',
      kind: typeof r.kind === 'string' ? r.kind : '',
      state: typeof r.state === 'string' ? r.state : '',
      cancelled: r.cancelled === true,
      startedAtMs: typeof r.startedAtMs === 'number' ? r.startedAtMs : null,
      durationMs: typeof r.durationMs === 'number' ? r.durationMs : null,
      text: typeof r.text === 'string' ? r.text : '',
    }));
}

/** A duration in milliseconds as text (`850 ms`, `12.3 s`, `4 min 5 s`, `2 h 3 min`), '' when missing. */
export function durationText(ms) {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const total = Math.floor(s);
  if (total < 3600) return `${Math.floor(total / 60)} min ${total % 60} s`;
  return `${Math.floor(total / 3600)} h ${Math.floor((total % 3600) / 60)} min`;
}

/**
 * The query text of a run as shown (one line, white space folded), with `…` when the server cut
 * it at `textHeadChars` characters; '' for a run without a text.
 */
export function runTextHead(run, textHeadChars = 200) {
  const text = run && typeof run.text === 'string' ? run.text : '';
  const line = text.replace(/\s+/g, ' ').trim();
  return [...text].length >= textHeadChars ? `${line}…` : line;
}

/** Whether a run row offers its Stop button, `{enabled, title}`. */
export function cancelRunAction(run, { busy = false } = {}) {
  if (busy) return { enabled: false, title: 'Wait until the running action ends' };
  if (run.cancelled) return { enabled: false, title: 'Already stopping: it ends at its next check' };
  return { enabled: true, title: 'Stop this run, whatever its client (its changes roll back)' };
}

// ---------------------------------------------------------------------------------------------
// Users and grants (the Users tab, ./users-tab.js; book/src/reference/admin_api.md "Users and grants")
// ---------------------------------------------------------------------------------------------

/** The user names the server reserves (grant subjects and the console's principal). */
export const RESERVED_USER_NAMES = Object.freeze(['anonymous', 'operator']);

/** The levels of a grant. */
export const GRANT_LEVELS = Object.freeze(['read', 'write']);

/**
 * How a user's token is shown: `…<last4>` (the server keeps only its hash and its last four
 * characters, so an admin can tell tokens apart), "set" for a token without them (an older
 * file), "none" after a revoke.
 */
export function tokenText(user) {
  if (!user || !user.hasToken) return 'none';
  return typeof user.tokenLast4 === 'string' && user.tokenLast4 ? `…${user.tokenLast4}` : 'set';
}

/**
 * The users of a `GET api/admin/users` answer as sorted rows `[{name, roles, admin, hasToken,
 * tokenLast4, tokenText, grants: [{database, level, kind, target, databaseText}]}]`; a malformed
 * entry is skipped. Throws an
 * Error when the answer has no `users` list.
 */
export function userRows(answer) {
  if (!answer || typeof answer !== 'object' || !Array.isArray(answer.users)) {
    throw new Error('the server answered no user list');
  }
  const rows = [];
  for (const u of answer.users) {
    if (!u || typeof u !== 'object' || typeof u.name !== 'string' || !u.name) continue;
    const roles = Array.isArray(u.roles) ? u.roles.filter((r) => typeof r === 'string' && r) : [];
    const user = {
      name: u.name,
      roles,
      admin: u.admin === true || roles.includes('admin'),
      hasToken: u.hasToken === true,
      tokenLast4: typeof u.tokenLast4 === 'string' && u.tokenLast4 ? u.tokenLast4 : null,
      grants: grantList(u.grants),
    };
    user.tokenText = tokenText(user);
    rows.push(user);
  }
  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  return rows;
}

function grantList(grants) {
  if (!Array.isArray(grants)) return [];
  return grants
    .filter((g) => g && typeof g.database === 'string' && g.database && GRANT_LEVELS.includes(g.level))
    .map((g) => {
      const mark = grantKind(g);
      return { database: g.database, level: g.level, ...mark, databaseText: grantDatabaseText({ database: g.database, ...mark }) };
    });
}

/** The kinds of name a grant's `database` can be now (the server's `kind`). */
export const GRANT_KINDS = Object.freeze(['database', 'alias', 'all', 'missing']);

/**
 * What a grant's `database` names now, from the server's `kind` and `target`: `{kind, target}`,
 * `kind` one of GRANT_KINDS (`*` is `all`; an older server without `kind`: `database`), `target`
 * the alias's current database (else null).
 */
export function grantKind(g) {
  const database = g && typeof g.database === 'string' ? g.database : '';
  if (database === '*') return { kind: 'all', target: null };
  const kind = g && GRANT_KINDS.includes(g.kind) ? g.kind : 'database';
  const target = kind === 'alias' && typeof g.target === 'string' && g.target ? g.target : null;
  return { kind, target };
}

/**
 * The words of a grant's database: `every database` for `*`, `prod → prod-2026-11 (alias)` for
 * an alias (its current target), `old (no such database)` for a name that names nothing now,
 * else the database's name.
 */
export function grantDatabaseText(g) {
  const { kind, target } = grantKind(g);
  const database = g && typeof g.database === 'string' ? g.database : '';
  switch (kind) {
    case 'all': return 'every database';
    case 'alias': return `${database} → ${target || '?'} (alias)`;
    case 'missing': return `${database} (no such database)`;
    default: return database;
  }
}

/** Names in natural order, case-insensitive (`db2` before `db10`), as the tables sort them. */
const nameCollator = typeof Intl !== 'undefined' && Intl.Collator
  ? new Intl.Collator('en', { numeric: true, sensitivity: 'base' })
  : null;

/**
 * The choices of the grant form's database picker from `databaseRows`' rows (round 4: aliases
 * first): the aliases marked with their current target (`cur → acme (alias)`), then the real
 * databases, `[{value, text, kind}]`, each part in natural alphabetical order ("every database"
 * is the form's own last choice).
 */
export function grantChoices(rows) {
  const list = Array.isArray(rows) ? rows : [];
  const byName = (a, b) => (nameCollator
    ? nameCollator.compare(a.value, b.value) || (a.value < b.value ? -1 : a.value > b.value ? 1 : 0)
    : (a.value < b.value ? -1 : a.value > b.value ? 1 : 0));
  const real = realRows(list).map((r) => ({ value: r.name, text: r.name, kind: 'database' })).sort(byName);
  const aliases = list.filter((r) => r.aliasOf)
    .map((r) => ({ value: r.name, text: `${r.name} → ${r.aliasOf} (alias)`, kind: 'alias' })).sort(byName);
  return [...aliases, ...real];
}

/** The words of a grant subject: `everyone (anonymous)`, `<user>`, or the subject as it is. */
export function subjectText(subject) {
  if (subject === 'anonymous') return 'everyone (anonymous)';
  if (typeof subject === 'string' && subject.startsWith('user:')) return subject.slice(5);
  return String(subject || '');
}

/**
 * The grants of a `GET api/admin/users` (or `GET api/admin/grants`) answer as rows
 * `[{subject, subjectText, database, databaseText, kind, target, level}]`: anonymous first, then
 * by subject and database. `databaseText` is grantDatabaseText's (an alias with its current
 * target).
 */
export function grantRows(answer) {
  const grants = answer && Array.isArray(answer.grants) ? answer.grants : [];
  const rows = grants
    .filter((g) => g && typeof g.subject === 'string' && g.subject && typeof g.database === 'string' && g.database && GRANT_LEVELS.includes(g.level))
    .map((g) => {
      const mark = grantKind(g);
      return {
        subject: g.subject,
        subjectText: subjectText(g.subject),
        database: g.database,
        databaseText: grantDatabaseText({ database: g.database, ...mark }),
        kind: mark.kind,
        target: mark.target,
        level: g.level,
      };
    });
  const rank = (g) => (g.subject === 'anonymous' ? 0 : 1);
  rows.sort((a, b) => rank(a) - rank(b)
    || (a.subject < b.subject ? -1 : a.subject > b.subject ? 1 : 0)
    || (a.database < b.database ? -1 : a.database > b.database ? 1 : 0));
  return rows;
}

/** How many users can act as an admin (the admin role and a token): the server's last-admin rule. */
export function usableAdmins(rows) {
  return rows.filter((u) => u.admin && u.hasToken).length;
}

/** The title of an action the last admin cannot take. */
export const LAST_ADMIN_TITLE = 'The last admin: make another user an admin first';

/**
 * What a user row offers, each `{enabled, title}`: newToken, revokeToken, admin (the toggle:
 * give or take the role), remove. The last usable admin can get a new token but cannot be
 * removed, demoted or left without a token (the server refuses it too, with help). `busy` = an
 * action of this row is running.
 */
export function userActions(user, rows, { busy = false } = {}) {
  const off = (title) => ({ enabled: false, title });
  const on = (title) => ({ enabled: true, title });
  if (busy) {
    const why = 'Wait until the running action ends';
    return { newToken: off(why), revokeToken: off(why), admin: off(why), remove: off(why) };
  }
  const last = user.admin && user.hasToken && usableAdmins(rows) <= 1;
  return {
    newToken: on(user.hasToken ? 'Replace the token: the old one stops working at once' : 'Give the user a token'),
    revokeToken: !user.hasToken ? off('The user has no token') : last ? off(LAST_ADMIN_TITLE)
      : on('Take the token away: the user cannot log in until it gets a new one'),
    admin: last ? off(LAST_ADMIN_TITLE)
      : on(user.admin ? 'Take the admin role: only its grants stay' : 'Every database (write), the admin API and these tabs'),
    remove: last ? off(LAST_ADMIN_TITLE) : on('Remove the user and its grants; its sessions end at once'),
  };
}

/** The problem with a new user's name, or null: the server's texts (`newNameReason` without the reserved database names, then its reserved user names). */
export function userNameProblem(name) {
  const n = typeof name === 'string' ? name.trim().toLowerCase() : '';
  if (!n) return 'Enter a user name.';
  const reason = newNameReason(n, { reserved: false });
  if (reason) return invalidNameText(n, reason);
  if (RESERVED_USER_NAMES.includes(n)) return `'${n}': a reserved name (a grant subject or principal of its own)`;
  return null;
}

/** The new user's request from the form `{name, admin}`, or the problems: `{name, admin}` | `{problems}`. */
export function addUserForm(form) {
  const f = form || {};
  const problem = userNameProblem(f.name);
  if (problem) return { problems: [problem] };
  return { name: f.name.trim().toLowerCase(), admin: f.admin === true };
}

/**
 * The grant from the form `{subject, database, level}` (subject `anonymous` or a user name), or
 * the problems: `{subject, database, level}` (subject as the server takes it: `anonymous` or
 * `user:<name>`) | `{problems}`.
 */
export function grantForm(form) {
  const f = form || {};
  const problems = [];
  const who = typeof f.subject === 'string' ? f.subject.trim() : '';
  const database = typeof f.database === 'string' ? f.database.trim() : '';
  if (!who) problems.push('Choose who gets the grant.');
  if (!database) problems.push('Choose a database.');
  if (!GRANT_LEVELS.includes(f.level)) problems.push('Choose read or write.');
  if (problems.length) return { problems };
  const subject = who === 'anonymous' || who.includes(':') ? who : `user:${who}`;
  return { subject, database, level: f.level };
}

/** The confirmation text of a user action, or null. */
export function userConfirmText(action, name, extra = '') {
  switch (action) {
    case 'removeUser':
      return `Remove the user "${name}" and its grants? Its browser and MCP sessions end at once.`;
    case 'newToken':
      return `Give "${name}" a new token? The old one stops working at once and its sessions end.`;
    case 'revokeToken':
      return `Take the token of "${name}" away? It cannot log in until it gets a new one; its sessions end at once.`;
    case 'demote':
      return `Take the admin role from "${name}"? It keeps only its own grants.`;
    case 'revokeGrant':
      return `Revoke the grant of ${subjectText(name)} on ${extra === '*' ? 'every database' : `"${extra}"`}?`;
    default:
      return null;
  }
}

/** How many MCP sessions an answer says ended, as a sentence tail ('' for none). */
function sessionsTail(answer) {
  const n = answer && typeof answer.mcpSessionsEnded === 'number' ? answer.mcpSessionsEnded : 0;
  return n > 0 ? `; ${n} MCP session${n === 1 ? '' : 's'} ended` : '';
}

/** The one-line outcome of a finished user or grant action. */
export function userOutcomeText(action, name, answer = {}) {
  switch (action) {
    case 'addUser': return `Added the user "${answer.name || name}"${answer.admin ? ' (admin)' : ''}. Copy its token now: it is shown only once.`;
    case 'newToken': return `"${answer.name || name}" has a new token${sessionsTail(answer)}. Copy it now: it is shown only once.`;
    case 'revokeToken': return `"${answer.name || name}" has no token now${sessionsTail(answer)}.`;
    case 'setAdmin': return answer.admin ? `"${answer.name || name}" is an admin.` : `"${answer.name || name}" is no longer an admin.`;
    case 'removeUser': {
      const g = typeof answer.grantsRemoved === 'number' ? answer.grantsRemoved : 0;
      return `Removed the user "${answer.name || name}"${g ? ` and ${g} grant${g === 1 ? '' : 's'}` : ''}${sessionsTail(answer)}.`;
    }
    case 'grant': return `${subjectText(answer.subject || name)} can ${answer.level === 'write' ? 'read and write' : 'read'} ${answer.database === '*' ? 'every database' : `"${answer.database}"`}${answer.kind === 'alias' && answer.target ? ` (an alias of "${answer.target}" now: reachable only through "${answer.database}")` : ''}.`;
    case 'revokeGrant': return answer.revoked === false
      ? `${subjectText(answer.subject || name)} had no grant on "${answer.database}".`
      : `Revoked the grant of ${subjectText(answer.subject || name)} on ${answer.database === '*' ? 'every database' : `"${answer.database}"`}.`;
    default: return '';
  }
}

/** The token an answer carries (add, new token) with its user, or null: `{name, token}`. */
export function shownToken(answer) {
  return answer && typeof answer.token === 'string' && answer.token
    ? { name: typeof answer.name === 'string' ? answer.name : '', token: answer.token }
    : null;
}
