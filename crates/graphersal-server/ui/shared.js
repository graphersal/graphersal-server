// The state the `server` profile's tabs share (one module instance per page: every tab imports
// this file): the server root, the host's request headers, the admin client and the database
// list with its reload (polled while a database is loading), and what a refused request does
// (401: the login page; 404 of the admin API: the root again when the principal lost the admin
// role), the fresh data of every tab on every visit (`useFreshData`, model ./fresh.js) and the
// sortable table columns (`useSort`, `SortTh`, model ./sort.js). Imports only `vue` and the
// profile's own files.

import { reactive, onMounted, onActivated, onDeactivated, onUnmounted } from 'vue';
import { createFreshness, listenForReturn } from './fresh.js';
import {
  sortRows, toggleSort, sortMark, ariaSort, loadSort, saveSort,
} from './sort.js';
import { createAdmin, sendAdmin, refusalNotice } from './admin-api.js';
import {
  accessRequest, loginHref, principalOf, refusalAction, rootHref,
} from './access-api.js';
import {
  fetchDatabases, anyLoading, databaseRows, hostHeaders, LANDING_POLL_MS,
  databaseOpenapiHref, SERVER_OPENAPI_HREF,
} from './databases.js';

/** The server root: the profile's files are served under `<root>/server-ui/`. Relative, so a
 * proxy prefix in front of the server keeps working. */
export const ROOT = new URL('../', import.meta.url);

/** The host's request headers (`window.GRAPHERSAL_ENGINE.headers`), sent with every request. */
export const headers = hostHeaders();

const fetchFn = (...args) => globalThis.fetch(...args);

let leaving = false;
let rechecking = null;

/** Loads `href` (once: the first refusal wins). */
function leaveTo(href) {
  if (leaving) return;
  leaving = true;
  globalThis.location.assign(href);
}

/**
 * Reacts to a refused request (./access-api.js `refusalAction`): a 401 that names the login
 * page loads it (the session ended); a 404 reads the root info again and loads the root when
 * the principal is no admin any more (the server then answers its own tabs).
 */
export function handleRefusal(err) {
  const action = refusalAction(err);
  if (action === 'login') {
    leaveTo(loginHref(err.login, ROOT));
  } else if (action === 'recheck' && !rechecking) {
    rechecking = sendAdmin({ base: ROOT, fetch: fetchFn, headers }, accessRequest.info())
      .then((info) => {
        if (!principalOf(info).admin) leaveTo(rootHref(ROOT));
      })
      .catch(() => {})
      .finally(() => {
        rechecking = null;
      });
  }
}

/** The request options of every admin and access request: refusals go to `handleRefusal`. */
export const requestOptions = Object.freeze({ base: ROOT, fetch: fetchFn, headers, onRefusal: handleRefusal });

/** The admin client (model: ./admin-api.js). */
export const admin = createAdmin(requestOptions);

/** A notice of a failure: the server's `error` and `help` verbatim (a refused stale change: a
 * warning saying the tab read its data again; ./admin-api.js `refusalNotice`). */
export const errorNotice = (err) => refusalNotice(err);

/** The CSS classes of a notice by its tone. */
export const noticeClass = (n) => (n.tone === 'ok' ? 'info' : n.tone === 'warn' ? 'srv-warn-notice' : 'srv-error');

/**
 * The database list: rows, aliases (and their revision), problems, the count of malformed entries, the read error,
 * and `version` (bumped after every reload that leaves nothing loading: the Models tab reads
 * the models again).
 */
export const list = reactive({
  rows: [], aliases: [], aliasesRevision: '', problems: [], skipped: 0, error: null, loading: false, loaded: false, version: 0,
});

let timer = null;
let pending = null;

/** Reads the database list again (one read at a time; polled while a database is loading). */
export function reload() {
  if (pending) return pending;
  pending = (async () => {
    list.loading = true;
    try {
      const answer = await fetchDatabases(requestOptions);
      list.rows = answer.rows;
      list.aliases = answer.aliases;
      list.aliasesRevision = answer.aliasesRevision;
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

/** The link to the OpenAPI description of a list row's database or alias (its HTTP API). */
export const openapiHref = (row) => new URL(databaseOpenapiHref(row.name), ROOT).href;

/** The link to the OpenAPI description of the server's own HTTP API. */
export const serverOpenapiHref = () => new URL(SERVER_OPENAPI_HREF, ROOT).href;

/**
 * Makes the calling tab component (in `setup`) read its data with `refresh` when it is set up,
 * each time it is shown again (the playground keeps tabs alive) and when the window gets the
 * focus back or the page becomes visible while it is shown (./fresh.js). `refresh` replaces
 * only the shown data, never the input of an open form. Returns the freshness state (`active`:
 * whether the tab is shown; a poll stops while it is not).
 */
export function useFreshData(refresh) {
  const freshness = createFreshness(refresh);
  let unlisten = () => {};
  onMounted(() => {
    freshness.mounted();
    unlisten = listenForReturn(freshness, globalThis.window, globalThis.document);
  });
  onActivated(() => freshness.activated());
  onDeactivated(() => freshness.deactivated());
  onUnmounted(() => {
    freshness.unmounted();
    unlisten();
  });
  return freshness;
}

/** The page's localStorage, or null (blocked: the accessor itself may throw). */
function localStore() {
  try {
    return globalThis.localStorage || null;
  } catch {
    return null;
  }
}

/**
 * The sort of the table `table` (a name unique in the UI, the localStorage key) over the
 * column definitions `columns` = `{key: {type: 'text'|'number'|'date'|'bool', value?(row)}}`
 * (./sort.js): `rows(list)` the list sorted, `toggle(key)` a header click, `mark(key)` the
 * indicator, `aria(key)` the header's `aria-sort`. The choice is kept per table in localStorage.
 */
export function useSort(table, columns) {
  const state = reactive({ sort: loadSort(localStore(), table, columns) });
  return {
    state,
    columns,
    rows: (list) => sortRows(list, state.sort, columns),
    toggle(key) {
      state.sort = toggleSort(state.sort, key);
      saveSort(localStore(), table, state.sort);
    },
    mark: (key) => sortMark(state.sort, key),
    aria: (key) => ariaSort(state.sort, key),
  };
}

/** A sortable column header: `<sort-th :s="sorter" k="name">Name</sort-th>` (`s` from
 * `useSort`, `k` the column's key; `title` its tooltip; `num` right-aligned). */
export const SortTh = {
  props: {
    s: { type: Object, required: true },
    k: { type: String, required: true },
    title: { type: String, default: '' },
    num: { type: Boolean, default: false },
  },
  template: `<th :aria-sort="s.aria(k)" :title="title || null" :class="{ num }"><button type="button" class="srv-sort" :data-sort="k" @click="s.toggle(k)"><slot></slot><span class="srv-sort-mark" :class="{ on: !!s.mark(k) }" aria-hidden="true">{{ s.mark(k) || '↕' }}</span></button></th>`,
};
