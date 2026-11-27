// The MCP profile editor of the MCP tab (MCPPROF): edits the MCP profile of one database or
// alias (`name`), or the server default profile (`name` null), without writing JSON: the
// built-in tools as checkboxes, each description in a text field that starts from the
// built-in text (an override is written only when it differs; "Reset" brings the built-in text
// back), the instructions mode with a text field that starts from the built-in instructions,
// saved queries as tools of their own (the loaded database's folders to pick from), the
// resources, the server name and title, the answer limit. The JSON view shows what will be
// saved (and takes edited JSON back), Import reads a graphersal `--mcp-config` file with its
// instructions file, Export downloads the profile; Preview shows what an agent sees (the
// server's own `initialize` and `tools/list` for the profile in the form, as a reader or a
// writer, nothing saved; with the block "Instructions for this database" that /mcp gives after
// use_graph: for the name, or for a database chosen beside "Preview /mcp"). Save sends the
// profile with the revision it was read at: a change made meanwhile is refused (409) and the
// form is kept. Data is read when the editor opens; the
// tab's refreshes never touch the form. Requests, form and texts: ./mcp-profile-api.js.

import { reactive, computed, onMounted } from 'vue';
import {
  createProfiles, builtinOf, formFromProfile, profileFromForm, isOverridden, defaultTools, importConfigFiles,
  folderChoices, selectedQueries, exportFileName, profileText, previewTools, clearQuestion, sourceTitle,
  previewBody, directoryBlockOf,
  INSTRUCTIONS_MODES, PROFILE_SOURCES, APPLIES_TEXT,
} from './mcp-profile-api.js';
import { isConflict } from './admin-api.js';
import { downloadBlob } from './databases.js';
import {
  requestOptions, errorNotice, notify, FormError,
} from './shared.js';

const profiles = createProfiles(requestOptions);

/** The built-in texts, read once per page (they change only with the server's version). */
let builtinCache = null;

