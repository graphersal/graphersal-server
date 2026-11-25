// The Databases tab of the `server` profile: the database list from `GET /api/admin/databases`
// (state, kind and role, read-only and auto-load toggles, size and memory when the server sends
// them, a link to each one's playground `db/<name>/`), the row actions (load with a poll until it
// ends, unload, clone, promote to a base, delete; Backups… and Export… open those tabs for the
// row, Runs… (a loaded row) the Runs tab), create (plain, base, or a layer over a base; optional description and schema) and the
// aliases (set, flip, rename, remove). Requests and rules: ./admin-api.js. Errors show the server's
// `error` and `help` verbatim. A `needs-model` row carries the hint to add its model; a base held
// by loaded layers reads "in pool"; a database holding copied model blobs shows the server's link
// warning. "Promote to base…" turns an unloaded plain database into a base after a one-way
// confirmation (`promoteAction` gives the reason it is disabled on every other row).

import { reactive, ref, computed, nextTick, onMounted } from 'vue';
import {
  createBody, nameProblem, existingNameProblem, renameProblem, NAME_HINT, rowActions, confirmText, outcomeText, outcomeTone, loadAndWatch, baseNames, realRows,
  bytesText, transferActions, needsModelHint, poolText, promoteAction, isConflict,
} from './admin-api.js';
import { admin, list, reload, patchRow, errorNotice, noticeClass, pageHref } from './shared.js';

