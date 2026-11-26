// The MCP tab of the `server` profile (admins only): the state of the server's MCP endpoints
// from `GET api/admin/mcp` (on, switched off, or no endpoint because server.toml turns MCP
// off), the global switch, the per-database table (offered to MCP, read-only for MCP, open
// sessions, tool calls; an alias narrows its database, never widens it), the usage statistics
// since the server started (open sessions per user, tool calls per tool and per user, the
// recent errors) and how to connect an agent (the endpoint URL and the `claude mcp add`
// command with a placeholder token). Every change sends the state's revision: a change
// someone else made meanwhile is refused (409) and the tab reads the state again. The state is
// read on every visit of the tab and when the window gets the focus back (./shared.js
// `useFreshData`); every table sorts by any column. Requests, rows and texts: ./mcp-api.js.

import { reactive, ref, computed } from 'vue';
import {
  createMcp, statusOf, mcpRows, usageOf, usageText, exposedTitle, readOnlyTitle, connectInfo,
} from './mcp-api.js';
import {
  ROOT, requestOptions, errorNotice, noticeClass, useFreshData, useSort, SortTh,
} from './shared.js';

/** The MCP client (model: ./mcp-api.js). */
const mcp = createMcp(requestOptions);

/** The sortable columns of the database table. */
const DB_COLUMNS = Object.freeze({
  name: { type: 'text', value: (r) => r.nameText },
  exposed: { type: 'bool', value: (r) => r.effectiveExposed },
  readOnly: { type: 'bool', value: (r) => r.effectiveReadOnly },
  sessions: { type: 'number' },
  calls: { type: 'number' },
  errors: { type: 'number' },
});

/** The sortable columns of the counter tables (tools, users). */
const COUNTER_COLUMNS = Object.freeze({
  name: { type: 'text' },
  calls: { type: 'number' },
  errors: { type: 'number' },
});

/** The sortable columns of the sessions-per-user table. */
const SESSION_COLUMNS = Object.freeze({ name: { type: 'text' }, sessions: { type: 'number' } });

/** The sortable columns of the recent errors. */
const ERROR_COLUMNS = Object.freeze({
  time: { type: 'number', value: (e) => e.atMs },
  tool: { type: 'text' },
  user: { type: 'text' },
  database: { type: 'text' },
  message: { type: 'text' },
});

