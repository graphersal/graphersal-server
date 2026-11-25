// The admin API of graphersal-server (pure, tested under Node by ui/tests/admin-*.test.mjs):
// request builders, the client, error parsing, form checks, the actions a row offers and the
// poll of a load. The views are the `server` UI profile's tabs (ui/databases-tab.js,
// ui/backups-tab.js, ui/export-tab.js, ui/import-tab.js, ui/models-tab.js); moved here from the
// engine's playground, where it was the landing view's admin panel.
//
// The contract is docs/admin-api.md (EXPERIMENTAL): every route below `api/admin/`, JSON bodies
// with `Content-Type: application/json` (unknown fields are refused, so the builders send only
// the documented ones), errors `{"error": <sentence>, "help"?: <text>}` shown verbatim
// (`AdminError`). Actions take REAL database names; an alias is changed only through the alias
// routes. Paths are relative to the server root (the base the client is created with).

/** The database name rule of the server (lower case; names are answered in lower case). */
export const NAME_RULE = /^[a-z0-9][a-z0-9._-]{0,62}$/;

/** How often a started load is polled (milliseconds). */
export const LOAD_POLL_MS = 1000;

/** How long a load is polled before the panel stops watching it (it keeps running). */
export const LOAD_POLL_LIMIT_MS = 10 * 60 * 1000;

/**
 * An admin API refusal or failure: `message` = the server's `error`, `help` its `help`, `login`
 * the login page a `401` names (`{"error", "login": "/"}`), else ''.
 */
export class AdminError extends Error {
  constructor(message, { status = 0, help = '', state = '', login = '' } = {}) {
    super(message);
    this.name = 'AdminError';
    this.status = status;
    this.help = help;
    this.state = state;
    this.login = login;
  }
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
  return new AdminError(error, { status, help, state, login });
}

const seg = (name) => encodeURIComponent(String(name));

/**
 * The request of one admin action: `{method, path, body?}` with `path` relative to the server
 * root. Actions: list, get, create(body), delete, clone(newName), load, unload,
 * readOnly(value), autoLoad(value), aliases, setAlias(alias, target), removeAlias(alias),
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
 * database). The transfers that are not JSON requests are
 * `exportPath` (a download) and `uploadImport` (the file as the body: an import, or a model
 * from `modelUploadRequest`).
 */
export const adminRequest = Object.freeze({
  list: () => ({ method: 'GET', path: 'api/admin/databases' }),
  get: (name) => ({ method: 'GET', path: `api/admin/databases/${seg(name)}` }),
  create: (body) => ({ method: 'POST', path: 'api/admin/databases', body }),
  delete: (name) => ({ method: 'DELETE', path: `api/admin/databases/${seg(name)}` }),
  clone: (name, newName) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/clone`, body: { name: newName } }),
  load: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/load` }),
  unload: (name) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/unload` }),
  readOnly: (name, value) => ({ method: 'PUT', path: `api/admin/databases/${seg(name)}/read-only`, body: { readOnly: !!value } }),
  autoLoad: (name, value) => ({ method: 'PUT', path: `api/admin/databases/${seg(name)}/auto-load`, body: { autoLoad: !!value } }),
  aliases: () => ({ method: 'GET', path: 'api/admin/aliases' }),
  setAlias: (alias, target) => ({ method: 'PUT', path: `api/admin/aliases/${seg(alias)}`, body: { target } }),
  removeAlias: (alias) => ({ method: 'DELETE', path: `api/admin/aliases/${seg(alias)}` }),
  renameAlias: (alias, newName) => ({ method: 'POST', path: `api/admin/aliases/${seg(alias)}/rename`, body: { name: newName } }),
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
  removeUser: (name) => ({ method: 'DELETE', path: `api/admin/users/${seg(name)}` }),
  newToken: (name) => ({ method: 'POST', path: `api/admin/users/${seg(name)}/token` }),
  revokeToken: (name) => ({ method: 'DELETE', path: `api/admin/users/${seg(name)}/token` }),
  setAdmin: (name, value) => ({ method: 'PUT', path: `api/admin/users/${seg(name)}/admin`, body: { admin: !!value } }),
  grant: (subject, database, level) => ({ method: 'POST', path: 'api/admin/grants', body: { subject, database, level } }),
  revokeGrant: (subject, database) => ({ method: 'DELETE', path: `api/admin/grants/${seg(subject)}/${seg(database)}` }),
  runs: (name) => ({ method: 'GET', path: name ? `api/admin/databases/${seg(name)}/runs` : 'api/admin/runs' }),
  cancelRun: (name, id) => ({ method: 'POST', path: `api/admin/databases/${seg(name)}/runs/cancel`, body: { id } }),
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

/** The problem with a database or alias name, or null. */
export function nameProblem(name, what = 'name') {
  const n = typeof name === 'string' ? name.trim() : '';
  if (!n) return `Enter a ${what}.`;
  if (!NAME_RULE.test(n.toLowerCase())) {
    return `A ${what} starts with a letter or digit and has at most 63 of a-z, 0-9, '.', '_', '-'.`;
  }
  return null;
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
 * text, optional), readOnly, autoLoad}`. Only the documented fields are sent, and only the ones
 * that differ from the server's defaults (a layer sends `kind` + `base`, no `role`).
 */
