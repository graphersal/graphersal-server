// The state the `server` profile's tabs share (one module instance per page: every tab imports
// this file): the server root, the host's request headers, the admin client and the database
// list with its reload (polled while a database is loading). Imports only `vue` and the
// profile's own files.

import { reactive } from 'vue';
import { createAdmin } from './admin-api.js';
import {
  fetchDatabases, anyLoading, databaseRows, hostHeaders, LANDING_POLL_MS,
} from './databases.js';

/** The server root: the profile's files are served under `<root>/server-ui/`. Relative, so a
 * proxy prefix in front of the server keeps working. */
export const ROOT = new URL('../', import.meta.url);

/** The host's request headers (`window.GRAPHERSAL_ENGINE.headers`), sent with every request. */
export const headers = hostHeaders();

const fetchFn = (...args) => globalThis.fetch(...args);

/** The admin client (model: ./admin-api.js). */
export const admin = createAdmin({ base: ROOT, fetch: fetchFn, headers });

/** A notice of a failure: the server's `error` and `help` verbatim. */
export const errorNotice = (err) => ({
  tone: 'error',
  text: err && err.message ? err.message : String(err),
  help: err && typeof err.help === 'string' ? err.help : '',
});

/** The CSS classes of a notice by its tone. */
export const noticeClass = (n) => (n.tone === 'ok' ? 'info' : n.tone === 'warn' ? 'srv-warn-notice' : 'srv-error');

/**
 * The database list: rows, aliases, problems, the count of malformed entries, the read error,
 * and `version` (bumped after every reload that leaves nothing loading: the Models tab reads
 * the models again).
 */
export const list = reactive({
  rows: [], aliases: [], problems: [], skipped: 0, error: null, loading: false, loaded: false, version: 0,
});

let timer = null;
let pending = null;

/** Reads the database list again (one read at a time; polled while a database is loading). */
export function reload() {
  if (pending) return pending;
  pending = (async () => {
    list.loading = true;
    try {
      const answer = await fetchDatabases({ base: ROOT, fetch: fetchFn, headers });
      list.rows = answer.rows;
      list.aliases = answer.aliases;
      list.problems = answer.problems;
      list.skipped = answer.skipped;
      list.error = null;
    } catch (err) {
      list.error = errorNotice(err);
    } finally {
      list.loading = false;
      list.loaded = true;
    }
    if (!anyLoading(list.rows)) list.version++;
    clearTimeout(timer);
    if (anyLoading(list.rows)) timer = setTimeout(reload, LANDING_POLL_MS);
  })().finally(() => {
    pending = null;
  });
  return pending;
}

/** The list once read (reads it when no tab has yet). */
export function ensureLoaded() {
  return list.loaded ? Promise.resolve() : reload();
}

/** Puts an entry answered by an action into the list at once (the reload follows). */
export function patchRow(entry) {
  if (!entry || typeof entry.name !== 'string') return;
  const [row] = databaseRows({ databases: [entry] }).rows;
  if (!row) return;
  list.rows = list.rows.map((r) => (r.name === row.name && !r.aliasOf ? row : r));
}

/** The link to the playground of a list row (`db/<name>/` on the server root). */
export const pageHref = (row) => new URL(row.href, ROOT).href;
