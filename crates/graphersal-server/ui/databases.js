// The database list of graphersal-server (pure, tested under Node by ui/tests/databases.test.mjs
// and the admin tests): the rows of `GET api/admin/databases`, its fetch, the host's request
// headers and a browser download of a Blob. Moved here from the engine's playground
// (js/server-mode.js), where the landing view used it.
//
// `GET api/admin/databases` (docs/admin-api.md):
//   `{"databases": [{"name", "state", "kind", "role", "readOnly", "autoLoad", "description",
//   "base"?, "created"?, "aliasOf"?, "reason"?, "model"?, "damage"? (state `maintenance`: the
//   damage report)}], "aliases": {alias: database},
//   "aliasesRevision", "problems": [text]}` with `state` one of `DATABASE_STATES`; each entry
//   also carries its settings' `revision`. Checked by `databaseRows`; a
//   malformed entry is skipped and counted, never shown half. The optional byte counts
//   `sizeBytes` (on disk) and `memoryBytes` (when loaded; `null` when not: `memoryReported`
//   tells a server that sends the field from an older one) with a layer's `sharedMemoryBytes`
//   (its base's memory, shared) are shown when the server sends them;
//   `pooled` + `pooledBy` (a base held in the shared pool by its loaded layers) and
//   `linkWarning` (model blobs copied, not hard-linked) are optional too.

import { aliasRows, bytesText, exactBytes, isOpenState } from './admin-api.js';

/** The database states of the server, in the words the list shows. */
export const DATABASE_STATES = Object.freeze({
  unloaded: 'unloaded',
  loading: 'loading…',
  loaded: 'loaded',
  'needs-model': 'needs a model',
  failed: 'failed',
  maintenance: 'maintenance (read-only, damaged)',
});

/** The badge tone (CSS class `badge <tone>`) of each state; unknown states get none. */
export const STATE_TONES = Object.freeze({
  unloaded: '',
  loading: 'accent',
  loaded: 'ok',
  'needs-model': 'warn',
  failed: 'danger',
  maintenance: 'danger',
});

/** How often the list reloads while a database is loading (milliseconds). */
export const LANDING_POLL_MS = 2000;

/** The page of database `name`, relative to the server root: `db/<name>/` (name percent-encoded). */
export function databaseHref(name) {
  return `db/${encodeURIComponent(name)}/`;
}

/**
 * The OpenAPI description of database `name`'s HTTP API (queries, schema, saved queries, ...),
 * relative to the server root: `db/<name>/openapi.json`. A client generator reads it; the
 * server answers it to every principal that reaches the name, also while it is not loaded.
 */
export function databaseOpenapiHref(name) {
  return `${databaseHref(name)}openapi.json`;
}

/** The OpenAPI description of the server's own HTTP API (login, the caller's databases, the
 * admin API), relative to the server root. */
export const SERVER_OPENAPI_HREF = 'openapi.json';

/**
 * The list's rows from a `GET /api/admin/databases` answer:
 * `{rows: [{name, href, state, stateText, tone, kind, role, readOnly, autoLoad, description,
 * base, created, aliasOf, reason, model, sizeBytes, memoryBytes, memoryReported, sharedMemoryBytes,
 * pooled, pooledBy, linkWarning, revision}], skipped, aliases: [{alias, target}], aliasesRevision, problems: [text]}`, sorted by
 * name (`revision`, `aliasesRevision`: what a change of the row's settings or of the aliases
 * sends, '' from an older server); `skipped` counts the
 * malformed entries. An unknown `state` is shown as it is (a newer server). Throws an Error when
 * the answer has no `databases` list.
 */
