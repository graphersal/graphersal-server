// Sortable columns of the server UI's tables (pure, tested under Node by ui/tests/sort.test.mjs).
// A click on a column header sorts the table by it, ascending; a second click on the same header
// descending, then ascending again. Until the first click a table keeps the server's order. The
// sort is stable (equal values keep their order) and typed per column:
//   text    natural order, case-insensitive (`db2` before `db10`);
//   number  numerically (bytes, durations, ids, counts);
//   date    by time (ISO 8601 text, a Date or epoch milliseconds);
//   bool    false before true.
// Missing values (null, undefined, '', NaN, an unreadable date) go last in both directions. The
// choice is kept per table in localStorage under `graphersal-server.sort.<table>` (every access
// in try/catch: a private window or blocked storage just forgets it). The Vue side is
// ./shared.js `useSort`; imports nothing.

/** The localStorage key prefix of a table's sort. */
export const SORT_KEY_PREFIX = 'graphersal-server.sort.';

/** The column types `compareValues` knows. */
export const SORT_TYPES = Object.freeze(['text', 'number', 'date', 'bool']);

const collator = typeof Intl !== 'undefined' && Intl.Collator
  ? new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })
  : null;

/** `value` as the comparable key of `type`, or null when it is missing or unreadable. */
export function sortKey(value, type = 'text') {
  if (value === null || value === undefined) return null;
  switch (type) {
    case 'number': {
      const n = typeof value === 'number' ? value : (typeof value === 'string' && value.trim() !== '' ? Number(value) : NaN);
      return Number.isFinite(n) ? n : null;
    }
    case 'date': {
      let t = NaN;
      if (value instanceof Date) t = value.getTime();
      else if (typeof value === 'number') t = value;
      else if (typeof value === 'string' && value.trim() !== '') t = Date.parse(value);
      return Number.isFinite(t) ? t : null;
    }
    case 'bool':
      return typeof value === 'boolean' ? Number(value) : null;
    default: {
      const s = Array.isArray(value) ? value.join(', ') : String(value);
      return s === '' ? null : s;
    }
  }
}

/** Compares two present keys of `type` (`sortKey`): negative, zero or positive. */
function compareKeys(a, b, type) {
  if (type === 'number' || type === 'date' || type === 'bool') return a - b;
  if (collator) return collator.compare(a, b);
  const x = a.toLowerCase();
  const y = b.toLowerCase();
  return x < y ? -1 : x > y ? 1 : 0;
}

/**
 * Compares the values `a` and `b` of a column of `type` in direction `dir` ('asc' | 'desc'):
 * negative when `a` comes first. Missing values come last in both directions.
 */
export function compareValues(a, b, type = 'text', dir = 'asc') {
  const x = sortKey(a, type);
  const y = sortKey(b, type);
  if (x === null && y === null) return 0;
  if (x === null) return 1;
  if (y === null) return -1;
  const c = compareKeys(x, y, type);
  return dir === 'desc' ? -c : c;
}

/**
 * `rows` sorted by `sort` = `{key, dir}` with the column definitions `columns` = `{key: {type,
 * value?}}` (`value(row)` the sorted value, else `row[key]`); a new array, stable. Without a
 * sort, or for a key `columns` does not name, the rows in their own order (a copy).
 */
export function sortRows(rows, sort, columns) {
  const list = Array.isArray(rows) ? rows.slice() : [];
  if (!sort || !sort.key || !columns || !Object.prototype.hasOwnProperty.call(columns, sort.key)) return list;
  const col = columns[sort.key] || {};
  const type = SORT_TYPES.includes(col.type) ? col.type : 'text';
  const valueOf = typeof col.value === 'function' ? col.value : (row) => (row ? row[sort.key] : undefined);
  const dir = sort.dir === 'desc' ? 'desc' : 'asc';
  return list
    .map((row, index) => ({ row, index, value: valueOf(row) }))
    .sort((a, b) => compareValues(a.value, b.value, type, dir) || a.index - b.index)
    .map((e) => e.row);
}

/** The sort after a click on the header of `key`: the same column flips its direction, another
 * column starts ascending. */
export function toggleSort(sort, key) {
  if (sort && sort.key === key) return { key, dir: sort.dir === 'asc' ? 'desc' : 'asc' };
  return { key, dir: 'asc' };
}

/** The header's indicator of `key`: '▲' ascending, '▼' descending, '' when not sorted by it. */
export function sortMark(sort, key) {
  if (!sort || sort.key !== key) return '';
  return sort.dir === 'desc' ? '▼' : '▲';
}

/** The header's `aria-sort` of `key`: 'ascending', 'descending' or 'none'. */
export function ariaSort(sort, key) {
  if (!sort || sort.key !== key) return 'none';
  return sort.dir === 'desc' ? 'descending' : 'ascending';
}

/** A checked sort `{key, dir}` from untrusted input (stored text), or null. */
export function checkSort(value, columns) {
  if (!value || typeof value !== 'object' || typeof value.key !== 'string') return null;
  if (columns && !Object.prototype.hasOwnProperty.call(columns, value.key)) return null;
  if (value.dir !== 'asc' && value.dir !== 'desc') return null;
  return { key: value.key, dir: value.dir };
}

/** The sort of `table` kept in `storage` (localStorage), or null (none kept, unreadable, or
 * no storage). Never throws. */
export function loadSort(storage, table, columns) {
  try {
    if (!storage) return null;
    const text = storage.getItem(SORT_KEY_PREFIX + table);
    return text ? checkSort(JSON.parse(text), columns) : null;
  } catch {
    return null;
  }
}

/** Keeps the sort of `table` in `storage` (null removes it). Never throws. */
export function saveSort(storage, table, sort) {
  try {
    if (!storage) return;
    if (sort) storage.setItem(SORT_KEY_PREFIX + table, JSON.stringify({ key: sort.key, dir: sort.dir }));
    else storage.removeItem(SORT_KEY_PREFIX + table);
  } catch {
    // a private window or blocked storage: the sort is just not kept
  }
}
