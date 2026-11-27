// The Import tab of the `server` profile: a new database from a file uploaded from this browser
// (`POST api/admin/import?name=...` with the file as the raw body, through XMLHttpRequest for the
// progress bar; the size is checked against the root `/api/info` `uploadBytes`, which the
// playground passes as `api.info.host`, before anything is sent), from the server's import inbox
// (`imports/` of the server directory, listed by `GET api/admin/imports`: pick one entry, sent as
// `{inbox}`; ./admin-api.js `inboxRows`) or from a path on the server's machine (the JSON form). Sources: a packed snapshot
// (full or layer), a Store, GraphSON or GraphML; the format is detected unless chosen. Request
// and rules: ./admin-api.js `importRequest`, `uploadImport`. The server's `error`/`help` (a `413`
// included) stay shown; Cancel aborts a running upload. "Import as a base" sends `role=base`
// (only a plain source becomes a base: the server refuses a layer source with `400` and a source
// missing its model with `409` + help). The database list (bases) and, with the inbox chosen, the
// inbox are read again on every visit and when the window gets the focus back (./shared.js
// `useFreshData`; the form keeps its input); the inbox table sorts by any column. KBFIX: "Move
// backup jobs under the backup root" sends `rebaseJobs`; the outcome names the stored jobs that
// write outside the backup root (`import.jobsOutside`: moved, or never run) as a warning.

import { reactive, ref, computed, watch } from 'vue';
import {
  importRequest, uploadImport, uploadLimit, uploadSizeProblem, progressText, suggestName, NAME_HINT,
  outcomeText, outcomeTone, needsModelHint, bytesText, baseNames, inboxRows, IMPORT_FORMATS, BASE_IS_FROZEN,
  MCP_NEW_HELP, REBASE_JOBS_HELP,
} from './admin-api.js';
import {
  admin, ROOT, headers, list, reload, notify, useFreshData, useSort, SortTh, FormError,
} from './shared.js';

/** The sortable columns of the inbox table. */
const INBOX_COLUMNS = Object.freeze({
  file: { type: 'text', value: (r) => r.label },
  kind: { type: 'text', value: (r) => r.kindText },
  size: { type: 'number', value: (r) => r.bytes },
  modified: { type: 'date' },
});
import { hostInfo } from './databases.js';

const freshForm = (source = 'upload') => ({ source, file: null, path: '', inbox: '', name: '', base: '', format: '', description: '', role: 'normal', mcp: false, rebaseJobs: false });