export function createBody(form) {
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
  // A base is always read-only on the server; the flag is sent only for a normal database.
  if (f.readOnly === true && body.role !== 'base') body.readOnly = true;
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
  if (row.aliasOf) {
    const why = `An alias of ${row.aliasOf}: change it under Aliases`;
    return { load: off(why), unload: off(why), clone: off(why), delete: off(why), readOnly: off(why), autoLoad: off(why) };
  }
  if (busy) {
    const why = 'Wait until the running action ends';
    return { load: off(why), unload: off(why), clone: off(why), delete: off(why), readOnly: off(why), autoLoad: off(why) };
  }
  const configless = !row.kind; // a failed database whose database.toml cannot be read
  const s = row.state;
  return {
    load: s === 'loaded' || s === 'loading'
      ? off(s === 'loaded' ? 'Already loaded' : 'Loading…')
      : configless ? off('Its configuration cannot be read') : on('Load it into memory and serve it'),
    unload: s === 'loaded' ? on('Close it; running requests get up to 30 s') : off('Not loaded'),
    clone: configless || s === 'failed'
      ? off('A failed database cannot be cloned')
      : on('Copy its latest state into a new database'),
    delete: s === 'loaded' || s === 'loading'
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

/** The confirmation text of a destructive action, or null (no confirmation needed). */
export function confirmText(action, subject, extra = '') {
  switch (action) {
    case 'delete':
      return `Delete the database "${subject}"? Its directory moves to the server's trash/ directory (recoverable by hand only).`;
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
      return `Promote "${subject}" to a base? ${BASE_IS_FROZEN}`;
    case 'cancelRun':
      return `Stop run ${extra} of "${subject}"? Its query ends at its next check and its changes roll back; its client gets an error.`;
    default:
      return null;
  }
}

/** The one-line outcome of a finished action (shown as a notice). */
export function outcomeText(action, subject, answer = {}) {
  switch (action) {
    case 'create': return `Created "${subject}" (unloaded).`;
    case 'clone': return `Cloned into "${subject}" (unloaded).`;
    case 'delete': return `Deleted "${subject}"${answer.trash ? `; it was moved to ${answer.trash}` : ''}.`;
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
  if (row.state === 'loaded' || row.state === 'loading') return { enabled: false, title: 'Unload it first (or restore into a new name)' };
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
 * name, base, format, description, role: 'normal'|'base'}`; `limit` = `uploadLimit(info)`.
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
 * directory, bytes, modified}]}`): `{dir, rows: [{name, label, kind, kindText, sizeText,
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
  if (row.state === 'loaded') return row.stateText;
  return row.pooledBy && row.pooledBy.length ? `in pool (layers: ${row.pooledBy.join(', ')})` : 'in pool';
}

/**
 * A database name suggested from an upload's file name: the extensions of the import sources
 * dropped (`acme.layer.gsnap` -> `acme`), lower case, every other character `-`; '' when nothing
 * usable is left. Only a suggestion: the form checks it with `nameProblem`.
 */
export function suggestName(fileName) {
  let n = typeof fileName === 'string' ? fileName : '';
  n = n.replace(/^.*[\\/]/, '').toLowerCase();
  n = n.replace(/(\.layer)?\.(gsnap|gstore|graphson|jsonl|json|graphml|xml)$/, '');
  n = n.replace(/[^a-z0-9._-]+/g, '-').replace(/^[^a-z0-9]+/, '').slice(0, 63).replace(/[._-]+$/, '');
  return NAME_RULE.test(n) ? n : '';
}


// ------------------------------------------------------------------ bases with data (P4)

/** What promotion means, said in every confirmation of it (one way: no route turns a base back). */
export const BASE_IS_FROZEN = 'A base is frozen forever: it cannot be written or turned back; layers are built over it.';

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
  if (row.state === 'loaded') return off('Unload it first: only an unloaded database can be promoted');
  if (row.state === 'loading') return off('Loading…: unload it once loaded, then promote it');
  if (row.state === 'needs-model') return off('Add its model first: a base cannot get a model later');
  if (row.state !== 'unloaded') return off(`Only an unloaded database can be promoted (it is ${row.state})`);
  return { enabled: true, title: `Turn it into a base. ${BASE_IS_FROZEN}` };
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
// Users and grants (the Users tab, ./users-tab.js; docs/admin-api.md "Users and grants")
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
 * tokenLast4, tokenText, grants: [{database, level}]}]`; a malformed entry is skipped. Throws an
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
    .map((g) => ({ database: g.database, level: g.level }));
}

/** The words of a grant subject: `everyone (anonymous)`, `<user>`, or the subject as it is. */
export function subjectText(subject) {
  if (subject === 'anonymous') return 'everyone (anonymous)';
  if (typeof subject === 'string' && subject.startsWith('user:')) return subject.slice(5);
  return String(subject || '');
}

/**
 * The grants of a `GET api/admin/users` (or `GET api/admin/grants`) answer as rows
 * `[{subject, subjectText, database, databaseText, level}]`: anonymous first, then by subject
 * and database. `*` reads "every database".
 */
export function grantRows(answer) {
  const grants = answer && Array.isArray(answer.grants) ? answer.grants : [];
  const rows = grants
    .filter((g) => g && typeof g.subject === 'string' && g.subject && typeof g.database === 'string' && g.database && GRANT_LEVELS.includes(g.level))
    .map((g) => ({
      subject: g.subject,
      subjectText: subjectText(g.subject),
      database: g.database,
      databaseText: g.database === '*' ? 'every database' : g.database,
      level: g.level,
    }));
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

/** The problem with a new user's name, or null. */
export function userNameProblem(name) {
  const n = typeof name === 'string' ? name.trim().toLowerCase() : '';
  if (!n) return 'Enter a user name.';
  if (!NAME_RULE.test(n)) return "A user name starts with a letter or digit and has at most 63 of a-z, 0-9, '.', '_', '-'.";
  if (RESERVED_USER_NAMES.includes(n)) return `"${n}" is reserved: choose another name.`;
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
    case 'grant': return `${subjectText(answer.subject || name)} can ${answer.level === 'write' ? 'read and write' : 'read'} ${answer.database === '*' ? 'every database' : `"${answer.database}"`}.`;
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
