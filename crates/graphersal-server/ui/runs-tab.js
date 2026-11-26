// The Runs tab of the `server` profile: the run administration. Every query that waits or runs
// on a loaded database, whatever its client (a browser page under db/<name>/, an MCP agent, a
// script), from `GET api/admin/runs` (all loaded databases) or `GET api/admin/databases/<name>/runs`
// (the tab's address parameter `db`, the Databases tab's row button "Runs…", or the filter),
// read again every RUNS_POLL_MS while the tab is shown (the poll stops while another tab is
// shown and starts again, with a read at once, on the next visit: ./shared.js `useFreshData`); "Stop" cancels one run by its id
// (`POST api/admin/databases/<name>/runs/cancel`). Rules and texts: ./admin-api.js. The server's
// `error`/`help` stay shown.

import { reactive, ref, computed, watch, onDeactivated, onUnmounted } from 'vue';
import {
  runRows, durationText, runTextHead, cancelRunAction, confirmText, outcomeText, realRows, RUNS_POLL_MS, isOpenState,
} from './admin-api.js';
import {
  admin, list, reload, errorNotice, noticeClass, useFreshData, useSort, SortTh,
} from './shared.js';

/** The sortable columns of the run table. */
const RUN_COLUMNS = Object.freeze({
  database: { type: 'text' },
  id: { type: 'number' },
  client: { type: 'text' },
  state: { type: 'text', value: (r) => (r.cancelled ? 'stopping' : r.state) },
  duration: { type: 'number', value: (r) => r.durationMs },
  text: { type: 'text' },
});

export default {
  components: { SortTh },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({
      db: '', runs: [], textHeadChars: 200, loaded: true, read: false, error: null, busy: '',
    });
    const notice = ref(null);
    let timer = null;
    let alive = true;

    const loadedNames = computed(() => realRows(list.rows).filter((r) => isOpenState(r.state)).map((r) => r.name));

    async function refresh() {
      clearTimeout(timer);
      try {
        const answer = await admin.runs(s.db || undefined);
        s.runs = runRows(answer);
        s.textHeadChars = Number.isInteger(answer.textHeadChars) ? answer.textHeadChars : 200;
        s.loaded = s.db ? answer.loaded !== false : true;
        s.read = true;
        s.error = null;
      } catch (err) {
        s.error = errorNotice(err);
      }
      clearTimeout(timer);
      if (alive && fresh.active) timer = setTimeout(refresh, RUNS_POLL_MS);
    }

    function choose(db) {
      s.db = db || '';
      s.runs = [];
      s.read = false;
      s.error = null;
      refresh();
    }
    function filter(event) {
      const db = event.target.value;
      if (db === (props.params.db || '')) choose(db);
      else props.api.navigate('runs', db ? { db } : {});
    }

    async function stop(run) {
      if (!window.confirm(confirmText('cancelRun', run.database, String(run.id)))) return;
      s.busy = `${run.database}/${run.id}`;
      notice.value = null;
      try {
        const answer = await admin.cancelRun(run.database, run.id);
        notice.value = { tone: answer.cancelled ? 'ok' : 'warn', text: outcomeText('cancelRun', run.database, answer), help: '' };
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.busy = '';
      }
      await refresh();
    }

    watch(() => props.params.db, (db) => choose(db || ''));
    // The first visit chooses the address's database; every later one reads the runs (and the
    // loaded databases of the filter) again and starts the poll.
    let chosen = false;
    const fresh = useFreshData(async () => {
      await reload();
      if (!chosen) {
        chosen = true;
        choose(props.params.db || '');
      } else {
        await refresh();
      }
    });
    onDeactivated(() => clearTimeout(timer));
    const runSort = useSort('runs', RUN_COLUMNS);
    const runs = computed(() => runSort.rows(s.runs));
    onUnmounted(() => {
      alive = false;
      clearTimeout(timer);
    });

    return {
      s, runs, runSort, notice, loadedNames, refresh, filter, stop, noticeClass, durationText,
      textOf: (run) => runTextHead(run, s.textHeadChars),
      stopAction: (run) => cancelRunAction(run, { busy: !!s.busy }),
      openDb: (db) => props.api.navigate('runs', { db }),
    };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>{{ s.db ? 'Runs of "' + s.db + '"' : 'Runs' }}</h1></div>
    <div class="srv-bar">
      <label>Database
        <select name="database" :value="s.db" @change="filter">
          <option value="">All loaded databases</option>
          <option v-for="n in loadedNames" :key="n" :value="n">{{ n }}</option>
          <option v-if="s.db && !loadedNames.includes(s.db)" :value="s.db">{{ s.db }}</option>
        </select></label>
      <span class="muted">Every client's queries that wait or run; refreshed every 2 s.</span>
      <span class="spacer"></span>
      <button type="button" class="btn small" @click="refresh">Refresh</button>
    </div>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" role="status">
      <div class="srv-notice-text"><span>{{ notice.text }}</span></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <p v-if="s.read && s.db && !s.loaded" class="muted">"{{ s.db }}" is not loaded: nothing runs on it.</p>
    <p v-else-if="s.read && !s.runs.length" class="muted">Nothing runs now.</p>
    <div v-if="s.runs.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-runs">
      <thead><tr><sort-th :s="runSort" k="database">Database</sort-th><sort-th :s="runSort" k="id" num>Run</sort-th><sort-th :s="runSort" k="client">Client</sort-th><sort-th :s="runSort" k="state">State</sort-th><sort-th :s="runSort" k="duration" num>Duration</sort-th><sort-th :s="runSort" k="text">Query</sort-th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="r in runs" :key="r.database + '/' + r.id" :data-run="r.database + '/' + r.id">
          <td><a href="#" @click.prevent="openDb(r.database)">{{ r.database }}</a></td>
          <td class="num mono">{{ r.id }}<div v-if="r.alias" class="muted srv-sub">{{ r.alias }}</div></td>
          <td class="mono">{{ r.client }}<div class="muted srv-sub">{{ r.kind }}</div></td>
          <td>{{ r.cancelled ? 'stopping' : r.state }}</td>
          <td class="num">{{ durationText(r.durationMs) }}</td>
          <td class="mono srv-run-text" :title="r.text">{{ textOf(r) }}</td>
          <td class="srv-actions"><button type="button" class="btn small" :disabled="!stopAction(r).enabled" :title="stopAction(r).title" @click="stop(r)">Stop</button></td>
        </tr>
      </tbody>
    </table>
    </div>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>
  </div>
  `,
};
