// The database list of graphersal-server (pure, tested under Node by ui/tests/databases.test.mjs
// and the admin tests): the rows of `GET api/admin/databases`, its fetch, the host's request
// headers and a browser download of a Blob. Moved here from the engine's playground
// (js/server-mode.js), where the landing view used it.
//
// `GET api/admin/databases` (docs/admin-api.md):
//   `{"databases": [{"name", "state", "kind", "role", "readOnly", "autoLoad", "description",
//   "base"?, "created"?, "aliasOf"?, "reason"?, "model"?}], "aliases": {alias: database},
//   "problems": [text]}` with `state` one of `DATABASE_STATES`. Checked by `databaseRows`; a
//   malformed entry is skipped and counted, never shown half. The optional byte counts
//   `sizeBytes` (on disk) and `memoryBytes` (when loaded) are shown when the server sends them;
//   `pooled` + `pooledBy` (a base held in the shared pool by its loaded layers) and
//   `linkWarning` (model blobs copied, not hard-linked) are optional too.

import { aliasRows } from './admin-api.js';

/** The database states of the server, in the words the list shows. */
export const DATABASE_STATES = Object.freeze({
  unloaded: 'unloaded',
  loading: 'loading…',
  loaded: 'loaded',
  'needs-model': 'needs a model',
  failed: 'failed',
});

/** The badge tone (CSS class `badge <tone>`) of each state; unknown states get none. */
export const STATE_TONES = Object.freeze({
  unloaded: '',
  loading: 'accent',
  loaded: 'ok',
  'needs-model': 'warn',
  failed: 'danger',
});

/** How often the list reloads while a database is loading (milliseconds). */
export const LANDING_POLL_MS = 2000;

/** The page of database `name`, relative to the server root: `db/<name>/` (name percent-encoded). */
export function databaseHref(name) {
  return `db/${encodeURIComponent(name)}/`;
}

/**
 * The list's rows from a `GET /api/admin/databases` answer:
 * `{rows: [{name, href, state, stateText, tone, kind, role, readOnly, autoLoad, description,
 * base, created, aliasOf, reason, model, sizeBytes, memoryBytes, pooled, pooledBy, linkWarning}],
 * skipped, aliases: [{alias, target}], problems: [text]}`, sorted by name; `skipped` counts the
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
      model: typeof d.model === 'string' ? d.model : '',
      sizeBytes: byteCount(d.sizeBytes),
      memoryBytes: byteCount(d.memoryBytes),
      pooled: d.pooled === true,
      pooledBy: Array.isArray(d.pooledBy) ? d.pooledBy.filter((x) => typeof x === 'string' && x) : [],
      linkWarning: typeof d.linkWarning === 'string' ? d.linkWarning : '',
    });
  }
  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const problems = Array.isArray(answer.problems) ? answer.problems.filter((p) => typeof p === 'string' && p) : [];
  return { rows, skipped, aliases: aliasRows(answer), problems };
}

function byteCount(n) {
  return typeof n === 'number' && Number.isFinite(n) && n >= 0 ? n : null;
}

/** Whether the list should be reloaded soon (a database is loading). */
export function anyLoading(rows) {
  return rows.some((r) => r.state === 'loading');
}

/**
 * Fetches the database list: `GET <base>api/admin/databases` with the host's headers; resolves
 * to `databaseRows`' answer, rejects with the server's error text (an Error whose `help` is the
 * server's `help`, when it sent one).
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>}} options
 */
export async function fetchDatabases({ base, fetch, headers = {} }) {
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

/** The root `GET api/info` answer (its `uploadBytes`), or `{}` when it cannot be read. */
export async function fetchRootInfo({ base, fetch, headers = {} }) {
  try {
    const r = await fetch(new URL('api/info', base), { cache: 'no-store', headers: { ...headers, Accept: 'application/json' } });
    if (!r.ok) return {};
    const info = JSON.parse(await r.text());
    return info && typeof info === 'object' ? info : {};
  } catch (e) {
    return {};
  }
}
