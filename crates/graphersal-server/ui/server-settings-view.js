// The server settings view of the Settings tab (round 4 B-SETTINGS), mounted by
// ./settings-tab.js for its first two picker entries: `mode` "defaults" (Server defaults:
// the query limits every database inherits key by key, what a new database starts with, and
// mcp.toml's MCP default of new names) and "server" (every key of server.toml). Each key shows
// its value (or its default), when a change applies (now, at the next load, for new databases,
// after a restart) and "restart pending" while the running server still applies the old value;
// a key is set, or reset to its default, one at a time, based on the file's revision (a change
// someone else made meanwhile is refused, 409, and the view is read again). Outcomes are
// toasts; a refused value shows under its own row. The view is read when it is shown and
// whenever `tick` changes (the tab's fresh-data hook). Model: ./server-settings-api.js.

import { reactive, computed, watch, onMounted } from 'vue';
import {
  createConfig, settingSections, isDefaultsKey, inputText, parseInput, changeSet, pendingKeys, pendingText,
  savedNotice, valueText, runningNote, placeholderText, DEFAULTS_HELP, SERVER_HELP,
} from './server-settings-api.js';
import {
  requestOptions, errorNotice, notify, noticeClass, serverState, useSort, SortTh, FormError,
} from './shared.js';

/** The config client (model: ./server-settings-api.js). */
const config = createConfig(requestOptions);

/** The sortable columns of the key tables (no sort: file order). */
const KEY_COLUMNS = Object.freeze({
  key: { type: 'text' },
  value: { type: 'text', value: (r) => r.text },
  applies: { type: 'text', value: (r) => (r.pending ? 'restart pending' : r.appliesText) },
});

