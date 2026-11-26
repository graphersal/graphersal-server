// The row actions menu ("…") of the server UI's tables (pure, tested under Node by
// ui/tests/menu.test.mjs; the Vue side is ./shared.js `ActionMenu`). The menu is fixed to the
// viewport, so a table wrapper that scrolls never clips it: it opens below its button, right
// edges aligned, and upward when the room below is too small for it and there is more room
// above. Keys (WAI-ARIA menu button): Enter, Space or ArrowDown on the button open it at the
// first item, ArrowUp at the last; in the menu ArrowDown/ArrowUp move (wrapping), Home/End jump,
// Escape closes it and gives the focus back to the button, Tab closes it. A click outside, a
// scroll or a resize closes it too. Imports nothing.

/** The gap between the button and its menu (px). */
export const MENU_GAP = 4;

/**
 * Where the menu of the button at `button` (a DOMRect: top, bottom, right) goes, for a menu
 * `height` px tall in a viewport `viewport` = `{width, height}`: `{up, style}`, the style as
 * fixed-position CSS (`right` and `top` or `bottom`, in px).
 */
export function menuPlacement(button, height, viewport) {
  const below = viewport.height - button.bottom - MENU_GAP;
  const above = button.top - MENU_GAP;
  const up = height > below && above > below;
  const right = `${Math.max(0, Math.round(viewport.width - button.right))}px`;
  const style = up
    ? { right, bottom: `${Math.round(viewport.height - button.top + MENU_GAP)}px` }
    : { right, top: `${Math.round(button.bottom + MENU_GAP)}px` };
  if (up && height > above) style.maxHeight = `${Math.max(0, Math.floor(above))}px`;
  if (!up && height > below) style.maxHeight = `${Math.max(0, Math.floor(below))}px`;
  return { up, style };
}

/**
 * The item to focus after `key` in a menu of `count` items whose focused item is `index` (-1:
 * none), skipping the disabled ones (`enabled[i]` false); `index` itself when the key moves
 * nothing, -1 when no item is enabled.
 */
export function menuKeyIndex(key, index, enabled) {
  const count = enabled.length;
  const usable = enabled.map((on, i) => (on ? i : -1)).filter((i) => i >= 0);
  if (!usable.length) return -1;
  switch (key) {
    case 'Home':
      return usable[0];
    case 'End':
      return usable[usable.length - 1];
    case 'ArrowDown': {
      for (let step = 1; step <= count; step++) {
        const i = (((index + step) % count) + count) % count;
        if (enabled[i]) return i;
      }
      return index;
    }
    case 'ArrowUp': {
      const from = index < 0 ? count : index;
      for (let step = 1; step <= count; step++) {
        const i = (((from - step) % count) + count) % count;
        if (enabled[i]) return i;
      }
      return index;
    }
    default:
      return index;
  }
}
