// The Export tab of the `server` profile: downloads a packed snapshot (`.gsnap`) of a database's
// latest state from `GET .../export` (./admin-api.js `exportPath`), with or without the
// embedding models' blobs and, for a layer, as a layer snapshot (only its changes) or
// materialized (a standalone full snapshot). The database comes from the tab's address parameter
// `db` (the Databases tab's row button "Export…") or the select.
//
// Without host headers the download is a plain link (the browser streams the file to disk, so a
// large snapshot never sits in memory; a refusal shows as a failed download). A link cannot
// carry a header, so with host headers (`window.GRAPHERSAL_ENGINE.headers`) the file is fetched
// with them, kept as a Blob and saved; a refusal then shows the server's `error`/`help` here.

import { reactive, ref, computed, watch, onMounted } from 'vue';
import { exportPath, parseAdminError, AdminError, realRows, transferActions } from './admin-api.js';
import { downloadBlob } from './databases.js';
import { ROOT, headers, list, ensureLoaded, noticeClass } from './shared.js';

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({ name: '', models: true, materialize: false, busy: false, error: null });
    const notice = ref(null);
    const rows = computed(() => realRows(list.rows));
    const row = computed(() => rows.value.find((r) => r.name === s.name) || null);
    const layer = computed(() => !!row.value && row.value.kind === 'layer');
    const allowed = computed(() => (row.value ? transferActions(row.value).export : { enabled: false, title: '' }));
    const withHeaders = Object.keys(headers).length > 0;
    const target = computed(() => exportPath(s.name, { models: s.models, materialize: s.materialize, layer: layer.value }));
    const href = computed(() => new URL(target.value.path, ROOT).href);
    // A layer snapshot never carries models: the box applies only to a full snapshot.
    const modelsApply = computed(() => !layer.value || s.materialize);

    function choose(name) {
      Object.assign(s, { name: name || '', models: true, materialize: false, busy: false, error: null });
    }
    function picked() {
      props.api.navigate('export', s.name ? { db: s.name } : {});
    }
    function linkClicked() {
      notice.value = { tone: 'ok', text: `The download of ${target.value.fileName} started.`, help: '' };
    }
    async function fetchAndSave() {
      s.busy = true;
      s.error = null;
      notice.value = null;
      try {
        let r;
        try {
          r = await fetch(href.value, { cache: 'no-store', headers: { ...headers } });
        } catch (err) {
          throw new AdminError(`The server could not be reached: ${err && err.message ? err.message : err}`);
        }
        if (!r.ok) throw parseAdminError(r.status, await r.text());
        downloadBlob(await r.blob(), target.value.fileName);
        notice.value = { tone: 'ok', text: `Saved ${target.value.fileName}.`, help: '' };
      } catch (err) {
        s.error = { text: err && err.message ? err.message : String(err), help: err && err.help ? err.help : '' };
      } finally {
        s.busy = false;
      }
    }

    watch(() => props.params.db, (db) => choose(db || ''));
    onMounted(async () => {
      choose(props.params.db || '');
      await ensureLoaded();
    });

    return { s, notice, loaded: computed(() => list.loaded), rows, row, layer, allowed, withHeaders, target, href, modelsApply, picked, linkClicked, fetchAndSave, noticeClass };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>{{ s.name ? 'Export "' + s.name + '"' : 'Export' }}</h1></div>
    <form class="srv-form srv-narrow" @submit.prevent>
      <p class="muted">A packed snapshot (.gsnap) of a database's latest state; another server imports it.</p>
      <label>Database
        <select v-model="s.name" name="database" @change="picked">
          <option value="">(choose a database)</option>
          <option v-for="r in rows" :key="r.name" :value="r.name">{{ r.name }}</option>
        </select></label>
      <p v-if="s.name && loaded && !row" class="srv-problems" role="alert">No database "{{ s.name }}" on this server.</p>
      <p v-else-if="row && !allowed.enabled" class="srv-problems" role="alert">{{ allowed.title }}</p>
      <template v-if="row && allowed.enabled">
        <fieldset v-if="layer">
          <legend>Layer</legend>
          <label class="inline"><input type="radio" :value="false" v-model="s.materialize"> Layer snapshot: only the layer's changes (loads only over the same base)</label>
          <label class="inline"><input type="radio" :value="true" v-model="s.materialize"> Materialized: a standalone snapshot of everything the layer shows</label>
        </fieldset>
        <label class="inline" :title="modelsApply ? '' : 'A layer snapshot never carries models'">
          <input type="checkbox" name="models" v-model="s.models" :disabled="!modelsApply"> With the embedding models' files
          <span class="muted">(without: the models stay named; the importing server needs them)</span></label>
        <div class="store-dialog-buttons">
          <a v-if="!withHeaders" class="btn primary" :href="href" :download="target.fileName" @click="linkClicked">Download {{ target.fileName }}</a>
          <button v-else type="button" class="btn primary" :disabled="s.busy" @click="fetchAndSave">{{ s.busy ? 'Downloading…' : 'Download ' + target.fileName }}</button>
        </div>
      </template>
      <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
        <div class="srv-notice-text"><span>{{ s.error.text }}</span>
          <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
      </div>
      <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" role="status">
        <div class="srv-notice-text"><span>{{ notice.text }}</span></div>
        <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
      </div>
    </form>
  </div>
  `,
};
