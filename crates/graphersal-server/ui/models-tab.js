// The Models tab of the `server` profile: the embedding model blobs the server keeps once in its
// models/ directory (`GET api/admin/models`: repository, licence, size, the databases holding it,
// the ones waiting for it with "Give to waiting" (`{repo}`), a warning when a database holds a
// copy instead of a hard link), the `needs-model` databases whose model models/ does not hold,
// add (`POST api/admin/models`: a file uploaded from this browser (a .gsnap carrying model blobs
// or a .tar / .tar.gz of a Model2Vec model directory, the raw body through XMLHttpRequest
// for the progress bar like the Import tab, the size checked against the root `/api/info`
// `uploadBytes` (`api.info.host`) first; Cancel aborts it), a model directory, .tar(.gz) or .gsnap on
// the server, or a repository models/ already holds; every add also resolves the waiting
// databases) and remove (`DELETE api/admin/models/<repo>`; refused with help while a database
// uses it). Rules and texts: ./admin-api.js (`modelUploadRequest`, `addModelBody`). The models
// are read again whenever the database list was reloaded, and with the database list on every
// visit of the tab and when the window gets the focus back (./shared.js `useFreshData`; the Add
// dialog keeps its input); both tables sort by any column.

import { reactive, ref, computed, nextTick, watch } from 'vue';
import {
  modelRows, missingRows, removeModelAction, addModelBody, modelUploadRequest, uploadImport,
  uploadLimit, uploadSizeProblem, progressText, confirmText, outcomeText, bytesText,
} from './admin-api.js';
import {
  admin, ROOT, headers, list, reload, errorNotice, noticeClass, useFreshData, useSort, SortTh,
} from './shared.js';

/** The sortable columns of the model table. */
const MODEL_COLUMNS = Object.freeze({
  model: { type: 'text', value: (m) => m.repo },
  size: { type: 'number', value: (m) => m.bytes },
  users: { type: 'text', value: (m) => m.users },
});

/** The sortable columns of the table of databases waiting for a model. */
const MISSING_COLUMNS = Object.freeze({ database: { type: 'text' }, model: { type: 'text' } });
import { hostInfo } from './databases.js';

const freshForm = () => ({ source: 'upload', file: null, path: '', repo: '', license: '' });

