// Access in the `server` profile (pure, tested under Node by ui/tests/access.test.mjs): the
// browser login and logout, the return path `next` of the login page, the principal of the root
// `GET /api/info`, the "my databases" listing (`GET /api/databases`) and what the profile does
// with a refused admin request (401: the login page; 404: the principal may have lost the admin
// role, so the root is read again). The views are ./login-tab.js, ./mine-tab.js and ./header.js;
// the contract is book/src/reference/admin_api.md ("Login" and "My databases").
//
// The server answers each principal its own profile: an admin the management tabs, a user the
// "My databases" tab, anonymous the "Sign in" tab. After a login or a logout the page loads the
// root again, so the server's answer picks the tabs; the page never decides them itself.

import { sendAdmin, isOpenState } from './admin-api.js';
import { databaseHref, databaseOpenapiHref, DATABASE_STATES, STATE_TONES } from './databases.js';

/** The routes of this model, relative to the server root. */
export const accessRequest = Object.freeze({
  login: (token) => ({ method: 'POST', path: 'api/login', body: { token } }),
  logout: () => ({ method: 'POST', path: 'api/logout' }),
  databases: () => ({ method: 'GET', path: 'api/databases' }),
  info: () => ({ method: 'GET', path: 'api/info' }),
});

/** The longest return path the login page follows. */
export const MAX_NEXT_CHARS = 2048;

/**
 * The return path `value` when the login page may follow it, else null: a path from the server
 * root (starts with `/`), not `//` (another host), no backslash (browsers read it as `/`), no
 * space or control character, at most MAX_NEXT_CHARS. So the login page never sends a browser to
 * another site (no open redirect).
 */
export function safeNext(value) {
  if (typeof value !== 'string' || !value || value.length > MAX_NEXT_CHARS) return null;
  if (value[0] !== '/' || value[1] === '/') return null;
  if (/[\\\s\u0000-\u001f\u007f]/.test(value)) return null;
  return value;
}

/** The `next` of the page's query string (`location.search`) when safeNext takes it, else null. */
export function nextFromSearch(search) {
  if (typeof search !== 'string') return null;
  let params;
  try {
    params = new URLSearchParams(search);
  } catch {
    return null;
  }
  return safeNext(params.get('next'));
}

/**
 * Where the page goes after a login: the safe `next` of `search`, else the server root `root`
 * (a URL or its text; the root without a hash, so the server's profile starts on its start tab).
 */
export function afterLoginTarget(search, root) {
  return nextFromSearch(search) || rootHref(root);
}

/** The server root as a link without query and hash. */
export function rootHref(root) {
  const url = new URL(String(root));
  url.search = '';
  url.hash = '';
  return url.href;
}

/** The problem with a pasted token, or null. */
export function tokenProblem(token) {
  const t = typeof token === 'string' ? token.trim() : '';
  if (!t) return 'Paste your token.';
  if (/\s/.test(t)) return 'A token has no spaces: paste it as you got it.';
  return null;
}

/** The help shown under a refused login. */
export const LOGIN_HELP = 'Paste the whole token you were given (a user token, 64 hex digits). '
  + 'A lost token cannot be shown again: ask an admin for a new one.';

/**
 * Logs in with `token`: `POST api/login`; resolves to the principal `{id, kind, admin}`.
 * Rejects with the AdminError of the server (a 401 gets LOGIN_HELP as its help).
 * @param {{base: URL|string, fetch: Function, headers?: Object<string,string>}} options
 */
export async function login(options, token) {
  try {
    const answer = await sendAdmin(options, accessRequest.login(String(token).trim()));
    return principalOf(answer);
  } catch (err) {
    if (err && err.status === 401 && !err.help) err.help = LOGIN_HELP;
    throw err;
  }
}

/** Logs out: `POST api/logout`; resolves to whether a session ended. */
export async function logout(options) {
  const answer = await sendAdmin(options, accessRequest.logout());
  return answer.loggedOut === true;
}

/**
 * The principal `{id, kind, admin}` of a root info answer (or a login answer), anonymous when
 * the answer names none (an older server).
 */
export function principalOf(answer) {
  const p = answer && answer.principal && typeof answer.principal === 'object' ? answer.principal : null;
  if (!p || typeof p.id !== 'string' || typeof p.kind !== 'string') {
    return { id: 'anonymous', kind: 'anonymous', admin: false };
  }
  return { id: p.id, kind: p.kind, admin: p.admin === true };
}