const emptyCreate = () => ({ name: '', kind: 'plain', role: 'normal', base: '', description: '', schema: '', readOnly: false, autoLoad: false });
const freshDialog = () => ({ mode: null, source: '', form: emptyCreate(), alias: '', target: '', newName: '', existing: false, problems: [], error: null, busy: false });

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const busy = reactive({});
    const notice = ref(null);
    const dlg = ref(null);
    const dialog = reactive(freshDialog());

    /** Runs one action of `subject`: busy marker, outcome or error notice, list reload. */
    async function run(subject, action, fn) {
      busy[subject] = action;
      notice.value = null;
      try {
        const answer = (await fn()) || {};
        const failed = action === 'load' && (answer.state === 'failed' || answer.state === 'needs-model');
        notice.value = { tone: failed ? 'error' : outcomeTone(answer), text: outcomeText(action, subject, answer), help: '' };
        return true;
      } catch (err) {
        notice.value = errorNotice(err);
        return false;
      } finally {
        delete busy[subject];
        await reload();
      }
    }

    const rows = computed(() => list.rows);
    const aliases = computed(() => list.aliases);
    const aliasTargets = computed(() => list.aliases.map((a) => a.target));
    const acts = (row) => rowActions(row, { busy: !!busy[row.name], aliasTargets: aliasTargets.value });
    const more = (row) => transferActions(row, { busy: !!busy[row.name] });
    const promo = (row) => promoteAction(row, { busy: !!busy[row.name] });
    const databaseNames = computed(() => realRows(list.rows).map((r) => r.name));
    const bases = computed(() => baseNames(list.rows));
    const showSize = computed(() => list.rows.some((r) => r.sizeBytes !== null));
    const showMemory = computed(() => list.rows.some((r) => r.memoryBytes !== null));

    const loadDb = (row) => run(row.name, 'load', () => loadAndWatch(admin, row.name, { onState: patchRow }));
    function unloadDb(row) {
      if (!window.confirm(confirmText('unload', row.name))) return;
      run(row.name, 'unload', () => admin.unload(row.name));
    }
    function promoteDb(row) {
      if (!window.confirm(confirmText('promote', row.name))) return;
      run(row.name, 'promote', () => admin.promote(row.name));
    }
    function deleteDb(row) {
      if (!window.confirm(confirmText('delete', row.name))) return;
      run(row.name, 'delete', () => admin.delete(row.name));
    }
    async function setFlag(row, flag, event) {
      const box = event.target;
      const value = box.checked;
      // Based on the row's revision: a change someone else made meanwhile refuses it (409), the
      // list is read again and the checkbox shows the server's value.
      const ok = await run(row.name, flag, () => (flag === 'readOnly'
        ? admin.readOnly(row.name, value, row.revision)
        : admin.autoLoad(row.name, value, row.revision)));
      // A refusal keeps the server's value: the checkbox shows it again.
      if (!ok) box.checked = !value;
    }

    async function openDialog(mode, init = {}) {
      Object.assign(dialog, freshDialog(), { mode }, init);
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function closeDialog() {
      if (dialog.busy) return;
      dialog.mode = null;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    const openCreate = () => openDialog('create', { form: { ...emptyCreate(), base: bases.value[0] || '' } });
    const openClone = (row) => openDialog('clone', { source: row.name, form: { ...emptyCreate(), name: `${row.name}-copy` } });
    const openAlias = (alias) => openDialog('alias', alias
      ? { alias: alias.alias, target: alias.target, existing: true }
      : { target: databaseNames.value[0] || '' });
    const openRename = (alias) => openDialog('rename', { alias: alias.alias, target: alias.target, newName: alias.alias });

    /** The request of the open dialog, or `{problems}` (shown in the dialog, nothing sent). */
    function dialogRequest() {
      if (dialog.mode === 'create') {
        const built = createBody(dialog.form);
        if (built.problems) return { problems: built.problems };
        return { subject: built.body.name, action: 'create', send: () => admin.create(built.body) };
      }
      if (dialog.mode === 'clone') {
        const problem = nameProblem(dialog.form.name, 'name for the clone');
        if (problem) return { problems: [problem] };
        const subject = dialog.form.name.trim().toLowerCase();
        const source = dialog.source;
        return { subject, action: 'clone', send: () => admin.clone(source, subject) };
      }
      if (dialog.mode === 'alias') {
        // A flip keeps an existing alias's name (one made before 0.3.0 may break the new rule).
        const aliasProblem = dialog.existing ? existingNameProblem(dialog.alias, 'alias name') : nameProblem(dialog.alias, 'alias name');
        const problem = aliasProblem || (dialog.target ? null : 'Choose the database the alias names.');
        if (problem) return { problems: [problem] };
        const subject = dialog.alias.trim().toLowerCase();
        const target = dialog.target;
        const was = list.aliases.find((a) => a.alias === subject);
        if (was && was.target !== target && !window.confirm(confirmText('flipAlias', subject, target))) return { cancelled: true };
        const revision = list.aliasesRevision;
        return { subject, action: 'setAlias', send: () => admin.setAlias(subject, target, revision) };
      }
      if (dialog.mode === 'rename') {
        const problem = renameProblem(dialog.alias, dialog.newName);
        if (problem) return { problems: [problem] };
        const subject = dialog.newName.trim().toLowerCase();
        const alias = dialog.alias;
        const revision = list.aliasesRevision;
        return { subject, action: 'renameAlias', send: () => admin.renameAlias(alias, subject, revision) };
      }
      return { cancelled: true };
    }

    async function submitDialog() {
      if (dialog.busy) return;
      const req = dialogRequest();
      dialog.problems = req.problems || [];
      if (!req.send) return;
      dialog.busy = true;
      dialog.error = null;
      notice.value = null;
      try {
        const answer = (await req.send()) || {};
        notice.value = { tone: outcomeTone(answer), text: outcomeText(req.action, req.subject, answer), help: '' };
      } catch (err) {
        // The dialog stays open with the server's words, so the input can be fixed; a change
        // someone else made meanwhile (409) reads the list again first.
        dialog.error = errorNotice(err);
        if (isConflict(err)) await reload();
        return;
      } finally {
        dialog.busy = false;
      }
      closeDialog();
      await reload();
    }
    function removeAlias(a) {
      if (!window.confirm(confirmText('removeAlias', a.alias))) return;
      run(a.alias, 'removeAlias', () => admin.removeAlias(a.alias, list.aliasesRevision));
    }

    onMounted(reload);

    return {
      list, rows, aliases, busy, notice, dlg, dialog, acts, more, promo, bases, databaseNames, showSize, showMemory,
      bytesText, needsModelHint, poolText, noticeClass, href: pageHref, reload, nameHint: NAME_HINT,
      loadDb, unloadDb, deleteDb, promoteDb, setFlag, openCreate, openClone, openAlias, openRename, closeDialog, submitDialog, removeAlias,
      openBackups: (name) => props.api.navigate('backups', name ? { db: name } : {}),
      openExport: (row) => props.api.navigate('export', { db: row.name }),
      openRuns: (row) => props.api.navigate('runs', { db: row.name }),
      openImport: () => props.api.navigate('import'),
      openModels: () => props.api.navigate('models'),
      kindText: (row) => [row.kind, row.role === 'base' ? 'base' : ''].filter(Boolean).join(' · '),
      dialogTitle: computed(() => ({
        create: 'New database',
        clone: `Clone "${dialog.source}"`,
        alias: dialog.existing ? `Alias "${dialog.alias}"` : 'New alias',
        rename: `Rename alias "${dialog.alias}"`,
      })[dialog.mode] || ''),
    };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head">
      <h1>Databases</h1>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="reload" :disabled="list.loading">{{ list.loading ? 'Loading…' : 'Refresh' }}</button>
      <button class="btn" type="button" @click="openBackups('')" :disabled="!list.loaded || !!list.error" title="The backups of any database, also of a deleted one">Backups…</button>
      <button class="btn" type="button" @click="openImport" :disabled="!list.loaded || !!list.error">Import…</button>
      <button class="btn primary" type="button" @click="openCreate" :disabled="!list.loaded || !!list.error">New database…</button>
    </div>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" :role="notice.tone === 'ok' ? 'status' : 'alert'">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="list.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>The database list could not be read: {{ list.error.text }}</span>
        <p v-if="list.error.help" class="srv-help">{{ list.error.help }}</p></div>
    </div>
    <p v-else-if="list.loaded && !rows.length" class="muted">This server has no databases yet.</p>
    <ul v-if="list.problems.length" class="srv-problems">
      <li v-for="p in list.problems" :key="p">{{ p }}</li>
    </ul>
    <div v-if="rows.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-databases">
      <thead><tr>
        <th>Name</th><th>State</th><th>Kind</th>
        <th title="Every write to the database is refused">Read-only</th>
        <th title="Loaded when the server starts">Auto-load</th>
        <th v-if="showSize">Size</th><th v-if="showMemory">Memory</th>
        <th><span class="sr-only">Actions</span></th>
      </tr></thead>
      <tbody>
        <tr v-for="row in rows" :key="(row.aliasOf ? '@' : '') + row.name" :data-db="row.name" :class="{ 'alias-row': row.aliasOf }">
          <td><a :href="href(row)" class="mono">{{ row.name }}</a>
            <span v-if="row.aliasOf" class="muted"> → {{ row.aliasOf }}</span>
            <div v-if="row.description" class="muted srv-sub">{{ row.description }}</div></td>
          <td><span class="badge" :class="row.pooled && row.state !== 'loaded' ? 'accent' : row.tone" :title="row.reason || poolText(row)">{{ busy[row.name] === 'unload' ? 'unloading…' : poolText(row) }}</span>
            <div v-if="row.reason" class="muted srv-sub">{{ row.reason }}</div>
            <div v-if="row.model" class="muted srv-sub mono">{{ row.model }}</div>
            <div v-if="needsModelHint(row)" class="srv-sub srv-hint" :title="needsModelHint(row)">Add the model in the <a href="#" @click.prevent="openModels">Models</a> tab.</div>
            <div v-if="row.linkWarning" class="srv-sub srv-warn" :title="row.linkWarning">{{ row.linkWarning }}</div></td>
          <td>{{ kindText(row) }}<div v-if="row.base" class="muted srv-sub">over {{ row.base }}</div></td>
          <td><input type="checkbox" :checked="row.readOnly" :disabled="!acts(row).readOnly.enabled" :title="acts(row).readOnly.title"
                     :aria-label="'Read-only: ' + row.name" @change="setFlag(row, 'readOnly', $event)"></td>
          <td><input type="checkbox" :checked="row.autoLoad" :disabled="!acts(row).autoLoad.enabled" :title="acts(row).autoLoad.title"
                     :aria-label="'Auto-load: ' + row.name" @change="setFlag(row, 'autoLoad', $event)"></td>
          <td v-if="showSize" class="num">{{ bytesText(row.sizeBytes) }}</td>
          <td v-if="showMemory" class="num">{{ row.state === 'loaded' ? bytesText(row.memoryBytes) : '' }}</td>
          <td class="srv-actions">
            <template v-if="!row.aliasOf">
              <button v-if="row.state === 'loaded'" type="button" class="btn small" :disabled="!acts(row).unload.enabled" :title="acts(row).unload.title" @click="unloadDb(row)">Unload…</button>
              <button v-else type="button" class="btn small" :disabled="!acts(row).load.enabled" :title="acts(row).load.title" @click="loadDb(row)">{{ busy[row.name] === 'load' || row.state === 'loading' ? 'Loading…' : 'Load' }}</button>
              <button type="button" class="btn small" :disabled="!acts(row).clone.enabled" :title="acts(row).clone.title" @click="openClone(row)">Clone…</button>
              <button type="button" class="btn small" :disabled="!more(row).backups.enabled" :title="more(row).backups.title" @click="openBackups(row.name)">Backups…</button>
              <button type="button" class="btn small" :disabled="!more(row).export.enabled" :title="more(row).export.title" @click="openExport(row)">Export…</button>
              <button v-if="row.state === 'loaded' && !row.aliasOf" type="button" class="btn small" title="The queries that wait or run on it, whatever their client; stop one" @click="openRuns(row)">Runs…</button>
              <button type="button" class="btn small" :disabled="!promo(row).enabled" :title="promo(row).title" @click="promoteDb(row)">{{ busy[row.name] === 'promote' ? 'Promoting…' : 'Promote to base…' }}</button>
              <button type="button" class="btn small danger" :disabled="!acts(row).delete.enabled" :title="acts(row).delete.title" @click="deleteDb(row)">Delete…</button>
            </template>
          </td>
        </tr>
      </tbody>
    </table>
    </div>
    <p v-if="list.skipped" class="muted">{{ list.skipped }} entr{{ list.skipped === 1 ? 'y' : 'ies' }} of the server's answer could not be read.</p>

    <div class="srv-head">
      <h2>Aliases</h2>
      <span class="spacer"></span>
      <button class="btn small" type="button" @click="openAlias(null)" :disabled="!databaseNames.length">New alias…</button>
    </div>
    <p v-if="!aliases.length" class="muted">No aliases. An alias is a second name for a database (its pages are served under db/&lt;alias&gt;/ too); point it to another database to switch every link at once.</p>
    <div v-else class="srv-table-wrap">
    <table class="data-table srv-table srv-aliases">
      <thead><tr><th>Alias</th><th>Database</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="a in aliases" :key="a.alias" :data-alias="a.alias">
          <td class="mono">{{ a.alias }}</td>
          <td class="mono">{{ a.target }}</td>
          <td class="srv-actions">
            <button type="button" class="btn small" :disabled="!!busy[a.alias]" @click="openAlias(a)">Point to…</button>
            <button type="button" class="btn small" :disabled="!!busy[a.alias]" title="A new name for the alias, same database; open MCP sessions that selected it follow" @click="openRename(a)">Rename…</button>
            <button type="button" class="btn small danger" :disabled="!!busy[a.alias]" @click="removeAlias(a)">Remove…</button>
          </td>
        </tr>
      </tbody>
    </table>
    </div>

    <dialog ref="dlg" class="store-dialog srv-dialog" aria-labelledby="srv-dialog-title" @cancel.prevent="closeDialog" @click.self="closeDialog">
      <form v-if="dialog.mode" @submit.prevent="submitDialog">
        <div class="dialog-head">
          <h2 id="srv-dialog-title">{{ dialogTitle }}</h2><span class="spacer"></span>
          <button type="button" class="icon-btn" @click="closeDialog" aria-label="Close">×</button>
        </div>
        <div class="dialog-body srv-form">
          <template v-if="dialog.mode === 'create'">
            <label>Name <input v-model="dialog.form.name" class="mono" name="name" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
            <fieldset>
              <legend>Kind</legend>
              <label class="inline"><input type="radio" value="plain" v-model="dialog.form.kind"> Plain</label>
              <label class="inline" :title="bases.length ? '' : 'Create a base database first'"><input type="radio" value="layer" v-model="dialog.form.kind" :disabled="!bases.length"> Layer over a base</label>
            </fieldset>
            <label v-if="dialog.form.kind === 'layer'">Base
              <select v-model="dialog.form.base" name="base"><option v-for="b in bases" :key="b" :value="b">{{ b }}</option></select></label>
            <label v-else class="inline"><input type="checkbox" name="role" :checked="dialog.form.role === 'base'" @change="dialog.form.role = $event.target.checked ? 'base' : 'normal'">
              A base database (always read-only; layers can be created over it)</label>
            <label>Description <input v-model="dialog.form.description" name="description" autocomplete="off"></label>
            <label>Schema (optional, JSON)
              <textarea v-model="dialog.form.schema" name="schema" class="mono" rows="5" spellcheck="false" placeholder='{"mode": "open", ...}'></textarea></label>
            <label class="inline" v-if="dialog.form.role !== 'base' || dialog.form.kind === 'layer'"><input type="checkbox" name="readOnly" v-model="dialog.form.readOnly"> Read-only</label>
            <label class="inline"><input type="checkbox" name="autoLoad" v-model="dialog.form.autoLoad"> Load when the server starts</label>
          </template>
          <template v-else-if="dialog.mode === 'clone'">
            <p>A copy of the latest state of "{{ dialog.source }}" with the same kind, base, description and read-only flag; auto-load off. It is independent afterwards.</p>
            <label>Name of the clone <input v-model="dialog.form.name" name="name" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          </template>
          <template v-else-if="dialog.mode === 'alias'">
            <label>Alias <input v-model="dialog.alias" name="alias" class="mono" autocomplete="off" spellcheck="false" :readonly="dialog.existing"><small v-if="!dialog.existing" class="srv-field-hint">{{ nameHint }}</small></label>
            <label>Database
              <select v-model="dialog.target" name="target"><option v-for="n in databaseNames" :key="n" :value="n">{{ n }}</option></select></label>
          </template>
          <template v-else-if="dialog.mode === 'rename'">
            <p>The alias "{{ dialog.alias }}" (database "{{ dialog.target }}") gets a new name. Links to db/{{ dialog.alias }}/ stop working; open MCP sessions that selected it move to the new name.</p>
            <label>New name <input v-model="dialog.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          </template>
          <ul v-if="dialog.problems.length" class="srv-problems" role="alert"><li v-for="p in dialog.problems" :key="p">{{ p }}</li></ul>
          <div v-if="dialog.error" class="notice srv-notice srv-error" role="alert">
            <div class="srv-notice-text"><span>{{ dialog.error.text }}</span>
              <p v-if="dialog.error.help" class="srv-help">{{ dialog.error.help }}</p></div>
          </div>
          <div class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy">{{ dialog.busy ? 'Working…' : dialog.mode === 'create' ? 'Create' : dialog.mode === 'clone' ? 'Clone' : dialog.mode === 'rename' ? 'Rename' : 'Save' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy" @click="closeDialog">Cancel</button>
          </div>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
