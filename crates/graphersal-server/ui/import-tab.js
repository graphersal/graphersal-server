// The Import tab of the `server` profile: a new database from a file uploaded from this browser
// (`POST api/admin/import?name=...` with the file as the raw body, through XMLHttpRequest for the
// progress bar; the size is checked against the root `/api/info` `uploadBytes`, which the
// playground passes as `api.info.host`, before anything is sent) or from a path on the server's machine (the JSON form). Sources: a packed snapshot
// (full or layer), a Store, GraphSON or GraphML; the format is detected unless chosen. Request
// and rules: ./admin-api.js `importRequest`, `uploadImport`. The server's `error`/`help` (a `413`
// included) stay shown; Cancel aborts a running upload. "Import as a base" sends `role=base`
// (only a plain source becomes a base: the server refuses a layer source with `400` and a source
// missing its model with `409` + help).

import { reactive, ref, computed, onMounted } from 'vue';
import {
  importRequest, uploadImport, uploadLimit, uploadSizeProblem, progressText, suggestName,
  outcomeText, needsModelHint, bytesText, baseNames, IMPORT_FORMATS, BASE_IS_FROZEN,
} from './admin-api.js';
import { admin, ROOT, headers, list, ensureLoaded, reload, noticeClass } from './shared.js';
import { hostInfo } from './databases.js';

