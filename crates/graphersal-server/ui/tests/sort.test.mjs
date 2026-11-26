// Node tests of the sortable table columns (ui/sort.js): the typed comparator (natural text,
// numbers, sizes, dates, booleans; missing values last in both directions), the stable sort, the
// header toggling and its indicator, the kept choice per table (localStorage, never throwing),
// and that every table of the admin UI has sortable headers.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  compareValues, sortKey, sortRows, toggleSort, sortMark, ariaSort, checkSort, loadSort, saveSort, SORT_KEY_PREFIX,
} from '../sort.js';

const order = (values, type, dir) => values.slice().sort((a, b) => compareValues(a, b, type, dir));

test('text: natural and case-insensitive; missing last both ways', () => {
  assert.deepEqual(order(['db10', 'DB2', 'alpha', 'db1'], 'text'), ['alpha', 'db1', 'DB2', 'db10']);
  assert.deepEqual(order(['b', '', null, 'a', undefined], 'text'), ['a', 'b', '', null, undefined]);
  assert.deepEqual(order(['b', '', null, 'a'], 'text', 'desc'), ['b', 'a', '', null]);
  assert.equal(sortKey(['x', 'y'], 'text'), 'x, y', 'a list sorts by its text');
});

test('number (bytes, durations, ids): numeric, not textual', () => {
  assert.deepEqual(order([1024, 9, 1048576, 0, null], 'number'), [0, 9, 1024, 1048576, null]);
  assert.deepEqual(order([1024, 9, 1048576, 0, null], 'number', 'desc'), [1048576, 1024, 9, 0, null]);
  assert.deepEqual(order(['10', '9', 'x', NaN], 'number'), ['9', '10', 'x', NaN], 'unreadable numbers last');
  assert.equal(sortKey('', 'number'), null);
});

test('date: by time from ISO text, Date or epoch ms; unreadable last', () => {
  const a = '2026-11-25T10:00:00Z';
  const b = '2026-11-26T09:00:00+01:00';
  const c = new Date('2026-01-01T00:00:00Z');
  assert.deepEqual(order([a, 'never', b, c], 'date'), [c, a, b, 'never']);
  assert.deepEqual(order([a, b, c], 'date', 'desc'), [b, a, c]);
  assert.equal(sortKey(0, 'date'), 0);
});

test('bool: false before true ascending', () => {
  assert.deepEqual(order([true, false, null, true], 'bool'), [false, true, true, null]);
  assert.deepEqual(order([true, false], 'bool', 'desc'), [true, false]);
});

test('sortRows: stable, by a column value function, a copy; unknown or no sort keeps the order', () => {
  const rows = [
    { name: 'b', size: 5, n: 1 }, { name: 'a', size: 5, n: 2 }, { name: 'c', size: 1, n: 3 }, { name: 'd', size: null, n: 4 },
  ];
  const columns = { name: { type: 'text' }, size: { type: 'number', value: (r) => r.size } };
  assert.deepEqual(sortRows(rows, { key: 'size', dir: 'asc' }, columns).map((r) => r.n), [3, 1, 2, 4], 'equal sizes keep their order');
  assert.deepEqual(sortRows(rows, { key: 'size', dir: 'desc' }, columns).map((r) => r.n), [1, 2, 3, 4], 'stable descending too; missing last');
  assert.deepEqual(sortRows(rows, { key: 'name', dir: 'desc' }, columns).map((r) => r.name), ['d', 'c', 'b', 'a']);
  assert.deepEqual(sortRows(rows, null, columns).map((r) => r.n), [1, 2, 3, 4]);
  assert.deepEqual(sortRows(rows, { key: 'nope', dir: 'asc' }, columns).map((r) => r.n), [1, 2, 3, 4]);
  assert.notEqual(sortRows(rows, null, columns), rows, 'never the same array');
  assert.deepEqual(sortRows(null, null, columns), []);
});

