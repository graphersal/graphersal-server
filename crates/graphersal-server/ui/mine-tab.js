// The "My databases" tab of the `server` profile: the page a signed-in user without the admin
// role gets at the server root. `GET api/databases` lists only the databases (and aliases) the
// user may reach, with its level (read only, read and write) and a link to each loaded one's
// playground `db/<name>/`; a database without a grant is never listed. A 401 (the session
// ended) loads the login page (./shared.js `handleRefusal`). The list is read again on every visit
// and when the window gets the focus back (./shared.js `useFreshData`: a grant given meanwhile
// shows up); the table sorts by any column. Each row links the OpenAPI description of the
// database's HTTP API (`db/<name>/openapi.json`, served also while it is not loaded). Model:
// ./access-api.js.

import { reactive, computed } from 'vue';
import { fetchMine, mineNote, emptyMineText, principalOf } from './access-api.js';
import { hostInfo } from './databases.js';
import {
  requestOptions, errorNotice, pageHref, openapiHref, useFreshData, useSort, SortTh,
} from './shared.js';

/** The sortable columns of the table. */
const MINE_COLUMNS = Object.freeze({
  name: { type: 'text' },
  level: { type: 'text', value: (r) => r.levelText },
  state: { type: 'text', value: (r) => r.stateText },
});

export default {
  components: { SortTh },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({ rows: [], read: false, loading: false, error: null });
    const principal = principalOf(hostInfo(props.api));

    async function refresh() {
      s.loading = true;
      try {
        s.rows = await fetchMine(requestOptions);
        s.error = null;
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.loading = false;
        s.read = true;
      }
    }

    useFreshData(refresh);
    const mineSort = useSort('mine', MINE_COLUMNS);
    const rows = computed(() => mineSort.rows(s.rows));
    return { s, rows, mineSort, principal, refresh, mineNote, empty: emptyMineText(principal), href: pageHref, apiHref: openapiHref };
  },
  template: `
  <div class="srv-tab srv-mine">
    <div class="srv-head">
      <h1>My databases</h1>
      <span class="spacer"></span>
      <button type="button" class="btn small" :disabled="s.loading" @click="refresh">Refresh</button>
    </div>
    <p class="muted">The databases you may use. Open one to query it in its playground. "OpenAPI" is the description of its HTTP API: generate a client from it and send your token as <span class="mono">Authorization: Bearer &lt;token&gt;</span>.</p>
    <p v-if="s.read && !s.error && !s.rows.length" class="muted srv-empty">{{ empty }}</p>
    <div v-if="s.rows.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><sort-th :s="mineSort" k="name">Name</sort-th><sort-th :s="mineSort" k="level">Access</sort-th><sort-th :s="mineSort" k="state">State</sort-th></tr></thead>
      <tbody>
        <tr v-for="row in rows" :key="row.name" :data-db="row.name">
          <td><a v-if="row.open" :href="href(row)" class="mono">{{ row.name }}</a><span v-else class="mono">{{ row.name }}</span>
            <a :href="apiHref(row)" class="srv-api-link" target="_blank" rel="noopener" data-openapi title="The OpenAPI description of this database's HTTP API">OpenAPI</a>
            <span class="muted srv-kind" :data-kind="row.kind"> {{ row.kindText }}</span>
            <div v-if="row.description" class="muted srv-sub">{{ row.description }}</div></td>
          <td><span class="badge" :class="row.readOnly ? '' : 'ok'">{{ row.levelText }}</span></td>
          <td><span class="badge" :class="row.tone">{{ row.stateText }}</span>
            <div v-if="mineNote(row)" class="muted srv-sub">{{ mineNote(row) }}</div></td>
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
