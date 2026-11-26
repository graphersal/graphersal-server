// The notices of the server UI as toasts (pure, tested under Node by ui/tests/toasts.test.mjs):
// one stack for every tab, fixed in the top-right corner below the top bar, so a notice never
// moves the page (the playground's system journal notices are the model: a coloured left edge,
// the close cross on hover and focus). An outcome (`tone` 'ok') hides itself after
// `TOAST_LIFE_MS.ok`, a warning after `TOAST_LIFE_MS.warn`; an error stays until it is closed.
// A timer is paused while the pointer or the keyboard focus is on its toast. At most
// `TOAST_MAX` toasts at once: a new one pushes out the oldest. The same notice again (tone,
// text and help) does not stack: its toast counts it and starts its time again. Time comes from
// an injectable clock (`now()` in ms), so the model has no timers of its own: the view
// (./shared.js `ToastStack`) asks `nextDeadline()` and calls `expire()` then. Imports nothing.
//
// A notice is `{tone: 'ok'|'warn'|'error', text, help?, links?: [{text, run}]}` (the shape of
// ./admin-api.js `refusalNotice`); `links` are actions shown under the text (a tab to open).

/** How long a toast of each tone stays (ms; `null` = until closed). */
export const TOAST_LIFE_MS = Object.freeze({ ok: 6000, warn: 12000, error: null });

/** The most toasts shown at once; a new one pushes out the oldest. */
export const TOAST_MAX = 5;

/** A notice's tone as one the stack knows ('ok' for anything else). */
export function toastTone(tone) {
  return tone === 'error' || tone === 'warn' ? tone : 'ok';
}

/** The CSS class of a toast of `tone`. */
export function toastClass(tone) {
  return `srv-toast-${toastTone(tone)}`;
}

/** The ARIA role of a toast of `tone`: an error interrupts ('alert'), the rest waits ('status'). */
export function toastRole(tone) {
  return toastTone(tone) === 'error' ? 'alert' : 'status';
}

/** Whether a toast of `tone` stays until closed. */
export function isSticky(tone) {
  return TOAST_LIFE_MS[toastTone(tone)] === null;
}

/**
 * A toast stack. `now()` gives the time in ms (default `Date.now`). `push(notice)` adds a
 * notice (the id of its toast, or 0 for no notice), `close(id)`, `clear()`, `hover(id, on)`
 * (pauses or resumes its timer: whether anything changed), `expire()` (closes the toasts whose
 * time is up: whether any was), `nextDeadline()` (the earliest time a toast ends, or null) and
 * `view()` (the toasts to show, oldest first: `{id, tone, cls, role, text, help, links, count}`).
 */
export function createToasts({ now = () => Date.now() } = {}) {
  let toasts = [];
  let nextId = 1;

  const lifeOf = (t) => TOAST_LIFE_MS[t.tone];
  const deadlineOf = (t) => (lifeOf(t) === null || t.hovered ? null : t.since + lifeOf(t) - t.spent);

  return {
    push(notice) {
      if (!notice || typeof notice !== 'object' || !notice.text) return 0;
      const tone = toastTone(notice.tone);
      const text = String(notice.text);
      const help = notice.help ? String(notice.help) : '';
      const links = Array.isArray(notice.links)
        ? notice.links.filter((l) => l && typeof l.text === 'string' && typeof l.run === 'function')
        : [];
      const same = toasts.find((t) => t.tone === tone && t.text === text && t.help === help);
      if (same) {
        same.count += 1;
        same.since = now();
        same.spent = 0;
        same.links = links;
        toasts = [...toasts.filter((t) => t !== same), same];
        return same.id;
      }
      const toast = { id: nextId++, tone, text, help, links, count: 1, since: now(), spent: 0, hovered: false };
      toasts = [...toasts, toast].slice(-TOAST_MAX);
      return toast.id;
    },
    close(id) {
      const before = toasts.length;
      toasts = toasts.filter((t) => t.id !== id);
      return toasts.length !== before;
    },
    clear() {
      toasts = [];
    },
    hover(id, on) {
      const t = toasts.find((x) => x.id === id);
      if (!t || t.hovered === !!on) return false;
      if (on) {
        t.spent += now() - t.since;
        t.hovered = true;
      } else {
        t.since = now();
        t.hovered = false;
      }
      return true;
    },
    expire() {
      const at = now();
      const before = toasts.length;
      toasts = toasts.filter((t) => {
        const end = deadlineOf(t);
        return end === null || end > at;
      });
      return toasts.length !== before;
    },
    nextDeadline() {
      const ends = toasts.map(deadlineOf).filter((d) => d !== null);
      return ends.length ? Math.min(...ends) : null;
    },
    view() {
      return toasts.map((t) => ({
        id: t.id,
        tone: t.tone,
        cls: toastClass(t.tone),
        role: toastRole(t.tone),
        text: t.text,
        help: t.help,
        links: t.links,
        count: t.count,
      }));
    },
  };
}
