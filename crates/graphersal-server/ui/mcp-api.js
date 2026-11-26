// The MCP tab's model (no Vue): the requests of the admin API's MCP routes (`GET/PUT
// api/admin/mcp`, `PUT api/admin/mcp/databases/<name>`; book/src/reference/admin_api.md "MCP"), the rows of
// its tables, the status text and how to connect an agent. Imports only ./admin-api.js.

import { sendAdmin } from './admin-api.js';

const seg = (name) => encodeURIComponent(String(name));

/** `request` based on `revision` (sent as `If-Match`): unchanged without one. */
const based = (request, revision) => (typeof revision === 'string' && revision ? { ...request, ifMatch: revision } : request);

/**
 * The requests: `state()` (the switches, every database's MCP settings and the usage
 * statistics), `setEnabled(on, revision?)` (MCP for the whole server), `setExposeNew(on,
 * revision?)` (whether new databases and aliases are offered to MCP), `setName(name,
 * {exposed?, readOnly?}, revision?)` (one database or alias). A change takes the state's
 * `revision`: a change someone else made meanwhile refuses it (409).
 */
export const mcpRequest = Object.freeze({
  state: () => ({ method: 'GET', path: 'api/admin/mcp' }),
  setEnabled: (on, revision) => based({ method: 'PUT', path: 'api/admin/mcp', body: { enabled: !!on } }, revision),
  setExposeNew: (on, revision) => based({ method: 'PUT', path: 'api/admin/mcp', body: { exposeNew: !!on } }, revision),
  setName: (name, change, revision) => {
    const body = {};
    if (typeof change.exposed === 'boolean') body.exposed = change.exposed;
    if (typeof change.readOnly === 'boolean') body.readOnly = change.readOnly;
    return based({ method: 'PUT', path: `api/admin/mcp/databases/${seg(name)}`, body }, revision);
  },
});

/** An MCP client over `sendAdmin`'s options: one method per request of `mcpRequest`. */
export function createMcp(options) {
  return {
    state: () => sendAdmin(options, mcpRequest.state()),
    setEnabled: (on, revision) => sendAdmin(options, mcpRequest.setEnabled(on, revision)),
    setExposeNew: (on, revision) => sendAdmin(options, mcpRequest.setExposeNew(on, revision)),
    setName: (name, change, revision) => sendAdmin(options, mcpRequest.setName(name, change, revision)),
  };
}

const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
const str = (v) => (typeof v === 'string' ? v : '');
const arr = (v) => (Array.isArray(v) ? v : []);

/**
 * The question before MCP is switched off for the whole server (round 4: says exactly what the
 * router does: every MCP request is answered 503, the session table is not touched).
 */
export const MCP_OFF_CONFIRM = 'Switch MCP off for the whole server?\n\n'
  + 'While it is off, every MCP request is refused with 503, also the requests of agents that are connected now: '
  + 'no agent can use a database through MCP.\n\n'
  + 'Their sessions are not ended: when MCP is switched on again, connected agents continue with the same session id '
  + 'and the same selected database, without connecting again.\n\n'
  + 'The HTTP API and the web pages are not affected.';

/** The status of the answer: `{tone, text, help}` (on, switched off, or no endpoint at all). */
export function statusOf(answer = {}) {
  if (answer.endpointBuilt === false) {
    return {
      tone: 'warn',
      text: 'MCP is off in server.toml',
      help: 'server.toml [mcp] enabled = false: the server built no MCP endpoint. Set it to true and restart the server; the switch below changes nothing until then.',
    };
  }
  if (answer.enabled === false) {
    return {
      tone: 'warn',
      text: 'MCP is switched off',
      help: '/mcp and every /db/<name>/mcp answer 503 to every request, also to the agents connected before: none of them reaches a database through MCP now. Their sessions are kept (not ended): when MCP is switched on again they continue with the same session id and the same selected database, without connecting again.',
    };
  }
  return { tone: 'ok', text: 'MCP is on', help: '' };
}

/** Whether new databases and aliases are offered to MCP (`exposeNew`; default and missing: no). */
export const exposeNewOf = (answer = {}) => answer.exposeNew === true;

/** The tooltip of the "Expose new databases and aliases" checkbox. */
export const EXPOSE_NEW_HELP = 'Whether a database or alias created from now on (create, import, clone, a restore into a new name, repair, a new alias) is offered to MCP. '
  + 'Off (the default): it is hidden until it is exposed here, or created with "Expose through MCP". '
  + 'The databases and aliases there now keep their settings.';

