// The row actions menu (ui/menu.js; shared.js `ActionMenu`): where it opens (below, or upward
// near the bottom of the viewport), the keys, and the Databases tab's use of it (every row
// action and the OpenAPI link in it, the table sorted by name by default, no HTTP-clients line).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { menuPlacement, menuKeyIndex, MENU_GAP } from '../menu.js';

const rect = (top, bottom, right) => ({ top, bottom, right });

test('placement: below the button, right edges aligned; upward near the bottom', () => {
  const viewport = { width: 1280, height: 800 };
  const down = menuPlacement(rect(100, 128, 1200), 300, viewport);
  assert.equal(down.up, false);
  assert.deepEqual(down.style, { right: '80px', top: `${128 + MENU_GAP}px` });
  const up = menuPlacement(rect(700, 728, 1200), 300, viewport);
  assert.equal(up.up, true);
  assert.deepEqual(up.style, { right: '80px', bottom: `${800 - 700 + MENU_GAP}px` });
  // Too tall for both sides: the larger side, with a height limit (the menu scrolls).
  const tall = menuPlacement(rect(300, 328, 1200), 900, viewport);
  assert.equal(tall.up, false);
  assert.equal(tall.style.maxHeight, `${800 - 328 - MENU_GAP}px`);
  const tallUp = menuPlacement(rect(600, 628, 1200), 900, viewport);
  assert.equal(tallUp.up, true);
  assert.equal(tallUp.style.maxHeight, `${600 - MENU_GAP}px`);
  // A button past the right edge never gives a negative offset.
  assert.equal(menuPlacement(rect(10, 30, 1300), 100, viewport).style.right, '0px');
});

test('keys: arrows wrap, Home/End jump, disabled items skipped, others move nothing', () => {
  const all = [true, true, true, true];
  assert.equal(menuKeyIndex('ArrowDown', -1, all), 0);
  assert.equal(menuKeyIndex('ArrowDown', 3, all), 0);
  assert.equal(menuKeyIndex('ArrowUp', 0, all), 3);
  assert.equal(menuKeyIndex('ArrowUp', -1, all), 3);
  assert.equal(menuKeyIndex('Home', 2, all), 0);
  assert.equal(menuKeyIndex('End', 0, all), 3);
  assert.equal(menuKeyIndex('x', 2, all), 2);
  const some = [false, true, false, true];
  assert.equal(menuKeyIndex('Home', -1, some), 1);
  assert.equal(menuKeyIndex('ArrowDown', 1, some), 3);
  assert.equal(menuKeyIndex('ArrowDown', 3, some), 1);
  assert.equal(menuKeyIndex('ArrowUp', 1, some), 3);
  assert.equal(menuKeyIndex('End', -1, [false, false]), -1);
});

test('the menu component: a menu button with keyboard, outside click, Escape, fixed placement', () => {
  const shared = readFileSync(new URL('../shared.js', import.meta.url), 'utf8');
  for (const piece of ['aria-haspopup="menu"', ':aria-expanded=', 'role="menu"', 'role="menuitem"', "'Escape'", "'pointerdown'", "'scroll'", "'resize'", 'menuPlacement(', 'onDeactivated(() => close(false))']) {
    assert.ok(shared.includes(piece), piece);
  }
  const css = readFileSync(new URL('../server-ui.css', import.meta.url), 'utf8');
  assert.match(css, /\.srv-menu-list \{[^}]*position: fixed/, 'a scrolling table wrapper never clips it');
});

test('the Databases tab: one menu per row with every action and the OpenAPI link; sorted by name', () => {
  const tab = readFileSync(new URL('../databases-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes('<action-menu :items="rowItems(row)"'), 'the menu in every row');
  for (const key of ['load', 'unload', 'settings', 'runs', 'clone', 'backups', 'export', 'verify', 'repair', 'promote', 'openapi', 'delete', 'point', 'rename', 'remove']) {
    assert.ok(tab.includes(`key: '${key}'`), key);
  }
  // No row action is left as a button of its own in the database table.
  const table = tab.slice(tab.indexOf('<table class="data-table srv-table srv-databases">'), tab.indexOf('</table>'));
  assert.ok(!/<button/.test(table.replace(/<sort-th[^]*?<\/sort-th>/g, '')), 'no buttons in the rows');
  assert.ok(!tab.includes('HTTP clients:'), 'the HTTP-clients line is gone');
  assert.ok(tab.includes("useSort('databases', DATABASE_COLUMNS, { key: 'name', dir: 'asc' })"));
  assert.ok(tab.includes("useSort('aliases', ALIAS_COLUMNS, { key: 'alias', dir: 'asc' })"));
  const mcp = readFileSync(new URL('../mcp-tab.js', import.meta.url), 'utf8');
  assert.ok(mcp.includes("useSort('mcp-databases', DB_COLUMNS, { key: 'name', dir: 'asc' })"));
});
