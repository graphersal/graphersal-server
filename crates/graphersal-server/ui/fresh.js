// Fresh data on every visit of a tab (pure, tested under Node by ui/tests/fresh.test.mjs). The
// playground keeps the profile's tabs alive (`<keep-alive>`): a tab is set up once, and leaving
// and entering it again shows it as it was. So a tab that read its data only when it was set up
// showed stale data until the page was reloaded (a user added in the console did not appear in
// Users). Every tab therefore reads its data again
//   - when it is set up (`mounted`),
//   - each time it is shown again (`activated`, after `deactivated`),
//   - when the window gets the focus back or the page becomes visible again while the tab is
//     shown (`shown`; both events arrive together: one read per FOCUS_GAP_MS).
// A tab's `refresh` replaces only the data it shows, never the input of an open form or dialog.
// One read at a time per tab: a trigger while a read runs gets that read. The Vue side (hooks
// and window listeners) is ./shared.js `useFreshData`; imports nothing.

/** The least time between two reads caused by focus or visibility (milliseconds). */
export const FOCUS_GAP_MS = 1000;

/**
 * The freshness state of one tab around its `refresh` (a function returning a promise, or
 * nothing). Returns `{mounted, activated, deactivated, shown, unmounted, active, reads}`:
 * `mounted`/`activated`/`shown` start a read when it is due (the promise of the read, or null
 * when none was due), `active` says whether the tab is shown, `reads` counts the reads started.
 * `now` is the clock (milliseconds), replaceable for tests.
 */
export function createFreshness(refresh, { now = () => Date.now(), focusGapMs = FOCUS_GAP_MS } = {}) {
  let active = false;
  let alive = true;
  let running = null;
  let lastStart = -Infinity;
  let reads = 0;

  function read() {
    if (!alive) return null;
    if (running) return running;
    lastStart = now();
    reads++;
    running = Promise.resolve()
      .then(() => refresh())
      .catch(() => {}) // a tab shows its own read errors; a trigger never throws
      .finally(() => {
        running = null;
      });
    return running;
  }

  return {
    /** The tab was set up and is shown: its first read. */
    mounted() {
      active = true;
      return read();
    },
    /** The tab is shown again: a read (none when it never left, e.g. right after `mounted`). */
    activated() {
      if (active) return null;
      active = true;
      return read();
    },
    /** The tab is hidden (another tab is shown): no reads until it is shown again. */
    deactivated() {
      active = false;
    },
    /** The window got the focus back or the page became visible: a read when the tab is shown
     * and the last read started at least `focusGapMs` ago. */
    shown() {
      if (!active || now() - lastStart < focusGapMs) return null;
      return read();
    },
    /** The tab is gone: nothing reads any more. */
    unmounted() {
      active = false;
      alive = false;
    },
    get active() {
      return active;
    },
    get reads() {
      return reads;
    },
  };
}

/**
 * Calls `freshness.shown()` when `win` gets the focus or `doc` becomes visible (not when it is
 * hidden). Returns the function that removes the listeners. `win`/`doc` are EventTargets
 * (`window`, `document`; mocks in tests); missing ones are skipped.
 */
export function listenForReturn(freshness, win, doc) {
  const onShown = () => {
    if (doc && doc.visibilityState === 'hidden') return;
    freshness.shown();
  };
  if (win && win.addEventListener) win.addEventListener('focus', onShown);
  if (doc && doc.addEventListener) doc.addEventListener('visibilitychange', onShown);
  return () => {
    if (win && win.removeEventListener) win.removeEventListener('focus', onShown);
    if (doc && doc.removeEventListener) doc.removeEventListener('visibilitychange', onShown);
  };
}