export function databaseRows(answer) {
  if (!answer || typeof answer !== 'object' || !Array.isArray(answer.databases)) {
    throw new Error('the server answered no database list');
  }
  const rows = [];
  let skipped = 0;
  for (const d of answer.databases) {
    if (!d || typeof d !== 'object' || typeof d.name !== 'string' || !d.name || typeof d.state !== 'string') {
      skipped++;
      continue;
    }
    rows.push({
      name: d.name,
      href: databaseHref(d.name),
      state: d.state,
      stateText: Object.prototype.hasOwnProperty.call(DATABASE_STATES, d.state) ? DATABASE_STATES[d.state] : d.state,
      tone: Object.prototype.hasOwnProperty.call(STATE_TONES, d.state) ? STATE_TONES[d.state] : '',
      kind: typeof d.kind === 'string' ? d.kind : '',
      role: typeof d.role === 'string' ? d.role : '',
      readOnly: d.readOnly === true,
      autoLoad: d.autoLoad === true,
      description: typeof d.description === 'string' ? d.description : '',
      base: typeof d.base === 'string' && d.base ? d.base : null,
      created: typeof d.created === 'string' ? d.created : '',
      aliasOf: typeof d.aliasOf === 'string' && d.aliasOf ? d.aliasOf : null,
      reason: typeof d.reason === 'string' ? d.reason : '',
      damage: typeof d.damage === 'string' ? d.damage : '',
      model: typeof d.model === 'string' ? d.model : '',
      sizeBytes: byteCount(d.sizeBytes),
      memoryBytes: byteCount(d.memoryBytes),
      memoryReported: Object.prototype.hasOwnProperty.call(d, 'memoryBytes'),
      sharedMemoryBytes: byteCount(d.sharedMemoryBytes),
      pooled: d.pooled === true,
      pooledBy: Array.isArray(d.pooledBy) ? d.pooledBy.filter((x) => typeof x === 'string' && x) : [],
      linkWarning: typeof d.linkWarning === 'string' ? d.linkWarning : '',
      revision: typeof d.revision === 'string' ? d.revision : '',
    });
  }
  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const problems = Array.isArray(answer.problems) ? answer.problems.filter((p) => typeof p === 'string' && p) : [];
  const aliasesRevision = typeof answer.aliasesRevision === 'string' ? answer.aliasesRevision : '';
  return { rows, skipped, aliases: aliasRows(answer), aliasesRevision, problems };
}

function byteCount(n) {
  return typeof n === 'number' && Number.isFinite(n) && n >= 0 ? n : null;
}

/**
 * The sortable size columns of the database table (./sort.js definitions): `size` (on disk) and
 * `memory` (in memory; a row that is not loaded has none and sorts last both ways).
 */
export const SIZE_COLUMNS = Object.freeze({
  size: Object.freeze({ type: 'number', value: (r) => r.sizeBytes }),
  memory: Object.freeze({ type: 'number', value: (r) => (isOpenState(r.state) ? r.memoryBytes : null) }),
});

/** The tooltip of a row's "On disk" cell: the exact bytes and what they count. */
export function diskTitle(row) {
  if (!row || row.sizeBytes === null) return '';
  return `${exactBytes(row.sizeBytes)} in the database's directory (Store snapshots, journal, models)`;
}

/** The "In memory" cell of a row: '' when it is not loaded (or not measured). */
export function memoryText(row) {
  return row && isOpenState(row.state) && row.memoryBytes !== null ? bytesText(row.memoryBytes) : '';
}

/** The tooltip of a row's "In memory" cell: the exact bytes; a layer adds its shared base. */
export function memoryTitle(row) {
  if (!memoryText(row)) return '';
  const own = exactBytes(row.memoryBytes);
  if (row.sharedMemoryBytes !== null) {
    return `${own}: this layer's own changes; its base "${row.base || ''}" takes ${bytesText(row.sharedMemoryBytes)} (${exactBytes(row.sharedMemoryBytes)}), shared with the base and its other layers`;
  }
  return `${own}: the graph's data in memory (the engine's estimate; loaded embedding models not included)`;
}

/**
 * The memory summary of `GET api/admin/server` (its `memory`, docs/admin-api.md): rows
 * `[{key, label, text, title}]` in the order process, databases, machine, limit, free; a
 * figure the server does not know reads "unknown". `[]` without a `memory` object (an older
 * server).
 */