export default {
  components: { SortTh, FormError },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const form = reactive(freshForm());
    const s = reactive({ busy: false, problems: [], error: null, loaded: 0, total: 0, sent: false });
    const inbox = reactive({ dir: '', rows: [], loading: false, error: null, loaded: false });
    const fileInput = ref(null);
    let running = null;
    let nameTouched = false;

    const bases = computed(() => baseNames(list.rows));
    const limit = computed(() => uploadLimit(hostInfo(props.api)));
    const fileProblem = computed(() => (form.source === 'upload' && form.file ? uploadSizeProblem(form.file.size, limit.value) : null));
    const percent = computed(() => (s.total > 0 ? Math.min(100, (s.loaded / s.total) * 100) : 0));

    async function loadInbox() {
      if (inbox.loading) return;
      inbox.loading = true;
      inbox.error = null;
      try {
        const listed = inboxRows(await admin.imports());
        inbox.dir = listed.dir;
        inbox.rows = listed.rows;
        if (form.inbox && !listed.rows.some((r) => r.name === form.inbox)) form.inbox = '';
      } catch (err) {
        inbox.error = { text: err && err.message ? err.message : String(err), help: err && err.help ? err.help : '' };
      } finally {
        inbox.loading = false;
        inbox.loaded = true;
      }
    }
    function pickInbox(row) {
      form.inbox = row.name;
      if (!nameTouched) form.name = suggestName(row.name);
    }
    watch(() => form.source, (source) => { if (source === 'inbox') loadInbox(); });

    function reset() {
      // The source stays chosen: an admin importing from the inbox picks the next file.
      Object.assign(form, freshForm(form.source));
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
      const jobs = Array.isArray(imp.jobsOutside) ? imp.jobsOutside : [];
      const warn = answer.state === 'needs-model' || jobs.some((j) => j && j.willRun !== true);
      notify({
        tone: warn ? 'warn' : outcomeTone(answer),
        text: outcomeText('import', subject, answer),
        help: [hint, ...notes].filter(Boolean).join('\n'),
        links: [
          { text: 'Databases', run: () => props.api.navigate('databases') },
          ...(answer.state === 'needs-model' ? [{ text: 'Models', run: () => props.api.navigate('models') }] : []),
        ],
      });
    }

    async function submit() {
      if (s.busy) return;
      const built = importRequest(form, limit.value);
      s.problems = built.problems || [];
      s.error = null;
      if (built.problems) return;
      const subject = form.name.trim().toLowerCase();
      s.busy = true;
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
        if (form.source === 'inbox') await loadInbox();
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

    useFreshData(async () => {
      await Promise.all([reload(), form.source === 'inbox' ? loadInbox() : null]);
    });
    const inboxSort = useSort('inbox', INBOX_COLUMNS);
    const inboxList = computed(() => inboxSort.rows(inbox.rows));

    return {
      form, s, inbox, inboxSort, inboxList, loadInbox, pickInbox, fileInput, bases, limit, fileProblem, percent, cancel, submit, pickFile, pathChanged,
      progressText, bytesText, formats: IMPORT_FORMATS, frozen: BASE_IS_FROZEN, nameHint: NAME_HINT, mcpHelp: MCP_NEW_HELP, rebaseHelp: REBASE_JOBS_HELP,
      touchName: () => { nameTouched = true; },
      openDatabases: () => props.api.navigate('databases'),
      openModels: () => props.api.navigate('models'),
    };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>Import a database</h1></div>
    <form class="srv-form srv-narrow" @submit.prevent="submit">
      <fieldset :disabled="s.busy">
        <legend>Source</legend>
        <label class="inline"><input type="radio" value="upload" v-model="form.source"> A file from this computer (uploaded)</label>
        <label class="inline"><input type="radio" value="inbox" v-model="form.source"> A file of the server's import inbox (imports/)</label>
        <label class="inline"><input type="radio" value="path" v-model="form.source"> A file or Store directory on the server's machine</label>
      </fieldset>
      <label v-if="form.source === 'upload'">File (.gsnap, .gstore, GraphSON, GraphML)
        <input ref="fileInput" type="file" name="file" @change="pickFile" :disabled="s.busy">
        <span class="muted srv-sub">{{ limit ? 'Up to ' + bytesText(limit) + ' per upload.' : '' }}<template v-if="form.file"> This file: {{ bytesText(form.file.size) }}.</template></span></label>
      <p v-if="fileProblem" class="srv-problems" role="alert">{{ fileProblem }}</p>
      <fieldset v-if="form.source === 'inbox'" class="srv-inbox" :disabled="s.busy">
        <legend>Import inbox</legend>
        <p class="muted srv-sub">Files put into <span class="mono">{{ inbox.dir || 'imports/' }}</span> on the server (a .gsnap, a .gstore or Store directory, GraphSON, GraphML).
          <button type="button" class="btn" @click="loadInbox" :disabled="inbox.loading">{{ inbox.loading ? 'Reading…' : 'Refresh' }}</button></p>
        <p v-if="inbox.error" class="srv-problems" role="alert">{{ inbox.error.text }}<template v-if="inbox.error.help"> {{ inbox.error.help }}</template></p>
        <p v-else-if="inbox.loaded && !inbox.rows.length" class="muted">The inbox is empty.</p>
        <div v-else-if="inbox.rows.length" class="srv-table-wrap"><table class="data-table srv-table">
          <thead><tr><th><span class="sr-only">Pick</span></th><sort-th :s="inboxSort" k="file">File</sort-th><sort-th :s="inboxSort" k="kind">Kind</sort-th><sort-th :s="inboxSort" k="size" num>Size</sort-th><sort-th :s="inboxSort" k="modified">Modified</sort-th></tr></thead>
          <tbody>
            <tr v-for="r in inboxList" :key="r.name">
              <td><input type="radio" name="inbox" :value="r.name" :checked="form.inbox === r.name" :disabled="!r.importable" @change="pickInbox(r)" :aria-label="'Import ' + r.name"></td>
              <td class="mono">{{ r.label }}</td><td>{{ r.kindText }}</td><td class="num">{{ r.sizeText }}</td><td class="muted">{{ r.modified }}</td>
            </tr>
          </tbody>
        </table></div>
      </fieldset>
      <label v-if="form.source === 'path'">Absolute path on the server
        <input v-model="form.path" name="path" class="mono" autocomplete="off" spellcheck="false" placeholder="/data/acme.gsnap" @change="pathChanged" :disabled="s.busy"></label>
      <label>Name of the new database
        <input v-model="form.name" name="name" class="mono" autocomplete="off" spellcheck="false" @input="touchName" :disabled="s.busy">
        <small class="srv-field-hint">{{ nameHint }}</small></label>
      <label>Format
        <select v-model="form.format" name="format" :disabled="s.busy">
          <option v-for="f in formats" :key="f" :value="f">{{ f || 'detected from the content' }}</option>
        </select></label>
      <label class="inline"><input type="checkbox" name="role" :checked="form.role === 'base'" :disabled="s.busy"
          @change="form.role = $event.target.checked ? 'base' : 'normal'">
        Import as a base (from a full snapshot, a plain Store, GraphSON or GraphML; layers are built over it)</label>
      <label class="inline" :title="mcpHelp"><input type="checkbox" name="mcp" v-model="form.mcp" :disabled="s.busy" data-import-mcp> Expose through MCP</label>
      <label class="inline" :title="rebaseHelp"><input type="checkbox" name="rebaseJobs" v-model="form.rebaseJobs" :disabled="s.busy || form.role === 'base'" data-import-rebase-jobs> Move backup jobs under the backup root (a store from elsewhere may carry jobs writing outside it; they would never run here)</label>
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
      <div class="store-dialog-buttons">
        <button type="submit" class="btn primary" :disabled="s.busy || !!fileProblem">{{ s.busy ? 'Importing…' : 'Import' }}</button>
        <button type="button" class="btn" :disabled="s.busy && (form.source !== 'upload' || s.sent)" @click="cancel">{{ s.busy && form.source === 'upload' && !s.sent ? 'Cancel upload' : 'Clear' }}</button>
      </div>
      <form-error :error="s.error" :problems="s.problems"></form-error>
    </form>
  </div>
  `,
};