/** The kinds of a database row: a database, an alias, or a name with settings that names nothing now. */
export const MCP_ROW_KINDS = Object.freeze(['database', 'alias', 'missing']);

/** The per-database rows of the answer, each with `nameText` (an alias: `alias → target`). */
export function mcpRows(answer = {}) {
  return arr(answer.databases)
    .filter((r) => r && typeof r.name === 'string')
    .map((r) => {
      const kind = MCP_ROW_KINDS.includes(r.kind) ? r.kind : 'database';
      const target = kind === 'alias' ? str(r.target) : '';
      return {
        name: r.name,
        kind,
        target,
        nameText: kind === 'alias' ? `${r.name} → ${target}` : kind === 'missing' ? `${r.name} (no such database)` : r.name,
        exposed: r.exposed !== false,
        readOnly: r.readOnly === true,
        effectiveExposed: r.effectiveExposed !== false,
        effectiveReadOnly: r.effectiveReadOnly === true,
        sessions: num(r.sessions),
        calls: num(r.calls),
        errors: num(r.errors),
      };
    });
}

/** The tooltip of a row's "Offered" cell. */
export function exposedTitle(row) {
  if (!row.exposed) return 'Hidden from MCP: list_graphs does not show it, use_graph and "graph" answer as for an unknown name, its /db/<name>/mcp answers 404.';
  if (!row.effectiveExposed) return `Offered itself, but its database ${row.target} is hidden from MCP, so it is too.`;
  return 'Offered to MCP agents granted access to it.';
}

/** The tooltip of a row's "Read-only" cell. */
export function readOnlyTitle(row) {
  if (row.readOnly) return 'MCP sessions only read it, whatever their grant (the HTTP API still follows the grant).';
  if (row.effectiveReadOnly) return 'Read-only for MCP anyway: its database is read-only (a flag, a base, or MCP read-only on the database).';
  return 'MCP sessions write when their grant allows it.';
}

/** The usage tables of the answer (`usage` null: offline, no statistics). */
export function usageOf(answer = {}) {
  const u = answer.usage;
  if (!u || typeof u !== 'object') return null;
  const counters = (rows, key) => arr(rows)
    .filter((r) => r && typeof r[key] === 'string')
    .map((r) => ({ name: r[key], calls: num(r.calls), errors: num(r.errors) }));
  const sessions = u.sessions || {};
  return {
    startedAtMs: num(u.startedAtMs),
    open: num(sessions.open),
    withoutDatabase: num(sessions.withoutDatabase),
    sessionsByUser: arr(sessions.byUser).filter((r) => r && typeof r.user === 'string').map((r) => ({ name: r.user, sessions: num(r.sessions) })),
    calls: num(u.calls && u.calls.total),
    errors: num(u.calls && u.calls.errors),
    refused: num(u.calls && u.calls.refused),
    tools: counters(u.tools, 'tool'),
    users: counters(u.users, 'user'),
    recentErrors: arr(u.recentErrors).filter((e) => e && typeof e.message === 'string').map((e, i) => ({
      key: `${num(e.atMs)}-${i}`,
      atMs: num(e.atMs),
      time: num(e.atMs) ? new Date(num(e.atMs)).toISOString().replace('T', ' ').replace(/\.\d+Z$/, ' UTC') : '',
      tool: str(e.tool),
      user: str(e.user),
      database: str(e.database),
      message: e.message,
    })),
    recentErrorsMax: num(u.recentErrorsMax),
  };
}

/** The one-line summary of the usage. */
export function usageText(usage) {
  if (!usage) return 'The statistics exist only while the server runs.';
  const s = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
  return `${s(usage.open, 'open session')}, ${s(usage.calls, 'tool call')} (${s(usage.errors, 'error')}, ${s(usage.refused, 'request')} refused while off) since the server started`;
}

/** The placeholder of a token in the connect commands. */
export const TOKEN_PLACEHOLDER = '<token>';

/**
 * How to connect an agent: the endpoint's absolute URL on the server root `root`, the
 * `claude mcp add` command with a placeholder token, and the command of one database's endpoint.
 */
export function connectInfo(root, endpoint = '/mcp') {
  const base = String(root).replace(/\/+$/, '');
  const url = `${base}/${String(endpoint || '/mcp').replace(/^\/+/, '')}`;
  const command = (name, target) => `claude mcp add --transport http --header "Authorization: Bearer ${TOKEN_PLACEHOLDER}" ${name} ${target}`;
  return {
    url,
    command: command('graphersal', url),
    database: (name) => command(name, `${base}/db/${seg(name)}/mcp`),
  };
}
