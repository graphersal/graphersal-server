// The state the `server` profile's tabs share (one module instance per page: every tab imports
// this file): the server root, the host's request headers, the admin client and the database
// list with its reload (polled while a database is loading), and what a refused request does
// (401: the login page; 404 of the admin API: the root again when the principal lost the admin
// role), the fresh data of every tab on every visit (`useFreshData`, model ./fresh.js), the
// sortable table columns (`useSort`, `SortTh`, model ./sort.js), the one toast stack of every
// tab (`notify`, `ToastStack`, model ./toasts.js), the row actions menu (`ActionMenu`, model
// ./menu.js) and the error of a form under its submit button (`FormError`). Imports only `vue`
// and the profile's own files.

import {
  reactive, ref, nextTick, createApp, onMounted, onActivated, onDeactivated, onUnmounted, onBeforeUnmount,
} from 'vue';
import { createFreshness, listenForReturn } from './fresh.js';
import { createToasts } from './toasts.js';
import { menuPlacement, menuKeyIndex } from './menu.js';
import {
  sortRows, toggleSort, sortMark, ariaSort, loadSort, saveSort,
} from './sort.js';
import { createAdmin, sendAdmin, refusalNotice } from './admin-api.js';
import {
  accessRequest, loginHref, principalOf, refusalAction, rootHref,
} from './access-api.js';
import {
  fetchDatabases, anyLoading, databaseRows, hostHeaders, LANDING_POLL_MS,
  databaseOpenapiHref,
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

/** The CSS classes of an inline notice (a state shown in the page, not a toast) by its tone. */
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
 * indicator, `aria(key)` the header's `aria-sort`. The choice is kept per table in localStorage;
 * until one is made the table sorts by `defaultSort` (`{key, dir}`; null: the server's order).
 */
export function useSort(table, columns, defaultSort = null) {
  const state = reactive({ sort: loadSort(localStore(), table, columns) || defaultSort });
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

// ---------------------------------------------------------------- toasts

const toastModel = createToasts();
/** The toasts shown (./toasts.js `view()`), for `ToastStack`. */
const toastView = reactive({ toasts: [] });
let toastTimer = null;
let toastHost = null;

function renderToasts() {
  toastView.toasts = toastModel.view();
  clearTimeout(toastTimer);
  toastTimer = null;
  const next = toastModel.nextDeadline();
  if (next !== null) {
    toastTimer = setTimeout(() => {
      toastTimer = null;
      toastModel.expire();
      renderToasts();
    }, Math.max(0, next - Date.now()) + 20);
  }
}

/** The stack of toasts (one per page, mounted by `notify` on its first call). */
export const ToastStack = {
  setup() {
    const close = (id) => {
      if (toastModel.close(id)) renderToasts();
    };
    const hover = (id, on) => {
      if (toastModel.hover(id, on)) renderToasts();
    };
    const follow = (t, link) => {
      close(t.id);
      link.run();
    };
    return { v: toastView, close, hover, follow };
  },
  template: `
  <div class="srv-toasts" aria-label="Notices">
    <div v-for="t in v.toasts" :key="t.id" class="srv-toast" :class="t.cls" :role="t.role"
         :aria-live="t.role === 'alert' ? 'assertive' : 'polite'" :data-toast="t.tone"
         @mouseenter="hover(t.id, true)" @mouseleave="hover(t.id, false)"
         @focusin="hover(t.id, true)" @focusout="hover(t.id, false)">
      <div class="srv-toast-body">
        <span class="srv-toast-text">{{ t.text }}<span v-if="t.count > 1" class="srv-toast-count" :aria-label="t.count + ' times'"> ×{{ t.count }}</span></span>
        <p v-if="t.help" class="srv-toast-help">{{ t.help }}</p>
        <p v-if="t.links.length" class="srv-toast-links"><template v-for="(l, i) in t.links" :key="l.text"><template v-if="i"> · </template><a href="#" @click.prevent="follow(t, l)">{{ l.text }}</a></template></p>
      </div>
      <button type="button" class="srv-toast-close" @click="close(t.id)" aria-label="Close notice">×</button>
    </div>
  </div>
  `,
};

/** Mounts the toast stack once, on a node of its own at the end of the page's body. */
function ensureToastHost() {
  if (toastHost || !globalThis.document || !globalThis.document.body) return;
  const node = globalThis.document.createElement('div');
  node.className = 'srv-toast-host';
  globalThis.document.body.appendChild(node);
  toastHost = createApp(ToastStack);
  toastHost.mount(node);
}

/**
 * Shows `notice` (`{tone, text, help?, links?}`, e.g. ./admin-api.js `refusalNotice`) as a toast:
 * the page does not move; an outcome hides itself after a few seconds, a warning a bit later, an
 * error stays until closed (./toasts.js). Returns the toast's id (0: no notice).
 */
export function notify(notice) {
  const id = toastModel.push(notice);
  if (id) {
    ensureToastHost();
    renderToasts();
  }
  return id;
}

// ---------------------------------------------------------------- forms

/**
 * The error of a form, under its submit button: `<form-error :error="x" :problems="list">`
 * (`error` a notice `{text, help}` as ./admin-api.js `refusalNotice` gives, `problems` the
 * form's own checks, texts).
 */
export const FormError = {
  props: {
    error: { type: Object, default: null },
    problems: { type: Array, default: () => [] },
  },
  template: `
  <div v-if="error || problems.length" class="srv-form-error" role="alert" data-form-error>
    <ul v-if="problems.length" class="srv-problems"><li v-for="p in problems" :key="p">{{ p }}</li></ul>
    <div v-if="error" class="notice srv-notice" :class="error.tone === 'warn' ? 'srv-warn-notice' : 'srv-error'">
      <div class="srv-notice-text"><span>{{ error.text }}</span>
        <p v-if="error.help" class="srv-help">{{ error.help }}</p></div>
    </div>
  </div>
  `,
};

// ---------------------------------------------------------------- row actions menu

let menuIds = 0;

/**
 * The actions of a table row behind one "…" button: `<action-menu :items="list" label="...">`.
 * An item is `{key, text, title?, enabled?, danger?, href?, run?}`: a link (`href`, opened in a
 * new tab) or an action (`run()`); a disabled item stays focusable and says why in its title;
 * `{key, separator: true}` draws a line between groups.
 * Keyboard and placement: ./menu.js.
 */
export const ActionMenu = {
  props: {
    items: { type: Array, required: true },
    label: { type: String, default: 'Actions' },
  },
  setup(props) {
    const open = ref(false);
    const style = ref({});
    const up = ref(false);
    const button = ref(null);
    const menu = ref(null);
    const id = `srv-menu-${++menuIds}`;

    const itemNodes = () => (menu.value ? [...menu.value.querySelectorAll('[role="menuitem"]')] : []);
    function focusItem(index) {
      const nodes = itemNodes();
      if (index >= 0 && nodes[index]) nodes[index].focus();
    }
    function onOutside(event) {
      if (!open.value) return;
      const t = event.target;
      if ((menu.value && menu.value.contains(t)) || (button.value && button.value.contains(t))) return;
      close(false);
    }
    function onMove(event) {
      // A scroll inside the menu itself (a long menu with its own scroll) keeps it.
      if (event && event.type === 'scroll' && menu.value && menu.value.contains(event.target)) return;
      close(false);
    }
    function listen(on) {
      const w = globalThis.window;
      if (!w) return;
      const method = on ? 'addEventListener' : 'removeEventListener';
      w.document[method]('pointerdown', onOutside, true);
      w[method]('scroll', onMove, true);
      w[method]('resize', onMove);
    }
    function place() {
      if (!button.value || !menu.value) return;
      const w = globalThis.window;
      const viewport = { width: w.document.documentElement.clientWidth || w.innerWidth, height: w.innerHeight };
      const placed = menuPlacement(button.value.getBoundingClientRect(), menu.value.scrollHeight, viewport);
      up.value = placed.up;
      style.value = placed.style;
    }
    async function show(at) {
      if (open.value) return;
      open.value = true;
      style.value = { visibility: 'hidden', top: '0px', right: '0px' };
      listen(true);
      await nextTick();
      place();
      await nextTick();
      const all = itemNodes().map(() => true);
      focusItem(at === 'last' ? menuKeyIndex('End', -1, all) : menuKeyIndex('Home', -1, all));
    }
    function close(refocus) {
      if (!open.value) return;
      open.value = false;
      listen(false);
      if (refocus && button.value) button.value.focus();
    }
    function toggle() {
      if (open.value) close(true);
      else show('first');
    }
    function onButtonKey(event) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        show(event.key === 'ArrowUp' ? 'last' : 'first');
      }
    }
    function onMenuKey(event) {
      const nodes = itemNodes();
      const index = nodes.indexOf(globalThis.document.activeElement);
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        close(true);
      } else if (event.key === 'Tab') {
        close(false);
      } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        focusItem(menuKeyIndex(event.key, index, nodes.map(() => true)));
      }
    }
    function choose(item, event) {
      if (item.enabled === false) {
        if (event) event.preventDefault();
        return;
      }
      close(!item.href);
      if (!item.href && typeof item.run === 'function') item.run();
    }
    onDeactivated(() => close(false));
    onBeforeUnmount(() => close(false));
    return { open, style, up, button, menu, id, toggle, onButtonKey, onMenuKey, choose };
  },
  template: `
  <span class="srv-menu">
    <button ref="button" type="button" class="btn small srv-menu-button" aria-haspopup="menu" :aria-expanded="open ? 'true' : 'false'"
            :aria-controls="open ? id : null" :aria-label="label" :title="label" data-menu-button @click="toggle" @keydown="onButtonKey">…</button>
    <div v-if="open" ref="menu" :id="id" class="srv-menu-list" :class="{ up }" role="menu" :aria-label="label" :style="style" data-menu @keydown="onMenuKey">
      <template v-for="item in items" :key="item.key">
        <div v-if="item.separator" role="separator" class="srv-menu-sep"></div>
        <a v-else-if="item.href" role="menuitem" class="srv-menu-item" :href="item.href" target="_blank" rel="noopener" tabindex="-1"
           :title="item.title || null" :data-action="item.key" @click="choose(item, $event)">{{ item.text }}</a>
        <button v-else type="button" role="menuitem" class="srv-menu-item" :class="{ danger: item.danger }" tabindex="-1"
                :aria-disabled="item.enabled === false ? 'true' : null" :title="item.title || null" :data-action="item.key"
                @click="choose(item, $event)">{{ item.text }}</button>
      </template>
    </div>
  </span>
  `,
};