export default {
  components: { FormError },
  props: {
    name: { type: String, default: null },
    kind: { type: String, default: 'database' },
    target: { type: String, default: '' },
    /** The databases and aliases a server-default preview may show the /mcp block of. */
    databases: { type: Array, default: () => [] },
  },
  emits: ['close', 'saved'],
  setup(props, { emit }) {
    const st = reactive({
      builtin: null, view: null, form: null, revision: '', read: false, readError: null,
      error: null, busy: false, json: '', jsonOpen: false, jsonError: '', preview: null, previewError: null,
      importError: '', startedFrom: '', previewDb: '', previewLevel: 'write',
    });

    const built = computed(() => (st.form && st.builtin ? profileFromForm(st.form, st.builtin) : { profile: {}, problems: [] }));
    const shownJson = computed(() => profileText(built.value.profile));
    const folders = computed(() => folderChoices(st.view && st.view.savedQueries));
    const chosenQueries = computed(() => (st.form ? selectedQueries(st.view && st.view.savedQueries, st.form.savedQueries) : []));
    const title = computed(() => (props.name ? `MCP profile of ${props.name}` : 'Server default MCP profile'));
    const endpoint = computed(() => (props.name ? `/db/${props.name}/mcp` : '/mcp'));
    const previewList = computed(() => previewTools(st.preview || {}));
    const previewBlock = computed(() => directoryBlockOf(st.preview || {}));
    const instructionsLength = computed(() => {
      if (!st.form) return 0;
      return (st.form.instructionsMode === 'append' ? st.form.appendText : st.form.replaceText).trim().length;
    });

    async function load(keepForm = false) {
      try {
        if (!builtinCache) builtinCache = builtinOf(await profiles.builtin());
        st.builtin = builtinCache;
        const view = await profiles.get(props.name);
        st.view = view;
        st.revision = typeof view.revision === 'string' ? view.revision : '';
        if (!keepForm) {
          const start = view.profile || view.effective || null;
          st.form = formFromProfile(start, st.builtin);
          st.startedFrom = view.profile ? 'own' : view.source;
        }
        st.readError = null;
      } catch (err) {
        st.readError = errorNotice(err);
      } finally {
        st.read = true;
      }
    }

    async function save() {
      st.error = null;
      if (built.value.problems.length) return;
      st.busy = true;
      try {
        const view = await profiles.set(props.name, built.value.profile, st.revision);
        st.view = view;
        st.revision = typeof view.revision === 'string' ? view.revision : '';
        st.startedFrom = 'own';
        const warnings = Array.isArray(view.warnings) ? view.warnings : [];
        notify({ tone: warnings.length ? 'warn' : 'ok', text: `${title.value} saved.`, help: warnings.join(' ') });
        emit('saved');
      } catch (err) {
        if (isConflict(err)) {
          st.error = { tone: 'warn', text: 'The MCP settings were changed meanwhile (another admin or tab).', help: 'Your edits are kept: check them and save again to apply them over the current settings.' };
          await load(true);
        } else {
          st.error = errorNotice(err);
        }
      } finally {
        st.busy = false;
      }
    }

    async function clear() {
      if (!window.confirm(clearQuestion(props.name, props.kind))) return;
      st.busy = true;
      st.error = null;
      try {
        await profiles.clear(props.name, st.revision);
        notify({ tone: 'ok', text: `${title.value} cleared.`, help: '' });
        await load();
        emit('saved');
      } catch (err) {
        st.error = errorNotice(err);
        if (isConflict(err)) await load(true);
      } finally {
        st.busy = false;
      }
    }

    async function preview(level) {
      st.previewError = null;
      st.preview = null;
      const body = previewBody(props.name, level, built.value.profile, st.previewDb);
      try {
        st.preview = await profiles.preview(body);
      } catch (err) {
        st.previewError = errorNotice(err);
      }
    }

    function startFromBuiltin() {
      st.form = formFromProfile(null, st.builtin);
      st.startedFrom = 'builtin';
    }

    function resetTool(tool) {
      tool.text = tool.builtin;
    }

    function resetInstructions() {
      st.form.replaceText = st.builtin.instructions;
    }

    function toggleFolder(folder) {
      const list = st.form.savedQueries.folders;
      const at = list.indexOf(folder);
      if (at >= 0) list.splice(at, 1);
      else list.push(folder);
    }

    function openJson() {
      st.json = shownJson.value;
      st.jsonError = '';
      st.jsonOpen = true;
    }

    function applyJson() {
      try {
        const value = JSON.parse(st.json);
        if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('a profile is a JSON object');
        st.form = formFromProfile(value, st.builtin);
        st.jsonOpen = false;
        st.jsonError = '';
      } catch (err) {
        st.jsonError = `Not applied: ${err.message}`;
      }
    }

    async function importFiles(event) {
      st.importError = '';
      const files = [...(event.target.files || [])];
      event.target.value = '';
      if (!files.length) return;
      try {
        const read = await Promise.all(files.map(async (f) => ({ name: f.name, text: await f.text() })));
        st.form = formFromProfile(importConfigFiles(read), st.builtin);
        notify({ tone: 'ok', text: `Imported ${files.map((f) => f.name).join(' + ')}: check the form, then Save.`, help: '' });
      } catch (err) {
        st.importError = err.message;
      }
    }

    function exportProfile() {
      downloadBlob(new Blob([shownJson.value], { type: 'application/json' }), exportFileName(props.name));
    }

    onMounted(() => load());

    return {
      st, built, shownJson, folders, chosenQueries, title, endpoint, previewList, previewBlock, instructionsLength,
      INSTRUCTIONS_MODES, PROFILE_SOURCES, APPLIES_TEXT, isOverridden, sourceTitle,
      save, clear, preview, startFromBuiltin, resetTool, resetInstructions, toggleFolder, openJson, applyJson,
      importFiles, exportProfile, defaultTools, load, close: () => emit('close'),
    };
  },
  template: `
  <section class="srv-profile" data-mcp-profile-editor :data-name="name || ''">
    <div class="srv-head">
      <h2>{{ title }}</h2>
      <span class="spacer"></span>
      <button type="button" class="btn small" data-profile-close @click="close">Close</button>
    </div>
    <p class="muted">What AI agents see on <span class="mono">{{ endpoint }}</span><template v-if="!name"> and on every <span class="mono">/db/&lt;name&gt;/mcp</span> without a profile of its own</template>: the tools offered with their descriptions, the instructions, saved queries as tools, the resources. Every text field starts from the built-in text: change what you need, leave the rest.</p>
    <div v-if="st.readError" class="notice srv-notice srv-error" role="alert"><div class="srv-notice-text"><span>{{ st.readError.text }}</span><p v-if="st.readError.help" class="srv-help">{{ st.readError.help }}</p></div></div>
    <template v-if="st.form && st.builtin">
    <p class="muted" data-profile-source>Now applies: <strong :title="sourceTitle(st.view.source)">{{ PROFILE_SOURCES[st.view.source] || st.view.source }}</strong><template v-if="kind === 'alias' && target"> (alias of {{ target }})</template>.
      <template v-if="st.startedFrom !== 'own'"> The form starts from {{ PROFILE_SOURCES[st.startedFrom] || 'the built-in profile' }}; saving gives this {{ name ? 'name' : 'server' }} its own profile.</template>
      <template v-if="st.view.readOnly"> MCP serves it read-only (a base, its read-only flag or MCP read-only): agents get the reading tools only.</template>
      <button type="button" class="btn small" data-profile-builtin @click="startFromBuiltin">Start from the built-in profile</button></p>
    <ul v-if="st.view.warnings && st.view.warnings.length" class="srv-problems"><li v-for="w in st.view.warnings" :key="w">{{ w }}</li></ul>

    <div class="srv-form srv-profile-form">
      <fieldset>
        <legend>Server</legend>
        <label>Server name <input v-model="st.form.serverName" :placeholder="st.builtin.serverName" data-profile-server-name><small class="srv-field-hint">The name agents show for this server (empty: {{ st.builtin.serverName }}).</small></label>
        <label>Server title <input v-model="st.form.serverTitle" :placeholder="st.builtin.serverTitle" data-profile-server-title></label>
        <label>Answer limit (characters) <input v-model="st.form.answerLimit" inputmode="numeric" :placeholder="String(st.builtin.answerLimitChars)" data-profile-answer-limit><small class="srv-field-hint">The longest tool answer (empty: {{ st.builtin.answerLimitChars }}; at least {{ st.builtin.minAnswerChars }}).</small></label>
      </fieldset>

      <fieldset>
        <legend>Instructions</legend>
        <label v-for="(text, mode) in INSTRUCTIONS_MODES" :key="mode" class="inline"><input type="radio" name="instructions-mode" :value="mode" v-model="st.form.instructionsMode" :data-profile-mode="mode"> {{ text }}</label>
        <label v-if="st.form.instructionsMode === 'append'">Your notes (after the built-in text)
          <textarea v-model="st.form.appendText" rows="8" spellcheck="false" data-profile-append></textarea>
          <small class="srv-field-hint">{{ instructionsLength }} / {{ st.builtin.maxInstructionsChars }} characters.</small></label>
        <label v-if="st.form.instructionsMode === 'replace'">The instructions (instead of the built-in text)
          <textarea v-model="st.form.replaceText" rows="14" spellcheck="false" data-profile-replace></textarea>
          <small class="srv-field-hint">{{ instructionsLength }} / {{ st.builtin.maxInstructionsChars }} characters. <button type="button" class="btn small" data-profile-reset-instructions @click="resetInstructions">Reset to built-in</button></small></label>
        <details v-if="st.form.instructionsMode !== 'replace'"><summary class="muted">The built-in instructions</summary><pre class="srv-pre">{{ st.builtin.instructions }}</pre></details>
        <small v-if="!name" class="srv-field-hint">On /mcp the agent first reads how to pick a database (list_graphs, use_graph), then these instructions.</small>
      </fieldset>

      <fieldset>
        <legend>Tools</legend>
        <p class="muted">Checked tools are offered. An agent that may only read (a read grant, a read-only database) never gets the writing ones. <button type="button" class="btn small" data-profile-default-tools @click="defaultTools(st.form)">Default selection</button></p>
        <div class="srv-table-wrap">
        <table class="data-table srv-table srv-profile-tools">
          <thead><tr><th>Offer</th><th>Tool</th><th>Description agents see</th></tr></thead>
          <tbody>
            <tr v-for="t in st.form.tools" :key="t.name" :data-profile-tool="t.name">
              <td><input type="checkbox" v-model="t.on" :aria-label="'Offer ' + t.name" data-profile-tool-on></td>
              <td class="mono">{{ t.name }}<br><span v-if="t.writes && !t.readers" class="badge" title="Changes data: never offered to an agent that may only read">writes</span><span v-else-if="t.writes" class="badge" title="May change data; offered to readers, who are refused writes">reads/writes</span> <span v-if="t.optIn" class="badge" title="Not offered unless checked">opt-in</span></td>
              <td><textarea v-model="t.text" rows="3" spellcheck="false" :disabled="!t.on" :aria-label="'Description of ' + t.name" data-profile-tool-text></textarea>
                <small v-if="isOverridden(t)" class="srv-field-hint"><span class="badge">changed</span> <button type="button" class="btn small" data-profile-tool-reset @click="resetTool(t)">Reset to built-in</button></small></td>
            </tr>
          </tbody>
        </table>
        </div>
      </fieldset>

      <fieldset>
        <legend>Saved queries as tools</legend>
        <label class="inline"><input type="checkbox" v-model="st.form.savedQueries.on" data-profile-sq-on> Offer saved queries as tools of their own (each with its own name, description and parameters)</label>
        <p v-if="!name" class="srv-field-hint">On /mcp saved queries are never tools of their own (agents run them with run_saved_query); this applies to the databases without a profile of their own.</p>
        <template v-if="st.form.savedQueries.on">
          <label class="inline"><input type="radio" value="all" v-model="st.form.savedQueries.scope" data-profile-sq-all> Every saved query</label>
          <label class="inline"><input type="radio" value="some" v-model="st.form.savedQueries.scope" data-profile-sq-some> The saved queries of these folders and names</label>
          <template v-if="st.form.savedQueries.scope === 'some'">
            <div v-if="folders.length" class="srv-profile-folders" data-profile-folders>
              <label v-for="f in folders" :key="f.folder" class="inline"><input type="checkbox" :checked="st.form.savedQueries.folders.includes(f.folder)" @change="toggleFolder(f.folder)" :data-profile-folder="f.folder"> <span class="mono">{{ f.folder }}</span> <span class="muted">({{ f.count }})</span></label>
            </div>
            <p v-else class="srv-field-hint">{{ name ? (st.view.loaded ? 'This database has no saved queries in folders.' : 'Load the database to pick from its folders; until then type them below.') : 'Type the folders below.' }}</p>
            <label>Other folders (comma-separated; a folder includes its subfolders) <input :value="st.form.savedQueries.folders.filter(f => !folders.some(c => c.folder === f)).join(', ')" @change="st.form.savedQueries.folders = [...st.form.savedQueries.folders.filter(f => folders.some(c => c.folder === f)), ...$event.target.value.split(',').map(s => s.trim()).filter(Boolean)]" data-profile-sq-folders></label>
            <label>Single queries (names, comma-separated) <input v-model="st.form.savedQueries.names" data-profile-sq-names></label>
          </template>
          <label>Leave out (names, comma-separated) <input v-model="st.form.savedQueries.exclude" data-profile-sq-exclude></label>
          <label class="inline"><input type="checkbox" v-model="st.form.savedQueries.paging" data-profile-sq-paging> Paging arguments (offset, max_rows, max_items) in each tool's input</label>
          <label>Note of a query that writes
            <select v-model="st.form.savedQueries.writeNote" data-profile-sq-note><option value="default">Built-in: "CHANGES DATA ...; run it only when the user asked"</option><option value="none">None</option><option value="custom">My text</option></select></label>
          <label v-if="st.form.savedQueries.writeNote === 'custom'">Text ({effects} stands for what the query changes) <input v-model="st.form.savedQueries.writeNoteText" data-profile-sq-note-text></label>
          <p v-if="st.view.savedQueries" class="srv-field-hint" data-profile-sq-chosen>Tools now: {{ chosenQueries.length ? chosenQueries.join(', ') : 'none' }}</p>
        </template>
      </fieldset>

      <fieldset>
        <legend>Resources</legend>
        <label v-for="r in st.form.resources" :key="r.name" class="inline"><input type="checkbox" v-model="r.on" :data-profile-resource="r.name"> <span class="mono">{{ r.name }}</span> <span class="muted">{{ r.title }}</span></label>
      </fieldset>

      <details class="srv-profile-json" :open="st.jsonOpen">
        <summary data-profile-json-summary>JSON (what will be saved: graphersal's --mcp-config format)</summary>
        <template v-if="!st.jsonOpen">
          <pre class="srv-pre" data-profile-json>{{ shownJson }}</pre>
          <button type="button" class="btn small" data-profile-json-edit @click="openJson">Edit the JSON</button>
        </template>
        <template v-else>
          <textarea v-model="st.json" rows="16" spellcheck="false" data-profile-json-text></textarea>
          <div class="store-dialog-buttons"><button type="button" class="btn small primary" data-profile-json-apply @click="applyJson">Apply to the form</button><button type="button" class="btn small" @click="st.jsonOpen = false">Cancel</button></div>
          <p v-if="st.jsonError" class="srv-field-hint" role="alert">{{ st.jsonError }}</p>
        </template>
      </details>
      <div class="srv-profile-files">
        <label class="inline">Import a configuration file <input type="file" multiple accept=".json,.md,.txt,application/json,text/plain,text/markdown" data-profile-import @change="importFiles"></label>
        <small class="srv-field-hint">A graphersal --mcp-config file; when it names an instructions file, choose both files at once.</small>
        <p v-if="st.importError" class="srv-field-hint" role="alert" data-profile-import-error>{{ st.importError }}</p>
        <button type="button" class="btn small" data-profile-export @click="exportProfile">Export JSON</button>
      </div>

      <div class="store-dialog-buttons">
        <button type="button" class="btn primary" :disabled="st.busy || built.problems.length > 0" data-profile-save @click="save">Save</button>
        <button v-if="st.view.profile" type="button" class="btn danger" :disabled="st.busy" data-profile-clear @click="clear">Clear profile…</button>
        <template v-if="name">
          <button type="button" class="btn" data-profile-preview-read @click="preview('read')">Preview as reader</button>
          <button type="button" class="btn" data-profile-preview-write @click="preview('write')">Preview as writer</button>
        </template>
        <template v-else>
          <label class="inline" title="The block &quot;Instructions for this database&quot; /mcp gives after use_graph">for <select v-model="st.previewDb" data-profile-preview-db><option value="">no database</option><option v-for="d in databases" :key="d" :value="d">{{ d }}</option></select></label>
          <label class="inline">as <select v-model="st.previewLevel" :disabled="!st.previewDb" data-profile-preview-level><option value="write">writer</option><option value="read">reader</option></select></label>
          <button type="button" class="btn" data-profile-preview-server @click="preview(st.previewLevel)">Preview /mcp</button>
        </template>
      </div>
      <form-error :error="st.error" :problems="built.problems"></form-error>
      <small class="srv-field-hint">{{ APPLIES_TEXT }}</small>
    </div>

    <div v-if="st.previewError" class="notice srv-notice srv-error" role="alert"><div class="srv-notice-text"><span>{{ st.previewError.text }}</span><p v-if="st.previewError.help" class="srv-help">{{ st.previewError.help }}</p></div></div>
    <div v-if="st.preview" class="srv-profile-preview" data-profile-preview>
      <div class="srv-head"><h3>What an agent sees on <span class="mono">{{ st.preview.endpoint }}</span> ({{ st.preview.readOnly ? 'may only read' : 'may write' }}; not saved yet)</h3><span class="spacer"></span><button type="button" class="btn small" @click="st.preview = null">Hide</button></div>
      <ul v-if="(st.preview.notes || []).length || (st.preview.warnings || []).length" class="srv-problems"><li v-for="n in [...(st.preview.notes || []), ...(st.preview.warnings || [])]" :key="n">{{ n }}</li></ul>
      <p class="muted">Server: <span class="mono">{{ st.preview.serverInfo && st.preview.serverInfo.name }}</span></p>
      <h4>Instructions</h4>
      <pre class="srv-pre" data-profile-preview-instructions>{{ st.preview.instructions }}</pre>
      <h4>Tools ({{ previewList.length }})</h4>
      <table class="data-table srv-table"><tbody>
        <tr v-for="t in previewList" :key="t.name" :data-profile-preview-tool="t.name"><td class="mono">{{ t.name }}</td><td>{{ t.description }}</td></tr>
      </tbody></table>
      <template v-if="previewBlock">
        <h4>On /mcp: after use_graph {{ st.preview.name }} ({{ st.preview.level === 'read' ? 'may only read' : 'may write' }})</h4>
        <p class="muted">The text /mcp appends to the answer of use_graph and of the first call on this database in a session: its own instructions, the tools available there for the caller, its saved queries.</p>
        <pre class="srv-pre" data-profile-preview-block>{{ previewBlock }}</pre>
      </template>
    </div>
    </template>
    <p v-else-if="st.read && !st.readError" class="muted">No answer.</p>
  </section>
  `,
};