export default {
  components: { SortTh },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const models = ref([]);
    const missing = ref([]);
    const error = ref(null);
    const loaded = ref(false);
    const busy = reactive({});
    const notice = ref(null);
    const dlg = ref(null);
    const dialog = reactive({ form: freshForm(), problems: [], error: null, busy: false, loaded: 0, total: 0, sent: false });
    const fileInput = ref(null);
    let running = null;
    const limit = computed(() => uploadLimit(hostInfo(props.api)));
    const fileProblem = computed(() => (dialog.form.source === 'upload' && dialog.form.file
      ? uploadSizeProblem(dialog.form.file.size, limit.value, 'model') : null));
    const percent = computed(() => (dialog.total > 0 ? Math.min(100, (dialog.loaded / dialog.total) * 100) : 0));

    async function load() {
      try {
        const answer = await admin.models();
        models.value = modelRows(answer);
        missing.value = missingRows(answer);
        error.value = null;
      } catch (err) {
        error.value = errorNotice(err);
      } finally {
        loaded.value = true;
      }
    }
    /** After a change: the models and the database list (its states change with a model). */
    async function changed() {
      await load();
      await reload();
    }

    /** Reports a finished add: the resolved databases are loadable now; the still missing ones are named. */
    function addedNotice(answer) {
      const still = Array.isArray(answer.stillMissing) ? answer.stillMissing.filter((m) => m && m.database) : [];
      const notes = Array.isArray(answer.notes) ? answer.notes.filter((n) => typeof n === 'string') : [];
      const lines = [
        ...still.map((m) => `"${m.database}" still waits for ${m.model}.`),
        ...notes,
      ];
      return { tone: still.length ? 'warn' : 'ok', text: outcomeText('addModel', '', answer), help: lines.join('\n') };
    }

    async function remove(m) {
      if (!window.confirm(confirmText('removeModel', m.id))) return;
      busy[m.blob] = true;
      try {
        const answer = await admin.removeModel(m.id);
        notice.value = { tone: 'ok', text: outcomeText('removeModel', m.id, answer), help: '' };
      } catch (err) {
        notice.value = errorNotice(err);
      } finally {
        delete busy[m.blob];
        await changed();
      }
    }
    /** `{repo}` alone: a model models/ already holds resolves the databases waiting for it. */
    async function addByRepo(repo) {
      busy[`repo:${repo}`] = true;
      try {
        notice.value = addedNotice(await admin.addModel({ repo }));
      } catch (err) {
        notice.value = errorNotice(err);
      } finally {
        delete busy[`repo:${repo}`];
        await changed();
      }
    }

    async function openAdd(init = {}) {
      Object.assign(dialog, { form: { ...freshForm(), ...init }, problems: [], error: null, busy: false, loaded: 0, total: 0, sent: false });
      if (fileInput.value) fileInput.value.value = '';
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function closeAdd() {
      if (dialog.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    /** Cancel: aborts a running upload, else closes the dialog. */
    function cancelAdd() {
      if (running) {
        running.abort();
        return;
      }
      closeAdd();
    }
    function pickFile(event) {
      const file = event.target.files && event.target.files[0];
      dialog.form.file = file || null;
    }
    async function submitAdd() {
      if (dialog.busy) return;
      const upload = dialog.form.source === 'upload';
      const built = upload ? modelUploadRequest(dialog.form, limit.value) : addModelBody(dialog.form);
      dialog.problems = built.problems || [];
      if (!built.body && !built.upload) return;
      dialog.busy = true;
      dialog.error = null;
      try {
        let answer;
        if (upload) {
          Object.assign(dialog, { loaded: 0, total: built.upload.size, sent: false });
          running = uploadImport({ base: ROOT, headers, limit: limit.value, kind: 'model' }, built.upload, dialog.form.file, {
            onProgress: (loaded, total) => { dialog.loaded = loaded; dialog.total = total; },
            onSent: () => { dialog.sent = true; },
          });
          answer = await running.promise;
        } else {
          answer = await admin.addModel(built.body);
        }
        dialog.busy = false;
        notice.value = addedNotice(answer);
        closeAdd();
      } catch (err) {
        dialog.error = errorNotice(err);
      } finally {
        dialog.busy = false;
        running = null;
        await changed();
      }
    }

    useFreshData(() => Promise.all([load(), reload()]));
    const modelSort = useSort('models', MODEL_COLUMNS);
    const missingSort = useSort('missing-models', MISSING_COLUMNS);
    const modelList = computed(() => modelSort.rows(models.value));
    const missingList = computed(() => missingSort.rows(missing.value));
    watch(() => list.version, load);
    return {
      models, missing, modelSort, missingSort, modelList, missingList, error, loaded, busy, notice, dlg, dialog, load, remove, addByRepo, openAdd, closeAdd, cancelAdd, submitAdd,
      fileInput, pickFile, limit, fileProblem, percent, progressText,
      bytesText, noticeClass, removeAct: removeModelAction,
      held: (repo) => models.value.some((m) => m.repo === repo),
      shortHash: (h) => h.slice(0, 12),
    };
  },
  template: `
  <div class="srv-tab srv-models">
    <div class="srv-head">
      <h1>Models</h1>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="load">Refresh</button>
      <button class="btn primary" type="button" @click="openAdd()">Add model…</button>
    </div>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" :role="notice.tone === 'ok' ? 'status' : 'alert'">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>The models could not be read: {{ error.text }}</span>
        <p v-if="error.help" class="srv-help">{{ error.help }}</p></div>
    </div>
    <p v-else-if="loaded && !models.length" class="muted">No embedding models. The server keeps one copy of each model in its models/ directory and hard-links it into every database that uses it.</p>
    <div v-if="models.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-model-table">
      <thead><tr><sort-th :s="modelSort" k="model">Model</sort-th><sort-th :s="modelSort" k="size" num>Size</sort-th><sort-th :s="modelSort" k="users">Used by</sort-th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="m in modelList" :key="m.blob" :data-model="m.id">
          <td><span class="mono">{{ m.repo || '(not a model archive)' }}</span>
            <div class="muted srv-sub mono" :title="m.blob">{{ shortHash(m.blob) }}<template v-if="m.license"> · {{ m.license }}</template></div>
            <div v-if="m.copied" class="srv-sub srv-warn" :title="m.linkWarning">Copied, not linked, in {{ m.copiedIn.join(', ') }}<template v-if="m.linkWarning">: {{ m.linkWarning }}</template></div></td>
          <td class="num">{{ bytesText(m.bytes) }}</td>
          <td>{{ m.users.join(', ') || '—' }}
            <div v-if="m.waiting.length" class="muted srv-sub">waiting: {{ m.waiting.join(', ') }}</div></td>
          <td class="srv-actions">
            <button v-if="m.repo && m.waiting.length" type="button" class="btn small" :disabled="!!busy['repo:' + m.repo]" :title="'Give it to ' + m.waiting.join(', ') + ' (they become loadable)'" @click="addByRepo(m.repo)">Give to waiting</button>
            <button type="button" class="btn small danger" :disabled="!!busy[m.blob] || !removeAct(m).enabled" :title="removeAct(m).title" @click="remove(m)">Remove…</button>
          </td>
        </tr>
      </tbody>
    </table>
    </div>
    <template v-if="missing.length">
      <h2 class="srv-sub-head">Databases waiting for a model</h2>
      <div class="srv-table-wrap">
      <table class="data-table srv-table srv-missing-table">
        <thead><tr><sort-th :s="missingSort" k="database">Database</sort-th><sort-th :s="missingSort" k="model">Missing model</sort-th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          <tr v-for="m in missingList" :key="m.database" :data-missing="m.database">
            <td class="mono">{{ m.database }}</td>
            <td class="mono">{{ m.model }}</td>
            <td class="srv-actions">
              <button v-if="held(m.model)" type="button" class="btn small" :disabled="!!busy['repo:' + m.model]" title="models/ holds it: give it to the waiting databases" @click="addByRepo(m.model)">Use from models/</button>
              <button v-else type="button" class="btn small" title="Add it: upload a .gsnap or a .tar / .tar.gz of the model directory, or name one on the server" @click="openAdd({ repo: m.model })">Add…</button>
            </td>
          </tr>
        </tbody>
      </table>
      </div>
    </template>

    <dialog ref="dlg" class="store-dialog srv-dialog" aria-labelledby="srv-model-title" @cancel.prevent="cancelAdd" @click.self="closeAdd">
      <form @submit.prevent="submitAdd">
        <div class="dialog-head">
          <h2 id="srv-model-title">Add a model</h2><span class="spacer"></span>
          <button type="button" class="icon-btn" @click="closeAdd" aria-label="Close">×</button>
        </div>
        <div class="dialog-body srv-form">
          <fieldset :disabled="dialog.busy">
            <legend>From</legend>
            <label class="inline"><input type="radio" value="upload" v-model="dialog.form.source"> A file from this computer (uploaded)</label>
            <label class="inline"><input type="radio" value="path" v-model="dialog.form.source"> A Model2Vec model directory, .tar, .tar.gz or .gsnap on the server's machine</label>
            <label class="inline"><input type="radio" value="repo" v-model="dialog.form.source"> A repository models/ already holds</label>
          </fieldset>
          <label v-if="dialog.form.source === 'upload'">File (.gsnap carrying models, or a .tar or .tar.gz of a model directory)
            <input ref="fileInput" type="file" name="file" accept=".gsnap,.tar,.tar.gz,.tgz" @change="pickFile" :disabled="dialog.busy">
            <span class="muted srv-sub">{{ limit ? 'Up to ' + bytesText(limit) + ' per upload.' : '' }}<template v-if="dialog.form.file"> This file: {{ bytesText(dialog.form.file.size) }}.</template></span></label>
          <p v-if="fileProblem" class="srv-problems" role="alert">{{ fileProblem }}</p>
          <label v-if="dialog.form.source === 'path'">Absolute path
            <input v-model="dialog.form.path" name="path" class="mono" autocomplete="off" spellcheck="false" placeholder="/models/potion-base-8M"></label>
          <label>Repository (owner/name){{ dialog.form.source !== 'repo' ? ', when the directory does not name it' : '' }}
            <input v-model="dialog.form.repo" name="repo" class="mono" autocomplete="off" spellcheck="false" :disabled="dialog.busy"></label>
          <label v-if="dialog.form.source !== 'repo'">Licence (SPDX id, when the directory does not name it)
            <input v-model="dialog.form.license" name="license" class="mono" autocomplete="off" spellcheck="false" :disabled="dialog.busy"></label>
          <p class="muted srv-sub">Every database waiting for the model gets it and can be loaded.</p>
          <div v-if="dialog.busy && dialog.form.source === 'upload'" class="srv-progress" role="status">
            <progress :value="dialog.sent ? 100 : percent" max="100"></progress>
            <span class="muted">{{ dialog.sent ? 'Uploaded; the server is adding the model…' : 'Uploading ' + progressText(dialog.loaded, dialog.total) }}</span>
          </div>
          <ul v-if="dialog.problems.length" class="srv-problems" role="alert"><li v-for="p in dialog.problems" :key="p">{{ p }}</li></ul>
          <div v-if="dialog.error" class="notice srv-notice srv-error" role="alert">
            <div class="srv-notice-text"><span>{{ dialog.error.text }}</span>
              <p v-if="dialog.error.help" class="srv-help">{{ dialog.error.help }}</p></div>
          </div>
          <div class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy || !!fileProblem">{{ dialog.busy ? 'Adding…' : 'Add' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy && (dialog.form.source !== 'upload' || dialog.sent)" @click="cancelAdd">{{ dialog.busy && dialog.form.source === 'upload' && !dialog.sent ? 'Cancel upload' : 'Cancel' }}</button>
          </div>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