const freshForm = () => ({ source: 'upload', file: null, path: '', name: '', base: '', format: '', description: '', role: 'normal' });

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const form = reactive(freshForm());
    const s = reactive({ busy: false, problems: [], error: null, loaded: 0, total: 0, sent: false });
    const notice = ref(null);
    const fileInput = ref(null);
    let running = null;
    let nameTouched = false;

    const bases = computed(() => baseNames(list.rows));
    const limit = computed(() => uploadLimit(hostInfo(props.api)));
    const fileProblem = computed(() => (form.source === 'upload' && form.file ? uploadSizeProblem(form.file.size, limit.value) : null));
    const percent = computed(() => (s.total > 0 ? Math.min(100, (s.loaded / s.total) * 100) : 0));

    function reset() {
      Object.assign(form, freshForm());
      Object.assign(s, { busy: false, problems: [], error: null, loaded: 0, total: 0, sent: false });
      nameTouched = false;
      if (fileInput.value) fileInput.value.value = '';
    }
    function pickFile(event) {
      const file = event.target.files && event.target.files[0];
      form.file = file || null;
      if (file && !nameTouched) form.name = suggestName(file.name);
    }
    function pathChanged() {
      if (!nameTouched) form.name = suggestName(form.path);
    }

    function finished(answer, subject) {
      const imp = answer.import && typeof answer.import === 'object' ? answer.import : {};
      const notes = Array.isArray(imp.notes) ? imp.notes.filter((n) => typeof n === 'string') : [];
      const hint = needsModelHint(answer);
      notice.value = {
        tone: answer.state === 'needs-model' ? 'warn' : 'ok',
        text: outcomeText('import', subject, answer),
        help: [hint, ...notes].filter(Boolean).join('\n'),
      };
    }

    async function submit() {
      if (s.busy) return;
      const built = importRequest(form, limit.value);
      s.problems = built.problems || [];
      s.error = null;
      if (built.problems) return;
      const subject = form.name.trim().toLowerCase();
      s.busy = true;
      notice.value = null;
      try {
        let answer;
        if (built.upload) {
          Object.assign(s, { loaded: 0, total: built.upload.size, sent: false });
          running = uploadImport({ base: ROOT, headers, limit: limit.value }, built.upload, form.file, {
            onProgress: (loaded, total) => { s.loaded = loaded; s.total = total; },
            onSent: () => { s.sent = true; },
          });
          answer = await running.promise;
        } else {
          answer = await admin.importPath(built.request.body);
        }
        s.busy = false;
        finished(answer || {}, subject);
        reset();
        await reload();
      } catch (err) {
        s.error = { text: err && err.message ? err.message : String(err), help: err && err.help ? err.help : '' };
      } finally {
        s.busy = false;
        running = null;
      }
    }
    function cancel() {
      if (running) {
        running.abort();
        return;
      }
      reset();
    }

    onMounted(ensureLoaded);

    return {
      form, s, notice, fileInput, bases, limit, fileProblem, percent, cancel, submit, pickFile, pathChanged, noticeClass,
      progressText, bytesText, formats: IMPORT_FORMATS, frozen: BASE_IS_FROZEN,
      touchName: () => { nameTouched = true; },
      openDatabases: () => props.api.navigate('databases'),
      openModels: () => props.api.navigate('models'),
    };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>Import a database</h1></div>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" :role="notice.tone === 'ok' ? 'status' : 'alert'">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p>
        <p class="srv-help"><a href="#" @click.prevent="openDatabases">Databases</a><template v-if="notice.tone === 'warn'"> · <a href="#" @click.prevent="openModels">Models</a></template></p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <form class="srv-form srv-narrow" @submit.prevent="submit">
      <fieldset :disabled="s.busy">
        <legend>Source</legend>
        <label class="inline"><input type="radio" value="upload" v-model="form.source"> A file from this computer (uploaded)</label>
        <label class="inline"><input type="radio" value="path" v-model="form.source"> A file or Store directory on the server's machine</label>
      </fieldset>
      <label v-if="form.source === 'upload'">File (.gsnap, .gstore, GraphSON, GraphML)
        <input ref="fileInput" type="file" name="file" @change="pickFile" :disabled="s.busy">
        <span class="muted srv-sub">{{ limit ? 'Up to ' + bytesText(limit) + ' per upload.' : '' }}<template v-if="form.file"> This file: {{ bytesText(form.file.size) }}.</template></span></label>
      <p v-if="fileProblem" class="srv-problems" role="alert">{{ fileProblem }}</p>
      <label v-if="form.source === 'path'">Absolute path on the server
        <input v-model="form.path" name="path" class="mono" autocomplete="off" spellcheck="false" placeholder="/data/acme.gsnap" @change="pathChanged" :disabled="s.busy"></label>
      <label>Name of the new database
        <input v-model="form.name" name="name" class="mono" autocomplete="off" spellcheck="false" @input="touchName" :disabled="s.busy"></label>
      <label>Format
        <select v-model="form.format" name="format" :disabled="s.busy">
          <option v-for="f in formats" :key="f" :value="f">{{ f || 'detected from the content' }}</option>
        </select></label>
      <label class="inline"><input type="checkbox" name="role" :checked="form.role === 'base'" :disabled="s.busy"
          @change="form.role = $event.target.checked ? 'base' : 'normal'">
        Import as a base (from a full snapshot, a plain Store, GraphSON or GraphML; layers are built over it)</label>
      <p v-if="form.role === 'base'" class="muted srv-sub">{{ frozen }}</p>
      <label v-if="bases.length && form.role !== 'base'">Base (a layer snapshot or layer Store; empty: the base with its identity)
        <select v-model="form.base" name="base" :disabled="s.busy">
          <option value="">(found by the snapshot's base identity)</option>
          <option v-for="b in bases" :key="b" :value="b">{{ b }}</option>
        </select></label>
      <label>Description <input v-model="form.description" name="description" autocomplete="off" :disabled="s.busy"></label>
      <div v-if="s.busy && form.source === 'upload'" class="srv-progress" role="status">
        <progress :value="s.sent ? 100 : percent" max="100"></progress>
        <span class="muted">{{ s.sent ? 'Uploaded; the server is importing it…' : 'Uploading ' + progressText(s.loaded, s.total) }}</span>
      </div>
      <p v-else-if="s.busy" class="muted" role="status">The server is importing it…</p>
      <ul v-if="s.problems.length" class="srv-problems" role="alert"><li v-for="p in s.problems" :key="p">{{ p }}</li></ul>
      <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
        <div class="srv-notice-text"><span>{{ s.error.text }}</span>
          <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
      </div>
      <div class="store-dialog-buttons">
        <button type="submit" class="btn primary" :disabled="s.busy || !!fileProblem">{{ s.busy ? 'Importing…' : 'Import' }}</button>
        <button type="button" class="btn" :disabled="s.busy && (form.source !== 'upload' || s.sent)" @click="cancel">{{ s.busy && form.source === 'upload' && !s.sent ? 'Cancel upload' : 'Clear' }}</button>
      </div>
    </form>
  </div>
  `,
};
