// The Database settings tab of the `server` profile (admins only, round 3 B7): everything
// about one database from `GET api/admin/databases/<name>/settings`, grouped (identity, state
// and sizes, query limits, MCP, grants, backups, damage and recovery (round 3 B-CRASH: the
// maintenance state with its damage report, the last unclean shutdown's check of it), the
// Store's creation parameters), and the
// edit of what can change at runtime: the description, read-only, auto-load and the query limit
// overrides (set or clear each; a limit applies at the database's next load, which the tab
// says and offers to do). The database comes from the tab's address parameter `db` (the
// Databases tab's row button "Settings…") or the picker. Every change sends the view's
// revision: a change someone else made meanwhile is refused (409) and the tab reads the view
// again. The view is read on every visit of the tab and when the window gets the focus back
// (./shared.js `useFreshData`; an open edit keeps its input); every table sorts by any column.
// Requests, rules and rows: ./settings-api.js.

import { reactive, computed, watch } from 'vue';
import {
  createSettings, limitRows, limitsNote, limitValueProblem, limitSetBody, limitClearBody, limitText,
  descriptionProblem, identityRows, stateRows, storeRows, settingsGrantRows, settingsMcpRows, mcpText,
  backupsText, changedText, readOnlyLocked, recoveryView, verifyView, MAX_DESCRIPTION_CHARS,
} from './settings-api.js';
import { realRows, BASE_IS_FROZEN } from './admin-api.js';
import {
  admin, list, reload, requestOptions, errorNotice, notify, noticeClass, useFreshData, useSort, SortTh, FormError,
} from './shared.js';

/** The settings client (model: ./settings-api.js). */
const settings = createSettings(requestOptions);

/** The sortable columns of the limits table (null values sort last). */
const LIMIT_COLUMNS = Object.freeze({
  key: { type: 'text' },
  override: { type: 'number' },
  server: { type: 'number' },
  effective: { type: 'number' },
  source: { type: 'text', value: (r) => r.sourceText },
  inEffect: { type: 'number' },
});

/** The sortable columns of the grants table. */
const GRANT_COLUMNS = Object.freeze({
  subject: { type: 'text' },
  on: { type: 'text' },
  level: { type: 'text' },
  via: { type: 'text', value: (g) => g.viaText },
});

/** The sortable columns of the MCP table. */
const MCP_COLUMNS = Object.freeze({
  name: { type: 'text' },
  offered: { type: 'bool' },
  readOnly: { type: 'bool' },
  sessions: { type: 'number' },
  calls: { type: 'number' },
});