export function memoryRows(server) {
  const m = server && typeof server === 'object' ? server.memory : null;
  if (!m || typeof m !== 'object') return [];
  const n = (key) => byteCount(m[key]);
  const shown = (v) => (v === null ? 'unknown' : bytesText(v));
  const exact = (v) => (v === null ? '' : exactBytes(v));
  const rows = [];
  const process = n('processBytes');
  rows.push({
    key: 'process', label: 'Process', text: shown(process),
    title: process === null
      ? 'The resident memory of the server process is not known on this platform.'
      : `${exact(process)}: the resident memory (RSS) of the server process, every database and the engine included`,
  });
  const dbs = n('databasesBytes');
  rows.push({
    key: 'databases', label: 'Loaded databases', text: shown(dbs),
    title: `${exact(dbs)}: the graphs of the loaded databases, each once (a base shared by layers counts one time); the engine's estimate`,
  });
  const total = n('machineTotalBytes');
  const avail = n('machineAvailableBytes');
  rows.push({
    key: 'machine', label: 'Machine', text: `${shown(avail)} available of ${shown(total)}`,
    title: `available ${exact(avail) || 'unknown'}, total ${exact(total) || 'unknown'} (the machine's physical memory; inside a container the host's)`,
  });
  const limit = n('limitBytes');
  const source = typeof m.limitSource === 'string' ? m.limitSource : '';
  const configured = m.configuredLimit;
  const setting = typeof configured === 'number'
    ? `memory_limit = ${configured}`
    : typeof configured === 'string' ? `memory_limit = "${configured}"` : 'memory_limit not set ("auto")';
  const headroom = n('headroomBytes');
  const why = { configured: 'set in server.toml', cgroup: "the container's cgroup limit minus the headroom", physical: "80 % of the machine's memory" }[source] || '';
  rows.push({
    key: 'limit', label: 'Memory limit',
    text: limit === null ? 'none' : `${bytesText(limit)}${why ? ` (${why})` : ''}`,
    title: `${limit === null ? 'No ceiling: the process may grow until the system stops it.' : `${exact(limit)}: the engine's process memory ceiling; a query, import or write that would go over it is refused.`} server.toml [limits] ${setting}${headroom !== null ? `, memory_headroom = ${headroom}` : ''}.`,
  });
  const free = n('freeBytes');
  const basis = ['limit', 'machine-below-limit'].includes(m.freeBasis) ? m.freeBasis : 'machine';
  const freeWords = {
    limit: ['under the limit', "the limit minus the process's memory"],
    'machine-below-limit': ['on the machine (less than the limit leaves)', "the machine's available memory: less than the limit minus the process's memory"],
    machine: ['on the machine', "no limit is set, so the machine's available memory"],
  }[basis];
  rows.push({
    key: 'free', label: 'Free',
    text: free === null ? 'unknown' : `${bytesText(free)} ${freeWords[0]}`,
    title: free === null ? '' : `${exact(free)}: ${freeWords[1]}`,
  });
  return rows;
}

/** Whether the list should be reloaded soon (a database is loading). */
export function anyLoading(rows) {
  return rows.some((r) => r.state === 'loading');
}

/**
 * Fetches the database list: `GET <base>api/admin/databases` with the host's headers; resolves
 * to `databaseRows`' answer, rejects with the server's error text (an Error whose `help` is the
 * server's `help`, when it sent one, with `status` and the `login` page of a 401);
 * `onRefusal(err)` sees the error first (as in ./admin-api.js `sendAdmin`).
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>, onRefusal?: Function}} options
 */
export async function fetchDatabases({ base, fetch, headers = {}, onRefusal = null }) {
  const r = await fetch(new URL('api/admin/databases', base), {
    cache: 'no-store',
    headers: { ...headers, Accept: 'application/json' },
  });
  const text = await r.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch (e) {
    throw new Error(`The server sent no JSON (HTTP ${r.status})`);
  }
  if (!r.ok) {
    const err = new Error((data && data.error) || `HTTP ${r.status}`);
    err.help = data && typeof data.help === 'string' ? data.help : '';
    err.status = r.status;
    err.login = data && typeof data.login === 'string' ? data.login : '';
    if (typeof onRefusal === 'function') {
      try {
        onRefusal(err);
      } catch {
        // The hook only reacts; the caller gets the error.
      }
    }
    throw err;
  }
  return databaseRows(data);
}

/**
 * The request headers a host page asks for (`window.GRAPHERSAL_ENGINE.headers`, set by a classic
 * script before the playground's modules; the playground's engine sends them too): a copy of the
 * entries whose value is a text, or none. The admin requests carry them like the engine's.
 */
export function hostHeaders(scope = globalThis) {
  const config = scope && scope.GRAPHERSAL_ENGINE;
  const headers = config && config.headers && typeof config.headers === 'object' ? config.headers : {};
  const out = {};
  for (const [name, value] of Object.entries(headers)) {
    if (typeof value === 'string' && !/[\r\n]/.test(value)) out[name] = value;
  }
  return out;
}

/** Saves `blob` as `fileName` through a temporary link (the browser's download). */
export function downloadBlob(blob, fileName) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}

/**
 * The root `GET /api/info` answer as the server sent it (its `uploadBytes`, `mcp`), from a
 * profile module's `api` (`api.info.host`, delivered by the playground); `{}` when the
 * playground passes none.
 */
export function hostInfo(api) {
  const host = api && api.info && api.info.host;
  return host && typeof host === 'object' ? host : {};
}