export default {
  components: { FormError, SortTh },
  props: {
    mode: { type: String, default: 'server' },
    tick: { type: Number, default: 0 },
  },
  setup(props) {
    const s = reactive({ answer: null, revision: '', error: null, busy: '', edits: {}, formErrors: {}, read: false });
    const only = computed(() => (props.mode === 'defaults' ? isDefaultsKey : null));
    const keySort = useSort('server-settings', KEY_COLUMNS);
    const sections = computed(() => settingSections(s.answer || {}, only.value)
      .map((sec) => ({ ...sec, rows: keySort.rows(sec.rows) })));
    const pending = computed(() => pendingKeys(s.answer || {}));
    const pendingNote = computed(() => pendingText(s.answer || {}));

    function take(answer) {
      s.answer = answer;
      s.revision = typeof answer.revision === 'string' ? answer.revision : '';
      serverState.restartPending = pendingKeys(answer);
      serverState.read = true;
    }

    async function refresh() {
      try {
        take(await config.view());
        s.error = null;
      } catch (err) {
        s.answer = null;
        s.error = errorNotice(err);
      } finally {
        s.read = true;
      }
    }

    /** Sends `set` for the row `key`, based on the view's revision. */
    async function send(key, set, after = () => {}) {
      s.busy = key;
      delete s.formErrors[key];
      try {
        const answer = await config.change(set, s.revision);
        take(answer);
        notify(savedNotice(answer));
        after();
      } catch (err) {
        s.formErrors[key] = errorNotice(err);
        await refresh();
      } finally {
        s.busy = '';
      }
    }

    function save(row) {
      const parsed = parseInput(row, s.edits[row.key]);
      if (parsed.problem) {
        s.formErrors[row.key] = { tone: 'error', text: parsed.problem, help: '' };
        return;
      }
      send(row.key, changeSet(row.key, parsed.value), () => { delete s.edits[row.key]; });
    }
    function setBool(row, event) {
      send(row.key, changeSet(row.key, !!event.target.checked));
    }
    function setChoice(row, event) {
      send(row.key, changeSet(row.key, String(event.target.value)));
    }
    function reset(row) {
      send(row.key, changeSet(row.key, null), () => { delete s.edits[row.key]; });
    }
    /** The bool a checkbox shows: the file's value, else the default. */
    const boolOf = (row) => (row.set ? row.value === true : row.default === true);
    /** The choice a select shows. */
    const choiceOf = (row) => String(row.set ? row.value : row.default);

    /** mcp.toml's `expose_new` (Server defaults): the MCP default of new databases and aliases. */
    async function setExposeNew(event) {
      const on = !!event.target.checked;
      s.busy = 'mcpExposeNew';
      delete s.formErrors.mcpExposeNew;
      try {
        await config.exposeNew(on);
        notify({ tone: 'ok', text: on ? 'New databases and aliases are offered to MCP.' : 'New databases and aliases are not offered to MCP unless created with "Expose through MCP".', help: 'The names there now keep their MCP settings (MCP tab).' });
        await refresh();
      } catch (err) {
        s.formErrors.mcpExposeNew = errorNotice(err);
        await refresh();
      } finally {
        s.busy = '';
      }
    }

    function editStart(row) {
      if (s.edits[row.key] === undefined) s.edits[row.key] = inputText(row);
    }

    onMounted(refresh);
    watch(() => props.tick, refresh);
    watch(() => props.mode, () => {
      Object.assign(s, { edits: {}, formErrors: {} });
      refresh();
    });

    return {
      s, sections, keySort, pending, pendingNote, noticeClass, valueText, runningNote, placeholderText,
      help: computed(() => (props.mode === 'defaults' ? DEFAULTS_HELP : SERVER_HELP)),
      title: computed(() => (props.mode === 'defaults' ? 'Server defaults for databases' : 'Server settings (server.toml)')),
      refresh, save, setBool, setChoice, reset, boolOf, choiceOf, setExposeNew, editStart,
    };
  },
  template: `
  <div class="srv-server-settings" :data-config-mode="mode">
    <div class="srv-head"><h2>{{ title }}</h2><span class="spacer"></span>
      <span v-if="pending.length" class="badge warn" :title="pendingNote" data-config-pending-badge>restart pending: {{ pending.length }}</span>
      <button type="button" class="btn small" :disabled="!!s.busy" @click="refresh">Refresh</button>
    </div>
    <p class="muted">{{ help }}</p>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span><p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>
    <div v-if="pendingNote" class="notice srv-notice srv-warn-notice" data-config-pending>
      <div class="srv-notice-text"><span>{{ pendingNote }}</span></div>
    </div>
    <template v-if="s.answer">
      <p v-if="mode === 'server'" class="muted">File: <span class="mono">{{ s.answer.path }}</span><template v-if="!s.answer.running"> (no running server answered: changes apply at its next start)</template></p>
      <template v-if="mode === 'defaults'">
        <h3>MCP</h3>
        <div class="srv-form srv-narrow">
          <label class="inline" title="mcp.toml expose_new: whether a new database or alias (create, import, clone, a restore into a new name, repair, a new alias) is offered to MCP when its creator does not say. Applies to names created from now on.">
            <input type="checkbox" :checked="s.answer.mcpExposeNew === true" :disabled="!!s.busy || s.answer.mcpExposeNew === null" data-config-expose-new @change="setExposeNew($event)"> Offer new databases and aliases to MCP</label>
          <span class="badge" title="Read when a database or alias is created; the names there now keep their MCP settings.">new databases</span>
          <form-error :error="s.formErrors.mcpExposeNew || null"></form-error>
        </div>
      </template>
      <template v-for="sec in sections" :key="sec.id">
        <h3>{{ sec.title }}</h3>
        <p v-if="sec.id === 'defaults'" class="muted">What a new database (create, import) starts with. Chunk size and damage policy are creation parameters: applied when a database is created, fixed for its life. Auto-load and read-only are the first value of its own flags (changeable per database afterwards).</p>
        <p v-if="sec.id === 'limits.query'" class="muted">Inherited by every database key by key, unless it overrides the key in its own settings; a change applies at each database's next load.</p>
        <div class="srv-table-wrap">
        <table class="data-table srv-table srv-config-table" :data-config-section="sec.id">
          <thead><tr><sort-th :s="keySort" k="key">Key</sort-th><sort-th :s="keySort" k="value">Value</sort-th><sort-th :s="keySort" k="applies">Applies</sort-th><th><span class="sr-only">Change</span></th></tr></thead>
          <tbody>
            <tr v-for="r in sec.rows" :key="r.key" :data-config-key="r.key">
              <td><span class="mono">{{ r.name }}</span><div class="muted srv-sub">{{ r.help }}</div></td>
              <td><span :class="{ muted: !r.set }">{{ r.text }}</span>
                <div v-if="runningNote(r)" class="srv-sub" :class="{ 'srv-warn': r.pending }" :title="runningNote(r).title" data-running>{{ runningNote(r).text }}</div></td>
              <td><span class="badge" :class="r.pending ? 'warn' : r.appliesTone" :title="r.pending ? 'Saved; the running server applies it at its next start' : r.appliesTitle">{{ r.pending ? 'restart pending' : (r.creation ? 'at create only' : r.appliesText) }}</span></td>
              <td class="srv-actions srv-config-edit-cell">
                <div class="srv-config-edit">
                  <label v-if="r.kind === 'bool'" class="inline"><input type="checkbox" :checked="boolOf(r)" :disabled="!!s.busy" :aria-label="r.key" @change="setBool(r, $event)"> on</label>
                  <select v-else-if="r.choices && r.choices.length" :value="choiceOf(r)" :disabled="!!s.busy" :aria-label="r.key" @change="setChoice(r, $event)">
                    <option v-for="c in r.choices" :key="c" :value="c">{{ c }}</option>
                  </select>
                  <form v-else class="srv-limit-edit" @submit.prevent="save(r)">
                    <input v-model="s.edits[r.key]" class="mono" autocomplete="off" :placeholder="placeholderText(r)" :aria-label="'New value of ' + r.key" @focus="editStart(r)">
                    <button type="submit" class="btn small" :disabled="!!s.busy || s.edits[r.key] === undefined">{{ s.busy === r.key ? 'Saving…' : 'Set' }}</button>
                  </form>
                  <button v-if="r.set" type="button" class="btn small" :disabled="!!s.busy" title="Remove the key from server.toml: its default applies" @click="reset(r)">Reset to default</button>
                </div>
                <form-error :error="s.formErrors[r.key] || null"></form-error>
              </td>
            </tr>
          </tbody>
        </table>
        </div>
      </template>
    </template>
    <p v-else-if="s.read && !s.error" class="muted">No answer.</p>
  </div>
  `,
};