export default {
  components: { SortTh },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup() {
    const s = reactive({ answer: null, revision: '', read: false, error: null });
    const busy = reactive({});
    const notice = ref(null);
    const copied = ref('');

    const status = computed(() => statusOf(s.answer || {}));
    const rows = computed(() => mcpRows(s.answer || {}));
    const usage = computed(() => usageOf(s.answer || {}));
    const connect = computed(() => connectInfo(ROOT.href, (s.answer && s.answer.endpoint) || '/mcp'));
    const dbSort = useSort('mcp-databases', DB_COLUMNS);
    const toolSort = useSort('mcp-tools', COUNTER_COLUMNS);
    const userSort = useSort('mcp-users', COUNTER_COLUMNS);
    const sessionSort = useSort('mcp-sessions', SESSION_COLUMNS);
    const errorSort = useSort('mcp-errors', ERROR_COLUMNS);
    const databases = computed(() => dbSort.rows(rows.value));
    const tools = computed(() => toolSort.rows(usage.value ? usage.value.tools : []));
    const users = computed(() => userSort.rows(usage.value ? usage.value.users : []));
    const sessions = computed(() => sessionSort.rows(usage.value ? usage.value.sessionsByUser : []));
    const errors = computed(() => errorSort.rows(usage.value ? usage.value.recentErrors : []));

    async function refresh() {
      try {
        const answer = await mcp.state();
        s.answer = answer;
        s.revision = typeof answer.revision === 'string' ? answer.revision : '';
        s.error = null;
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.read = true;
      }
    }

    /** Runs one change under the busy marker `key`; a refused stale change reads the state again. */
    async function run(key, text, fn) {
      busy[key] = true;
      notice.value = null;
      try {
        await fn();
        notice.value = { tone: 'ok', text, help: '' };
      } catch (err) {
        // A refused stale change says so; the state is read again either way.
        notice.value = errorNotice(err);
      } finally {
        delete busy[key];
        await refresh();
      }
    }

    function toggleMcp() {
      const on = !(s.answer && s.answer.enabled !== false);
      if (!on && !window.confirm('Switch MCP off for the whole server? Every MCP request is refused (503) until it is switched on again; open sessions are kept.')) return;
      run('global', on ? 'MCP is switched on.' : 'MCP is switched off.', () => mcp.setEnabled(on, s.revision));
    }
    const toggleExposed = (r) => run(r.name, `${r.name}: ${r.exposed ? 'hidden from' : 'offered to'} MCP.`,
      () => mcp.setName(r.name, { exposed: !r.exposed }, s.revision));
    const toggleReadOnly = (r) => run(r.name, `${r.name}: ${r.readOnly ? 'writable' : 'read-only'} for MCP.`,
      () => mcp.setName(r.name, { readOnly: !r.readOnly }, s.revision));

    async function copy(text, key) {
      try {
        await navigator.clipboard.writeText(text);
        copied.value = key;
      } catch {
        copied.value = '';
      }
    }

    useFreshData(refresh);

    return {
      s, busy, notice, copied, status, rows, usage, connect, databases, tools, users, sessions, errors,
      dbSort, toolSort, userSort, sessionSort, errorSort, noticeClass, usageText, exposedTitle, readOnlyTitle,
      refresh, toggleMcp, toggleExposed, toggleReadOnly, copy,
    };
  },
  template: `
  <div class="srv-tab srv-mcp">
    <div class="srv-head">
      <h1>MCP</h1>
      <span class="spacer"></span>
      <button type="button" class="btn small" @click="refresh">Refresh</button>
    </div>
    <p class="muted">AI agents reach the databases over the Model Context Protocol: <span class="mono">/mcp</span> serves every database the agent's token is granted, <span class="mono">/db/&lt;name&gt;/mcp</span> one database. Here MCP is switched on or off, databases are hidden from MCP or made read-only for it, and its use is counted.</p>

    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" role="status">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>

    <template v-if="s.answer">
    <div class="notice srv-notice srv-mcp-status" :class="noticeClass(status)" role="status" data-mcp-status>
      <div class="srv-notice-text"><span>{{ status.text }}</span>
        <p v-if="status.help" class="srv-help">{{ status.help }}</p></div>
      <button type="button" class="btn small" :class="s.answer.enabled === false ? 'primary' : 'danger'" :disabled="!!busy.global" data-mcp-switch @click="toggleMcp">{{ s.answer.enabled === false ? 'Switch MCP on' : 'Switch MCP off…' }}</button>
    </div>

    <div class="srv-head"><h2>Databases</h2></div>
    <p class="muted">A database or alias hidden from MCP is not listed to agents and answers like an unknown name. Read-only for MCP: agents only read it, whatever their grant (the HTTP API still follows the grant). An alias can narrow its database, never widen it. Sessions and calls count since the server started.</p>
    <p v-if="!rows.length" class="muted">No databases.</p>
    <div v-else class="srv-table-wrap">
    <table class="data-table srv-table srv-mcp-databases">
      <thead><tr><sort-th :s="dbSort" k="name">Name</sort-th><sort-th :s="dbSort" k="exposed">Offered</sort-th><sort-th :s="dbSort" k="readOnly">Read-only</sort-th><sort-th :s="dbSort" k="sessions" num>Sessions</sort-th><sort-th :s="dbSort" k="calls" num>Calls</sort-th><sort-th :s="dbSort" k="errors" num>Errors</sort-th></tr></thead>
      <tbody>
        <tr v-for="r in databases" :key="r.name" :data-mcp-db="r.name">
          <td class="mono">{{ r.nameText }} <span v-if="r.kind === 'alias'" class="badge">alias</span></td>
          <td :title="exposedTitle(r)"><label class="inline"><input type="checkbox" :checked="r.exposed" :disabled="!!busy[r.name]" data-mcp-exposed @change="toggleExposed(r)"> {{ r.effectiveExposed ? 'yes' : 'no' }}</label></td>
          <td :title="readOnlyTitle(r)"><label class="inline"><input type="checkbox" :checked="r.readOnly" :disabled="!!busy[r.name]" data-mcp-read-only @change="toggleReadOnly(r)"> {{ r.effectiveReadOnly ? 'yes' : 'no' }}</label></td>
          <td class="num">{{ r.sessions }}</td>
          <td class="num">{{ r.calls }}</td>
          <td class="num">{{ r.errors }}</td>
        </tr>
      </tbody>
    </table>
    </div>

    <div class="srv-head"><h2>Usage</h2></div>
    <p class="muted" data-mcp-usage>{{ usageText(usage) }}</p>
    <template v-if="usage">
    <div v-if="sessions.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="sessionSort" k="name">User</sort-th><sort-th :s="sessionSort" k="sessions" num>Open sessions</sort-th></tr></thead>
      <tbody><tr v-for="r in sessions" :key="r.name"><td class="mono">{{ r.name }}</td><td class="num">{{ r.sessions }}</td></tr></tbody>
    </table>
    </div>
    <div v-if="tools.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="toolSort" k="name">Tool</sort-th><sort-th :s="toolSort" k="calls" num>Calls</sort-th><sort-th :s="toolSort" k="errors" num>Errors</sort-th></tr></thead>
      <tbody><tr v-for="r in tools" :key="r.name"><td class="mono">{{ r.name }}</td><td class="num">{{ r.calls }}</td><td class="num">{{ r.errors }}</td></tr></tbody>
    </table>
    </div>
    <div v-if="users.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="userSort" k="name">User</sort-th><sort-th :s="userSort" k="calls" num>Calls</sort-th><sort-th :s="userSort" k="errors" num>Errors</sort-th></tr></thead>
      <tbody><tr v-for="r in users" :key="r.name"><td class="mono">{{ r.name }}</td><td class="num">{{ r.calls }}</td><td class="num">{{ r.errors }}</td></tr></tbody>
    </table>
    </div>
    <div class="srv-head"><h2>Recent errors</h2></div>
    <p v-if="!errors.length" class="muted">No tool errors since the server started.</p>
    <div v-else class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="errorSort" k="time">Time</sort-th><sort-th :s="errorSort" k="tool">Tool</sort-th><sort-th :s="errorSort" k="user">User</sort-th><sort-th :s="errorSort" k="database">Database</sort-th><sort-th :s="errorSort" k="message">Message</sort-th></tr></thead>
      <tbody><tr v-for="e in errors" :key="e.key"><td class="mono">{{ e.time }}</td><td class="mono">{{ e.tool }}</td><td class="mono">{{ e.user }}</td><td class="mono">{{ e.database }}</td><td>{{ e.message }}</td></tr></tbody>
    </table>
    </div>
    </template>

    <div class="srv-head"><h2>Connect an agent</h2></div>
    <p class="muted">Give the agent a user token (Users tab: add a user, grant it databases) and put it in place of <span class="mono">&lt;token&gt;</span>.</p>
    <div class="srv-form srv-narrow">
      <label>Endpoint <span class="srv-token-row"><input class="mono" readonly :value="connect.url" aria-label="Endpoint" @focus="$event.target.select()"><button type="button" class="btn small" @click="copy(connect.url, 'url')">{{ copied === 'url' ? 'Copied' : 'Copy' }}</button></span></label>
      <label>Claude Code <span class="srv-token-row"><input class="mono" readonly :value="connect.command" aria-label="claude mcp add command" @focus="$event.target.select()"><button type="button" class="btn small" @click="copy(connect.command, 'command')">{{ copied === 'command' ? 'Copied' : 'Copy' }}</button></span></label>
      <small class="srv-field-hint">One database only: the same with its endpoint <span class="mono">/db/&lt;name&gt;/mcp</span>.</small>
    </div>
    </template>
    <p v-else-if="s.read && !s.error" class="muted">No answer.</p>
  </div>
  `,
};