test('toggling: a new column starts ascending, the same column flips; indicator and aria-sort follow', () => {
  let sort = null;
  assert.equal(sortMark(sort, 'name'), '');
  assert.equal(ariaSort(sort, 'name'), 'none');
  sort = toggleSort(sort, 'name');
  assert.deepEqual(sort, { key: 'name', dir: 'asc' });
  assert.equal(sortMark(sort, 'name'), '▲');
  assert.equal(ariaSort(sort, 'name'), 'ascending');
  assert.equal(sortMark(sort, 'size'), '', 'other columns show no direction');
  sort = toggleSort(sort, 'name');
  assert.deepEqual(sort, { key: 'name', dir: 'desc' });
  assert.equal(sortMark(sort, 'name'), '▼');
  assert.equal(ariaSort(sort, 'name'), 'descending');
  sort = toggleSort(sort, 'name');
  assert.deepEqual(sort, { key: 'name', dir: 'asc' });
  sort = toggleSort(sort, 'size');
  assert.deepEqual(sort, { key: 'size', dir: 'asc' }, 'another column starts ascending');
});

test('the choice is kept per table; bad or blocked storage never throws', () => {
  const map = new Map();
  const storage = {
    getItem: (k) => (map.has(k) ? map.get(k) : null), setItem: (k, v) => map.set(k, v), removeItem: (k) => map.delete(k),
  };
  const columns = { name: {}, size: {} };
  saveSort(storage, 'users', { key: 'name', dir: 'desc' });
  saveSort(storage, 'grants', { key: 'size', dir: 'asc' });
  assert.equal(map.get(`${SORT_KEY_PREFIX}users`), '{"key":"name","dir":"desc"}');
  assert.deepEqual(loadSort(storage, 'users', columns), { key: 'name', dir: 'desc' });
  assert.deepEqual(loadSort(storage, 'grants', columns), { key: 'size', dir: 'asc' });
  assert.equal(loadSort(storage, 'other', columns), null);
  saveSort(storage, 'users', null);
  assert.equal(loadSort(storage, 'users', columns), null);
  map.set(`${SORT_KEY_PREFIX}bad`, '{not json');
  assert.equal(loadSort(storage, 'bad', columns), null);
  map.set(`${SORT_KEY_PREFIX}gone`, '{"key":"removed","dir":"asc"}');
  assert.equal(loadSort(storage, 'gone', columns), null, 'a column that no longer exists');
  assert.equal(checkSort({ key: 'name', dir: 'up' }, columns), null);
  const blocked = { getItem() { throw new Error('SecurityError'); }, setItem() { throw new Error('QuotaExceeded'); }, removeItem() { throw new Error('x'); } };
  assert.equal(loadSort(blocked, 'users', columns), null);
  assert.doesNotThrow(() => saveSort(blocked, 'users', { key: 'name', dir: 'asc' }));
  assert.doesNotThrow(() => saveSort(blocked, 'users', null));
  assert.equal(loadSort(null, 'users', columns), null);
  assert.doesNotThrow(() => saveSort(undefined, 'users', { key: 'name', dir: 'asc' }));
});

test('every table of the admin UI sorts: its headers are sort-th, its rows come sorted', () => {
  const tables = {
    'databases-tab.js': ['dbSort', 'aliasSort'],
    'users-tab.js': ['userSort', 'grantSort'],
    'backups-tab.js': ['backupSort'],
    'import-tab.js': ['inboxSort'],
    'models-tab.js': ['modelSort', 'missingSort'],
    'runs-tab.js': ['runSort'],
    'mine-tab.js': ['mineSort'],
    'mcp-tab.js': ['dbSort', 'toolSort', 'userSort', 'sessionSort', 'errorSort'],
  };
  for (const [file, sorters] of Object.entries(tables)) {
    const src = readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');
    assert.ok(src.includes('components: { SortTh }'), `${file} registers the header`);
    for (const s of sorters) {
      assert.ok(src.includes(`:s="${s}"`), `${file}: ${s} drives headers`);
      assert.ok(src.includes(`${s}.rows(`), `${file}: ${s} sorts the rows`);
    }
    // No plain text header is left (only the empty actions/pick headers).
    const plain = [...src.matchAll(/<th>([^<]+)<\/th>/g)].map((m) => m[1]);
    assert.deepEqual(plain, [], `${file}: unsortable headers ${plain.join(', ')}`);
  }
});