/**
 * What the header shows for `principal`: `{show, name, role, logout}`. Anonymous shows nothing
 * (the page is the login page); a user can log out (its browser session); the operator (a host
 * page sending its token as a header) has no session to end.
 */
export function headerModel(principal) {
  const p = principal || principalOf(null);
  if (p.kind === 'anonymous') return { show: false, name: '', role: '', logout: false };
  return {
    show: true,
    name: p.id,
    role: p.admin ? 'admin' : '',
    logout: p.kind === 'user',
  };
}

/** The words of a level. */
export const LEVEL_TEXTS = Object.freeze({ read: 'read only', write: 'read and write' });

/**
 * The rows of "my databases" from a `GET api/databases` answer: `[{name, href, openapiHref, state,
 * stateText, tone, level, levelText, readOnly, description, aliasOf, open, kind, kindText}]`,
 * sorted by name; `kind` `alias` (with `aliasOf`, its current database) or `database`;
 * `open` = loaded (its playground answers). A malformed entry is skipped. Throws an Error when
 * the answer has no `databases` list.
 */
export function mineRows(answer) {
  if (!answer || typeof answer !== 'object' || !Array.isArray(answer.databases)) {
    throw new Error('the server answered no database list');
  }
  const has = (map, key) => Object.prototype.hasOwnProperty.call(map, key);
  const rows = [];
  for (const d of answer.databases) {
    if (!d || typeof d !== 'object' || typeof d.name !== 'string' || !d.name || typeof d.state !== 'string') continue;
    const level = d.level === 'write' ? 'write' : 'read';
    rows.push({
      name: d.name,
      href: databaseHref(d.name),
      openapiHref: databaseOpenapiHref(d.name),
      state: d.state,
      stateText: has(DATABASE_STATES, d.state) ? DATABASE_STATES[d.state] : d.state,
      tone: has(STATE_TONES, d.state) ? STATE_TONES[d.state] : '',
      level,
      levelText: LEVEL_TEXTS[level],
      readOnly: level === 'read',
      description: typeof d.description === 'string' ? d.description : '',
      aliasOf: typeof d.aliasOf === 'string' && d.aliasOf ? d.aliasOf : null,
      open: isOpenState(d.state),
    });
    const row = rows[rows.length - 1];
    row.kind = row.aliasOf ? 'alias' : 'database';
    row.kindText = mineKindText(row);
  }
  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  return rows;
}

/**
 * What a row of "my databases" names: `alias → <database>` (the database it names now; a grant
 * on the alias follows it) or `database`.
 */
export function mineKindText(row) {
  return row && row.aliasOf ? `alias → ${row.aliasOf}` : 'database';
}

/** Why a row of "my databases" has no link (it is not loaded), or ''. */
export function mineNote(row) {
  if (!row || row.open) return '';
  if (row.state === 'loading') return 'Loading: it opens in a moment.';
  return 'Not loaded: ask an admin to load it.';
}

/** What "my databases" says when the list is empty, per principal. */
export function emptyMineText(principal) {
  return principal && principal.kind === 'anonymous'
    ? 'No database is open to everyone. Sign in to see yours.'
    : 'No database is shared with you yet. Ask an admin for a grant.';
}

/**
 * Fetches "my databases": `GET api/databases`; resolves to mineRows' rows, rejects with the
 * AdminError of the server.
 */
export async function fetchMine(options) {
  return mineRows(await sendAdmin(options, accessRequest.databases()));
}

/**
 * What the profile does with a refused request (an AdminError with `status` and `login`):
 * `'login'` for a 401 that names the login page (the session ended: a revoked token, a logout
 * elsewhere), `'recheck'` for a 404 (the admin API is 404 for a principal without the admin
 * role: it may have just lost it), else null.
 */
export function refusalAction(err) {
  if (!err) return null;
  if (err.status === 401 && typeof err.login === 'string' && safeNext(err.login)) return 'login';
  if (err.status === 404) return 'recheck';
  return null;
}

/**
 * The login page of a 401's `login` (a path from the server root) as a link below `root`, so a
 * proxy prefix in front of the server survives; the root when `login` is not a safe path.
 */
export function loginHref(login, root) {
  const path = safeNext(login);
  const base = rootHref(root);
  return path ? new URL(path.slice(1), base).href : base;
}