export default {
  components: { SortTh, FormError },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({
      name: '', picked: '', answer: null, revision: '', read: false, error: null, busy: '',
      description: '', editingDescription: false, edits: {}, formErrors: {},
    });

    const names = computed(() => realRows(list.rows).map((r) => r.name));
    const limitSort = useSort('settings-limits', LIMIT_COLUMNS);
    const grantSort = useSort('settings-grants', GRANT_COLUMNS);
    const mcpSort = useSort('settings-mcp', MCP_COLUMNS);
    const limits = computed(() => limitSort.rows(limitRows(s.answer || {})));
    const grants = computed(() => grantSort.rows(settingsGrantRows(s.answer || {})));
    const mcpNames = computed(() => mcpSort.rows(settingsMcpRows(s.answer || {})));
    const loaded = computed(() => !!(s.answer && s.answer.limits && s.answer.limits.loaded));
    const db = computed(() => (s.answer && s.answer.database) || {});

    async function refresh() {
      if (!s.name) return;
      try {
        const answer = await settings.view(s.name);
        s.answer = answer;
        s.revision = typeof answer.revision === 'string' ? answer.revision : '';
        if (!s.editingDescription) s.description = (answer.database && answer.database.description) || '';
        s.error = null;
      } catch (err) {
        s.answer = null;
        s.error = errorNotice(err);
      } finally {
        s.read = true;
      }
    }

    /** Shows the settings of `name` ('' = none chosen). */
    async function choose(name) {
      Object.assign(s, {
        name: name || '', picked: name || '', answer: null, revision: '', read: false, error: null, busy: '',
        description: '', editingDescription: false, edits: {}, formErrors: {},
      });
      await refresh();
    }
    function pick() {
      const name = String(s.picked || '').trim().toLowerCase();
      if (name === (props.params.db || '')) choose(name);
      else props.api.navigate('settings', name ? { db: name } : {});
    }

    /** Shows the error of the form `key` ('description', 'limit:<key>') under its button, or as
     * a toast for a change without a form of its own (a flag). */
    function failed(key, notice) {
      if (key === 'description' || key.startsWith('limit:')) s.formErrors[key] = notice;
      else notify(notice);
    }

    /** One change of the database's settings, based on the view's revision. */
    async function change(key, body, after = () => {}) {
      if (!s.answer) return;
      s.busy = key;
      delete s.formErrors[key];
      const name = s.answer.name;
      try {
        const answer = await settings.change(name, body, s.revision);
        s.answer = answer;
        s.revision = typeof answer.revision === 'string' ? answer.revision : '';
        notify({ tone: 'ok', text: changedText(name, answer), help: '' });
        after();
        reload();
      } catch (err) {
        // A refused stale change says so; the view is read again either way.
        failed(key, errorNotice(err));
        await refresh();
      } finally {
        s.busy = '';
      }
    }

    function saveDescription() {
      const problem = descriptionProblem(s.description);
      if (problem) {
        failed('description', { tone: 'error', text: problem, help: '' });
        return;
      }
      change('description', { description: s.description }, () => { s.editingDescription = false; });
    }
    function setFlag(field, event) {
      const value = !!event.target.checked;
      if (field === 'readOnly' && !value && readOnlyLocked(s.answer || {})) return;
      change(field, { [field]: value });
    }
    function setLimit(row) {
      const text = s.edits[row.key];
      const problem = limitValueProblem(row.key, text);
      if (problem) {
        failed(`limit:${row.key}`, { tone: 'error', text: problem, help: '' });
        return;
      }
      change(`limit:${row.key}`, limitSetBody(row.key, text), () => { delete s.edits[row.key]; });
    }
    function clearLimit(row) {
      change(`limit:${row.key}`, limitClearBody(row.key), () => { delete s.edits[row.key]; });
    }

    /** Unloads and loads the database, so the changed limits apply now. */
    async function reloadNow() {
      const name = s.answer && s.answer.name;
      if (!name) return;
      if (!window.confirm(`Unload and load "${name}" now? Its open requests finish first; it is unavailable while it loads.`)) return;
      s.busy = 'reload';
      try {
        await admin.unload(name);
        await admin.load(name);
        notify({ tone: 'ok', text: `${name}: loading with the new limits.`, help: '' });
      } catch (err) {
        notify(errorNotice(err));
      } finally {
        s.busy = '';
        await reload();
        await refresh();
      }
    }

    watch(() => props.params.db, (name) => choose(name || ''));
    // The first visit chooses the address's database; every later one reads it again (an open
    // edit keeps its input).
    let chosen = false;
    useFreshData(async () => {
      await reload();
      if (!chosen) {
        chosen = true;
        await choose(props.params.db || '');
      } else {
        await refresh();
      }
    });

    return {
      s, db, names, limits, grants, mcpNames, loaded, limitSort, grantSort, mcpSort,
      identityRows, stateRows, storeRows, recoveryView, verifyView, limitsNote, limitText, mcpText, backupsText, readOnlyLocked, noticeClass,
      maxDescription: MAX_DESCRIPTION_CHARS, baseFrozen: BASE_IS_FROZEN,
      refresh, pick, saveDescription, setFlag, setLimit, clearLimit, reloadNow,
      openMcp: () => props.api.navigate('mcp'),
      openUsers: () => props.api.navigate('users'),
      openModels: () => props.api.navigate('models'),
      openBackups: () => props.api.navigate('backups', { db: s.answer ? s.answer.name : s.name }),
    };
  },
  template: `
  <div class="srv-tab srv-settings">
    <div class="srv-head"><h1>{{ s.answer ? 'Settings of "' + s.answer.name + '"' : 'Database settings' }}</h1>
      <span class="spacer"></span>
      <button v-if="s.name" type="button" class="btn small" :disabled="!!s.busy" @click="refresh">Refresh</button>
    </div>
    <form class="srv-inline-form srv-form" @submit.prevent="pick">
      <label>Database
        <select v-model="s.picked" name="database" data-settings-pick @change="pick">
          <option value="">— choose —</option>
          <option v-for="n in names" :key="n" :value="n">{{ n }}</option>
          <option v-if="s.picked && !names.includes(s.picked)" :value="s.picked">{{ s.picked }}</option>
        </select></label>
    </form>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>
    <p v-if="!s.name" class="muted">Choose a database to see and change its settings.</p>

    <template v-if="s.answer">
    <p v-if="s.answer.requested" class="muted">"{{ s.answer.requested }}" is an alias of "{{ s.answer.name }}": these are the settings of the database itself.</p>

    <div class="srv-head"><h2>Identity</h2></div>
    <dl class="srv-facts" data-settings-identity>
      <template v-for="r in identityRows(s.answer)" :key="r[0]"><dt>{{ r[0] }}</dt><dd :title="r[2] || null" class="mono">{{ r[1] }}</dd></template>
    </dl>
    <form class="srv-form srv-narrow" @submit.prevent="saveDescription">
      <label>Description
        <input v-model="s.description" name="description" :maxlength="maxDescription" autocomplete="off" data-settings-description
               @input="s.editingDescription = true"></label>
      <div class="store-dialog-buttons">
        <button type="submit" class="btn small primary" :disabled="!!s.busy || !s.editingDescription">{{ s.busy === 'description' ? 'Saving…' : 'Save description' }}</button>
        <button v-if="s.editingDescription" type="button" class="btn small" @click="s.editingDescription = false; s.description = db.description || ''; s.formErrors.description = null">Cancel</button>
      </div>
      <form-error :error="s.formErrors.description || null"></form-error>
    </form>

    <div class="srv-head"><h2>State and sizes</h2></div>
    <dl class="srv-facts" data-settings-state>
      <template v-for="r in stateRows(s.answer)" :key="r[0]"><dt>{{ r[0] }}</dt><dd :title="r[2] || null">{{ r[1] }}</dd></template>
    </dl>
    <div class="srv-form srv-narrow">
      <label class="inline" :title="readOnlyLocked(s.answer) ? baseFrozen : 'Served read-only: every write is refused. Applies at once, also to a loaded database.'">
        <input type="checkbox" :checked="db.readOnly" :disabled="!!s.busy || readOnlyLocked(s.answer)" data-settings-read-only @change="setFlag('readOnly', $event)"> Read-only</label>
      <label class="inline" title="Loaded when the server starts.">
        <input type="checkbox" :checked="db.autoLoad" :disabled="!!s.busy" data-settings-auto-load @change="setFlag('autoLoad', $event)"> Load at server start</label>
    </div>

    <div class="srv-head"><h2>Query limits</h2></div>
    <p class="muted">Each limit of this database overrides the server's default (server.toml [limits.query]); clearing an override lets the default apply again. A set limit is a ceiling: a query may lower it for itself, never raise it.</p>
    <div class="notice srv-notice" :class="noticeClass(limitsNote(s.answer))" data-settings-limits-note>
      <div class="srv-notice-text"><span>{{ limitsNote(s.answer).text }}</span>
        <p v-if="limitsNote(s.answer).help" class="srv-help">{{ limitsNote(s.answer).help }}</p></div>
      <button v-if="s.answer.limits && s.answer.limits.reloadNeeded" type="button" class="btn small" :disabled="!!s.busy" data-settings-reload @click="reloadNow">{{ s.busy === 'reload' ? 'Reloading…' : 'Unload and load now…' }}</button>
    </div>
    <p v-if="s.answer.limits && s.answer.limits.serverError" class="muted">server.toml: {{ s.answer.limits.serverError }}</p>
    <div class="srv-table-wrap">
    <table class="data-table srv-table srv-settings-limits">
      <thead><tr><sort-th :s="limitSort" k="key">Limit</sort-th><sort-th :s="limitSort" k="override" num title="This database's own value (database.toml)">This database</sort-th><sort-th :s="limitSort" k="server" num title="server.toml [limits.query]">Server default</sort-th><sort-th :s="limitSort" k="effective" num title="What the next load uses">Next load</sort-th><sort-th :s="limitSort" k="source">From</sort-th><sort-th v-if="loaded" :s="limitSort" k="inEffect" num title="What the loaded database runs under now">Now</sort-th><th><span class="sr-only">Change</span></th></tr></thead>
      <tbody>
        <tr v-for="r in limits" :key="r.key" :data-limit="r.key" :class="{ 'srv-differs': r.differs }">
          <td :title="r.help"><span class="mono">{{ r.key }}</span><div class="muted srv-sub">{{ r.label }}<template v-if="r.unit">, {{ r.unit }}</template>; 0 = {{ r.zero }}</div></td>
          <td class="num">{{ limitText(r.key, r.override) }}</td>
          <td class="num">{{ limitText(r.key, r.server) }}</td>
          <td class="num">{{ limitText(r.key, r.effective) }}</td>
          <td>{{ r.sourceText }}</td>
          <td v-if="loaded" class="num" :title="r.differs ? 'Differs from the next load: unload and load the database to apply' : null">{{ limitText(r.key, r.inEffect) }}</td>
          <td class="srv-actions srv-limit-edit-cell">
            <form class="srv-limit-edit" @submit.prevent="setLimit(r)">
              <input v-model="s.edits[r.key]" class="mono" inputmode="numeric" :placeholder="r.override === null ? 'value' : String(r.override)" :aria-label="'New value of ' + r.key" autocomplete="off">
              <button type="submit" class="btn small" :disabled="!!s.busy || s.edits[r.key] === undefined || s.edits[r.key] === ''">Set</button>
              <button type="button" class="btn small" :disabled="!!s.busy || r.override === null" title="Remove this database's override: the server's default applies" @click="clearLimit(r)">Clear</button>
            </form>
            <form-error :error="s.formErrors['limit:' + r.key] || null"></form-error>
          </td>
        </tr>
      </tbody>
    </table>
    </div>

    <div class="srv-head"><h2>MCP</h2><span class="spacer"></span><button type="button" class="btn small" @click="openMcp">MCP tab…</button></div>
    <p class="muted" data-settings-mcp>{{ mcpText(s.answer) }}</p>
    <div v-if="mcpNames.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="mcpSort" k="name">Name</sort-th><sort-th :s="mcpSort" k="offered">Offered</sort-th><sort-th :s="mcpSort" k="readOnly">Read-only for MCP</sort-th><sort-th :s="mcpSort" k="sessions" num>Sessions</sort-th><sort-th :s="mcpSort" k="calls" num>Calls</sort-th></tr></thead>
      <tbody><tr v-for="r in mcpNames" :key="r.name"><td class="mono">{{ r.name }} <span v-if="r.kind === 'alias'" class="badge">alias</span></td><td>{{ r.offered ? 'yes' : 'no' }}</td><td>{{ r.readOnly ? 'yes' : 'no' }}</td><td class="num">{{ r.sessions }}</td><td class="num">{{ r.calls }}</td></tr></tbody>
    </table>
    </div>

    <div class="srv-head"><h2>Grants</h2><span class="spacer"></span><button type="button" class="btn small" @click="openUsers">Users tab…</button></div>
    <p class="muted">Grants on this database, on an alias naming it (reaches it only by the alias's name) and on every database (*). Admins reach every database: {{ (s.answer.admins || []).join(', ') || 'none' }}.</p>
    <p v-if="!grants.length" class="muted" data-settings-no-grants>No grants.</p>
    <div v-else class="srv-table-wrap">
    <table class="data-table srv-table" data-settings-grants>
      <thead><tr><sort-th :s="grantSort" k="subject">Subject</sort-th><sort-th :s="grantSort" k="on">On</sort-th><sort-th :s="grantSort" k="level">Level</sort-th><sort-th :s="grantSort" k="via">Through</sort-th></tr></thead>
      <tbody><tr v-for="g in grants" :key="g.key"><td class="mono">{{ g.subject }}</td><td class="mono">{{ g.on }}</td><td>{{ g.level }}</td><td>{{ g.viaText }}</td></tr></tbody>
    </table>
    </div>

    <div class="srv-head"><h2>Backups</h2><span class="spacer"></span><button type="button" class="btn small" @click="openBackups">Backups tab…</button></div>
    <p class="muted" data-settings-backups>{{ backupsText(s.answer) }}</p>

    <div class="srv-head"><h2>Damage and recovery</h2></div>
    <div data-settings-recovery>
      <div v-if="recoveryView(s.answer).maintenance" class="notice srv-notice srv-error" role="alert">
        <div class="srv-notice-text"><span>Maintenance: its Store is damaged, so it is served read-only (queries read the intact data, writes are refused). Repair it into a new database (Databases tab: Repair…), or restore a backup.</span>
          <details class="srv-damage"><summary>Damage report</summary><pre class="mono">{{ recoveryView(s.answer).damage }}</pre></details></div>
      </div>
      <div class="srv-verify" :class="verifyView(s.answer).tone ? 'srv-verify-' + verifyView(s.answer).tone : ''" data-settings-verify>
        <p :class="verifyView(s.answer).tone ? null : 'muted'">{{ verifyView(s.answer).text }}</p>
        <ul v-if="verifyView(s.answer).problems.length" class="srv-problems mono">
          <li v-for="(p, i) in verifyView(s.answer).problems" :key="i">{{ p }}</li>
          <li v-if="verifyView(s.answer).more" class="muted">… and {{ verifyView(s.answer).more }} more</li>
        </ul>
        <p v-if="verifyView(s.answer).remedy" class="srv-help">{{ verifyView(s.answer).remedy }} <a v-if="verifyView(s.answer).tone === 'warn'" href="#models" @click.prevent="openModels">Models tab…</a></p>
      </div>
      <template v-if="recoveryView(s.answer).unclean">
        <p>Checked after the last unclean shutdown{{ recoveryView(s.answer).unclean.when ? ' (' + recoveryView(s.answer).unclean.when + ')' : '' }}{{ recoveryView(s.answer).unclean.pid ? ', the server with pid ' + recoveryView(s.answer).unclean.pid + ' was killed or crashed' : '' }}:</p>
        <dl class="srv-facts">
          <template v-for="r in recoveryView(s.answer).unclean.rows" :key="r[0]"><dt>{{ r[0] }}</dt><dd>{{ r[1] }}</dd></template>
        </dl>
      </template>
    </div>

    <div class="srv-head"><h2>Store</h2></div>
    <p class="muted">Creation parameters: fixed when the Store was created, shown read-only.</p>
    <dl class="srv-facts" data-settings-store>
      <template v-for="r in storeRows(s.answer)" :key="r[0]"><dt>{{ r[0] }}</dt><dd :title="r[2] || null" class="mono">{{ r[1] }}</dd></template>
    </dl>
    </template>
    <p v-else-if="s.read && s.name && !s.error" class="muted">No answer.</p>
  </div>
  `,
};
