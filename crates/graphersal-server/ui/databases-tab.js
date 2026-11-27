// The Databases tab of the `server` profile: the database list from `GET /api/admin/databases`
// (state, kind and role, read-only and auto-load toggles, size and memory when the server sends
// them, a link to each one's playground `db/<name>/`), the row actions (load with a poll until it
// ends, unload, clone, promote to a base, delete; Backups… and Export… open those tabs for the
// row, Runs… (a loaded row) the Runs tab), create (plain, base, or a layer over a base; optional description and schema) and the
// aliases (set, flip, rename, remove). Requests and rules: ./admin-api.js. Errors show the server's
// `error` and `help` verbatim. A `needs-model` row carries the hint to add its model; a base held
// by loaded layers reads "in pool"; a database holding copied model blobs shows the server's link
// warning. "Promote to base…" turns an unloaded plain database into a base after a one-way
// confirmation that explains bases and layers (`promoteAction` gives the reason it is disabled
// on every other row; the Kind column's tooltips and the "Bases and layers" help say what the
// kinds mean). The list is read again on every visit of the tab and when the window gets the
// focus back (./shared.js `useFreshData`); both tables sort by any column (`useSort`). Two size
// columns: "On disk" (the database's directory) and "In memory" (a loaded database's graph), each
// human-readable with the exact bytes in its tooltip; above the table a memory summary from
// `GET api/admin/server` (process, loaded databases, machine, the memory limit and what is free
// under it: ./databases.js `memoryRows`), read with the list. Round 3 B-CRASH: a database in
// `maintenance` (loaded read-only, its Store damaged) shows its damage report; "Verify" scrubs a
// database's Store and "Verify all" every one; "Repair…" (a plain database) builds a repaired copy
// as a NEW database (the damaged one is not changed) and then offers "Let it take the name…"
// (a confirmed swap: the damaged one moves to trash/) or "Keep both", with the hint to restore a
// backup instead; above the table the last unclean shutdown (`GET api/admin/recovery`: the
// databases the server had open when it was killed, each with its recovery and verify result).
// B-OPENAPI: each row's menu links its database's OpenAPI description (`db/<name>/openapi.json`).
// Round 4 B-LAYER: "Reset layer…" (layer rows only, `resetLayerAction`) discards the layer's
// changes over its base after a confirmation naming what is lost and what is kept; the outcome
// is a toast (`resetLayerOutcomeText`).
// Round 4 B-UI: every row action (and the OpenAPI link) sits behind ONE "…" button per row
// (./shared.js `ActionMenu`), so the table fits its width without a horizontal scroll; the
// table sorts by name until another column is chosen; outcomes and failures of row actions are
// toasts (./shared.js `notify`), a dialog's error shows under its own buttons (`FormError`).
// KBFIX: "Default model…" (plain, not base rows; `defaultModelAction`) picks which of the models
// the database's Store holds (read from its settings view) is the default, or none
// (`PUT api/admin/databases/<name>/default-model`).

import { reactive, ref, computed, nextTick } from 'vue';
import {
  createBody, nameProblem, existingNameProblem, renameProblem, NAME_HINT, rowActions, confirmText, outcomeText, outcomeTone, loadAndWatch, baseNames, realRows,
  bytesText, transferActions, needsModelHint, poolText, promoteAction, resetLayerAction, isConflict, kindHelp, kindCell, BASE_HELP, PROMOTE_HELP, KIND_HELP,
  isOpenState, damageActions, verifyText, verifyVerdict, verifyHelp, verifyAllText, verifyAllLabel, runVerifyAll, verifyBadge, repairSummary, repairedName, uncleanRows, REPAIR_HELP, RESTORE_HINT,
  MCP_NEW_HELP, defaultModelAction, defaultModelChoices,
} from './admin-api.js';
import { createSettings } from './settings-api.js';
import {
  admin, list, reload, patchRow, errorNotice, notify, pageHref, openapiHref, useFreshData, useSort, SortTh, ActionMenu, FormError, requestOptions,
} from './shared.js';
import { diskTitle, memoryText, memoryTitle, memoryRows, SIZE_COLUMNS } from './databases.js';

const settings = createSettings(requestOptions);


/** The sortable columns of the database table. */
const DATABASE_COLUMNS = Object.freeze({
  name: { type: 'text' },
  state: { type: 'text', value: (r) => r.stateText },
  kind: { type: 'text', value: (r) => kindCell(r).sortKey },
  readOnly: { type: 'bool' },
  autoLoad: { type: 'bool' },
  ...SIZE_COLUMNS,
});

/** What the State badge says while an action of the row runs. */
const BUSY_TEXT = Object.freeze({
  load: 'loading…', unload: 'unloading…', verify: 'verifying…', promote: 'promoting…', resetLayer: 'resetting…', delete: 'deleting…',
});

/** The sortable columns of the alias table. */
const ALIAS_COLUMNS = Object.freeze({ alias: { type: 'text' }, target: { type: 'text' } });

const emptyCreate = () => ({ name: '', kind: 'plain', role: 'normal', base: '', description: '', schema: '', readOnly: false, autoLoad: false, mcp: false });
/** The create form's flags from the server's defaults for new databases. */
const newDefaults = () => {
  const d = list.newDatabases || {};
  return { readOnly: d.readOnly === true, autoLoad: d.autoLoad === true, mcp: d.mcp === true };
};
const freshDialog = () => ({ mode: null, source: '', form: emptyCreate(), alias: '', target: '', newName: '', existing: false, mcp: false, problems: [], error: null, busy: false, repaired: null, models: [], current: '', loadingModels: false });

export default {
  components: { SortTh, ActionMenu, FormError },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const busy = reactive({});
    const dlg = ref(null);
    const dialog = reactive(freshDialog());

    /** Runs one action of `subject`: busy marker, outcome or error toast, list reload. */
    async function run(subject, action, fn) {
      busy[subject] = action;
      try {
        const answer = (await fn()) || {};
        const failed = action === 'load' && (answer.state === 'failed' || answer.state === 'needs-model');
        notify({ tone: failed ? 'error' : outcomeTone(answer), text: outcomeText(action, subject, answer), help: '' });
        return true;
      } catch (err) {
        notify(errorNotice(err));
        return false;
      } finally {
        delete busy[subject];
        await reload();
      }
    }

    const dbSort = useSort('databases', DATABASE_COLUMNS, { key: 'name', dir: 'asc' });
    const aliasSort = useSort('aliases', ALIAS_COLUMNS, { key: 'alias', dir: 'asc' });
    const rows = computed(() => dbSort.rows(list.rows));
    const aliases = computed(() => aliasSort.rows(list.aliases));
    const aliasTargets = computed(() => list.aliases.map((a) => a.target));
    const acts = (row) => rowActions(row, { busy: !!busy[row.name], aliasTargets: aliasTargets.value });
    const more = (row) => transferActions(row, { busy: !!busy[row.name] });
    const promo = (row) => promoteAction(row, { busy: !!busy[row.name] });
    const resetAct = (row) => resetLayerAction(row, { busy: !!busy[row.name] });
    const dmg = (row) => damageActions(row, { busy: !!busy[row.name] });
    const defAct = (row) => defaultModelAction(row, { busy: !!busy[row.name] });
    const databaseNames = computed(() => realRows(list.rows).map((r) => r.name));
    const bases = computed(() => baseNames(list.rows));
    const showSize = computed(() => list.rows.some((r) => r.sizeBytes !== null));
    const showMemory = computed(() => list.rows.some((r) => r.memoryReported || r.memoryBytes !== null));
    const memory = ref([]);
    const unclean = ref(null);
    /** The last unclean shutdown (`GET api/admin/recovery`); an older server or a failure shows none. */
    async function readRecovery() {
      try {
        unclean.value = uncleanRows((await admin.recovery()).uncleanShutdown);
      } catch {
        unclean.value = null;
      }
    }
    const hideUnclean = ref(false);
    /** The memory summary (`GET api/admin/server`); an older server or a failure shows none. */
    async function readMemory() {
      try {
        memory.value = memoryRows(await admin.server());
      } catch {
        memory.value = [];
      }
    }

    const loadDb = (row) => run(row.name, 'load', () => loadAndWatch(admin, row.name, { onState: patchRow }));
    function unloadDb(row) {
      if (!window.confirm(confirmText('unload', row.name))) return;
      run(row.name, 'unload', () => admin.unload(row.name));
    }
    function promoteDb(row) {
      if (!window.confirm(confirmText('promote', row.name))) return;
      run(row.name, 'promote', () => admin.promote(row.name));
    }
    /** "Reset layer…": confirmed (what is lost, what is kept), then the outcome as a toast. */
    function resetLayerDb(row) {
      if (!window.confirm(confirmText('resetLayer', row.name, row.base || ''))) return;
      run(row.name, 'resetLayer', () => admin.resetLayer(row.name));
    }
    /** Verifies one database: the result as a toast, its problems as lines. */
    async function verifyDb(row) {
      busy[row.name] = 'verify';
      try {
        const answer = await admin.verify(row.name);
        const verdict = verifyVerdict(answer);
        notify({
          tone: verdict === 'ok' ? 'ok' : verdict === 'needs-model' ? 'warn' : 'error',
          text: verifyText(answer),
          help: verifyHelp(answer, RESTORE_HINT),
        });
      } catch (err) {
        notify(errorNotice(err));
      } finally {
        delete busy[row.name];
        await reload();
      }
    }
    // Verify all (B-VERIFY): the databases one after the other through the per-database route,
    // so the button shows `x/y` and Cancel stops before the next one (the running verify ends);
    // the result is ONE toast with every problem line.
    const verifyRun = reactive({ running: false, done: 0, total: 0, cancel: false });
    const verifyingAll = computed(() => verifyRun.running);
    async function verifyAll() {
      if (verifyRun.running) return;
      const names = realRows(rows.value).map((r) => r.name);
      Object.assign(verifyRun, { running: true, done: 0, total: names.length, cancel: false });
      try {
        const run = await runVerifyAll(names, (name) => admin.verify(name), {
          progress: (done, total) => Object.assign(verifyRun, { done, total }),
          cancelled: () => verifyRun.cancel,
        });
        const summary = verifyAllText(run.results, { total: names.length, cancelled: run.cancelled });
        notify({ tone: summary.tone, text: summary.text, help: summary.lines.join('\n') });
      } catch (err) {
        notify(errorNotice(err));
      } finally {
        Object.assign(verifyRun, { running: false, cancel: false });
        await reload();
      }
    }
    const cancelVerifyAll = () => { if (verifyRun.running) verifyRun.cancel = true; };
    const openRepair = (row) => openDialog('repair', { source: row.name, newName: repairedName(row.name) });
    /** "Let it take the name…": the confirmed swap of the repaired copy into the damaged database's name. */
    async function swapRepaired() {
      const done = dialog.repaired;
      if (!done || dialog.busy) return;
      if (!window.confirm(confirmText('swap', done.source, done.name))) return;
      dialog.busy = true;
      dialog.error = null;
      try {
        const answer = (await admin.swap(done.source, done.name)) || {};
        notify({ tone: 'ok', text: outcomeText('swap', done.source, answer), help: '' });
      } catch (err) {
        dialog.error = errorNotice(err);
        return;
      } finally {
        dialog.busy = false;
      }
      closeDialog();
      await Promise.all([reload(), readRecovery()]);
    }
    function keepBoth() {
      const done = dialog.repaired;
      if (done) notify({ tone: 'ok', text: `Kept both: "${done.source}" as it is and the repaired copy "${done.name}".`, help: 'Delete either one later with Delete… on its row.' });
      closeDialog();
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
    // The create form starts from what the server gives a new database (server.toml [defaults],
    // mcp.toml expose_new: the listing's `newDatabases`).
    const openCreate = () => openDialog('create', {
      form: { ...emptyCreate(), ...newDefaults(), base: bases.value[0] || '' },
    });
    const openClone = (row) => openDialog('clone', { source: row.name, form: { ...emptyCreate(), name: `${row.name}-copy` } });
    const openAlias = (alias) => openDialog('alias', alias
      ? { alias: alias.alias, target: alias.target, existing: true }
      : { target: databaseNames.value[0] || '' });
    const openRename = (alias) => openDialog('rename', { alias: alias.alias, target: alias.target, newName: alias.alias });
    /** "Default model…": the dialog lists the models its Store holds (the settings view's `store`). */
    async function openDefaultModel(row) {
      await openDialog('defaultModel', { source: row.name, loadingModels: true });
      try {
        const choices = defaultModelChoices(await settings.view(row.name));
        if (dialog.mode !== 'defaultModel' || dialog.source !== row.name) return;
        Object.assign(dialog, { models: choices.models, current: choices.current, target: choices.current });
        if (choices.error) dialog.error = { text: `Its Store cannot be read: ${choices.error}`, help: '' };
      } catch (err) {
        dialog.error = errorNotice(err);
      } finally {
        dialog.loadingModels = false;
      }
    }

    /** The request of the open dialog, or `{problems}` (shown in the dialog, nothing sent). */
    function dialogRequest() {
      if (dialog.mode === 'create') {
        const built = createBody(dialog.form, list.newDatabases);
        if (built.problems) return { problems: built.problems };
        return { subject: built.body.name, action: 'create', send: () => admin.create(built.body) };
      }
      if (dialog.mode === 'clone') {
        const problem = nameProblem(dialog.form.name, 'name for the clone');
        if (problem) return { problems: [problem] };
        const subject = dialog.form.name.trim().toLowerCase();
        const source = dialog.source;
        const mcp = dialog.form.mcp === true;
        return { subject, action: 'clone', send: () => admin.clone(source, subject, mcp) };
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
        const mcp = !dialog.existing && dialog.mcp === true;
        return { subject, action: 'setAlias', send: () => admin.setAlias(subject, target, revision, mcp) };
      }
      if (dialog.mode === 'repair') {
        const problem = nameProblem(dialog.newName, 'name for the repaired copy');
        if (problem) return { problems: [problem] };
        const subject = dialog.newName.trim().toLowerCase();
        const source = dialog.source;
        return { subject, action: 'repair', keepOpen: true, send: () => admin.repair(source, subject) };
      }
      if (dialog.mode === 'defaultModel') {
        if (dialog.loadingModels) return { problems: ['Wait until its models are read.'] };
        const subject = dialog.source;
        const model = dialog.target || null;
        return { subject, action: 'defaultModel', send: () => admin.defaultModel(subject, model) };
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
      try {
        const answer = (await req.send()) || {};
        if (req.keepOpen) {
          // A repair: the dialog shows the report and offers the swap or keeping both.
          dialog.repaired = { source: answer.repairedFrom || dialog.source, name: answer.name || req.subject, summary: repairSummary(answer) };
          await reload();
          return;
        }
        notify({ tone: outcomeTone(answer), text: outcomeText(req.action, req.subject, answer), help: '' });
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

    /** The actions of a database row's "…" menu (./shared.js `ActionMenu`), in groups. */
    function databaseItems(row) {
      const a = acts(row);
      const m = more(row);
      const d = dmg(row);
      const p = promo(row);
      const r = resetAct(row);
      const open = isOpenState(row.state);
      return [
        open
          ? { key: 'unload', text: 'Unload…', enabled: a.unload.enabled, title: a.unload.title, run: () => unloadDb(row) }
          : { key: 'load', text: 'Load', enabled: a.load.enabled, title: a.load.title, run: () => loadDb(row) },
        { key: 'settings', text: 'Settings…', title: 'Everything about it: identity, state, sizes, query limits, MCP, grants, backups, Store; change its description, flags and limits', run: () => openSettings(row) },
        ...(open ? [{ key: 'runs', text: 'Runs…', title: 'The queries that wait or run on it, whatever their client; stop one', run: () => openRuns(row) }] : []),
        { key: 'sep-copy', separator: true },
        { key: 'clone', text: 'Clone…', enabled: a.clone.enabled, title: a.clone.title, run: () => openClone(row) },
        { key: 'backups', text: 'Backups…', enabled: m.backups.enabled, title: m.backups.title, run: () => openBackups(row.name) },
        { key: 'export', text: 'Export…', enabled: m.export.enabled, title: m.export.title, run: () => openExport(row) },
        { key: 'sep-store', separator: true },
        { key: 'verify', text: 'Verify', enabled: d.verify.enabled, title: d.verify.title, run: () => verifyDb(row) },
        { key: 'repair', text: 'Repair…', enabled: d.repair.enabled, title: d.repair.title, run: () => openRepair(row) },
        ...(row.kind === 'layer' || row.role === 'base' ? [] : [{ key: 'defaultModel', text: 'Default model…', enabled: defAct(row).enabled, title: defAct(row).title, run: () => openDefaultModel(row) }]),
        { key: 'promote', text: 'Promote to base…', enabled: p.enabled, title: p.title, run: () => promoteDb(row) },
        ...(row.kind === 'layer' ? [{ key: 'resetLayer', text: 'Reset layer…', danger: true, enabled: r.enabled, title: r.title, run: () => resetLayerDb(row) }] : []),
        { key: 'sep-api', separator: true },
        { key: 'openapi', text: 'OpenAPI description', href: openapiHref(row), title: "The OpenAPI description of this database's HTTP API (to generate a client)" },
        { key: 'sep-delete', separator: true },
        { key: 'delete', text: 'Delete…', danger: true, enabled: a.delete.enabled, title: a.delete.title, run: () => deleteDb(row) },
      ];
    }
    /** The actions of an alias row's "…" menu (in the database table). */
    function aliasItems(row) {
      const alias = list.aliases.find((x) => x.alias === row.name) || { alias: row.name, target: row.aliasOf };
      const idle = !busy[row.name];
      return [
        { key: 'settings', text: `Settings of "${row.aliasOf}"…`, title: 'The settings of the database the alias names', run: () => openSettings(row) },
        { key: 'point', text: 'Point to…', enabled: idle, run: () => openAlias(alias) },
        { key: 'rename', text: 'Rename…', enabled: idle, title: 'A new name for the alias, same database; open MCP sessions that selected it follow', run: () => openRename(alias) },
        { key: 'sep-api', separator: true },
        { key: 'openapi', text: 'OpenAPI description', href: openapiHref(row), title: "The OpenAPI description of the database's HTTP API under this alias (to generate a client)" },
        { key: 'sep-remove', separator: true },
        { key: 'remove', text: 'Remove…', danger: true, enabled: idle, run: () => removeAlias(alias) },
      ];
    }
    const rowItems = (row) => (row.aliasOf ? aliasItems(row) : databaseItems(row));
    const badgeText = (row) => BUSY_TEXT[busy[row.name]] || poolText(row);

    useFreshData(() => Promise.all([reload(), readMemory(), readRecovery()]));

    return {
      list, rows, aliases, memory, diskTitle, memoryText, memoryTitle, dbSort, aliasSort, kindHelp, baseHelp: BASE_HELP, mcpHelp: MCP_NEW_HELP, promoteHelp: PROMOTE_HELP, kindHelpOf: KIND_HELP, busy, dlg, dialog, acts, bases, databaseNames, showSize, showMemory,
      bytesText, needsModelHint, poolText, badgeText, rowItems, href: pageHref, reload, nameHint: NAME_HINT,
      unclean, hideUnclean, isOpenState, verifyDb, verifyAll, verifyingAll, verifyRun, cancelVerifyAll, verifyAllLabel, verifyBadge, openRepair, swapRepaired, keepBoth, repairHelp: REPAIR_HELP, restoreHint: RESTORE_HINT,
      loadDb, unloadDb, deleteDb, promoteDb, setFlag, openCreate, openClone, openAlias, openRename, closeDialog, submitDialog, removeAlias,
      openBackups: (name) => props.api.navigate('backups', name ? { db: name } : {}),
      openExport: (row) => props.api.navigate('export', { db: row.name }),
      openRuns: (row) => props.api.navigate('runs', { db: row.name }),
      openSettings: (row) => props.api.navigate('settings', { db: row.aliasOf || row.name }),
      openImport: () => props.api.navigate('import'),
      openModels: () => props.api.navigate('models'),
      kindCell,
      dialogTitle: computed(() => ({
        create: 'New database',
        clone: `Clone "${dialog.source}"`,
        alias: dialog.existing ? `Alias "${dialog.alias}"` : 'New alias',
        rename: `Rename alias "${dialog.alias}"`,
        defaultModel: `Default model of "${dialog.source}"`,
        repair: dialog.repaired ? `Repaired "${dialog.source}"` : `Repair "${dialog.source}"`,
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
      <button class="btn" type="button" @click="verifyAll" :disabled="!list.loaded || !!list.error || verifyingAll" :aria-busy="verifyingAll ? 'true' : null" data-verify-all title="Verify the Store of every database, one after the other (after a crash, or regularly: it finds damage while older snapshots can still repair it)">{{ verifyAllLabel(verifyRun) }}</button>
      <button v-if="verifyRun.running" class="btn" type="button" @click="cancelVerifyAll" :disabled="verifyRun.cancel" data-verify-cancel title="Stop after the database being verified now">Cancel</button>
      <button class="btn" type="button" @click="openImport" :disabled="!list.loaded || !!list.error">Import…</button>
      <button class="btn primary" type="button" @click="openCreate" :disabled="!list.loaded || !!list.error">New database…</button>
    </div>
    <div v-if="unclean && !hideUnclean" class="notice srv-notice srv-unclean" :class="unclean.rows.some((r) => r.damaged) ? 'srv-error' : 'srv-warn-notice'" role="status" data-unclean>
      <div class="srv-notice-text">
        <span>Last unclean shutdown{{ unclean.when ? ' (found ' + unclean.when + ')' : '' }}: the server{{ unclean.pid ? ' with pid ' + unclean.pid : '' }} was killed or crashed. {{ unclean.rows.length ? 'The databases it had open were loaded again, their journals replayed and their Stores verified' + (unclean.done ? '' : ' (still checking…)') + ':' : 'No database was open.' }}{{ unclean.known ? '' : ' Its open databases were unknown: the auto-load ones were checked.' }}</span>
        <ul class="srv-unclean-list">
          <li v-for="r in unclean.rows" :key="r.name" :data-unclean-db="r.name"><span class="mono">{{ r.name }}</span> <span class="badge" :class="r.tone">{{ r.outcome }}</span> <span class="muted">{{ r.text }}</span></li>
        </ul>
        <p v-if="unclean.rows.some((r) => r.damaged)" class="srv-help">A damaged database is served read-only (maintenance): Repair… on its row builds a repaired copy. {{ restoreHint }}</p>
      </div>
      <button type="button" class="icon-btn" @click="hideUnclean = true" aria-label="Dismiss">×</button>
    </div>
    <div v-if="list.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>The database list could not be read: {{ list.error.text }}</span>
        <p v-if="list.error.help" class="srv-help">{{ list.error.help }}</p></div>
    </div>
    <p v-else-if="list.loaded && !rows.length" class="muted">This server has no databases yet.</p>
    <dl v-if="memory.length" class="srv-memory" aria-label="Memory">
      <div v-for="m in memory" :key="m.key" :data-memory="m.key" :title="m.title || null"><dt>{{ m.label }}</dt><dd>{{ m.text }}</dd></div>
    </dl>
    <ul v-if="list.problems.length" class="srv-problems">
      <li v-for="p in list.problems" :key="p">{{ p }}</li>
    </ul>
    <div v-if="rows.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-databases">
      <thead><tr>
        <sort-th :s="dbSort" k="name">Name</sort-th><sort-th :s="dbSort" k="state">State</sort-th>
        <sort-th :s="dbSort" k="kind" title="Plain: stores all its data itself. Layer: stores only its own changes over a base. Base: frozen and read-only; layers are built over it.">Kind</sort-th>
        <sort-th :s="dbSort" k="readOnly" title="Every write to the database is refused">Read-only</sort-th>
        <sort-th :s="dbSort" k="autoLoad" title="Loaded when the server starts">Auto-load</sort-th>
        <sort-th v-if="showSize" :s="dbSort" k="size" num title="The database's directory on disk: Store snapshots, journal, models">On disk</sort-th><sort-th v-if="showMemory" :s="dbSort" k="memory" num title="A loaded database's graph in memory (a layer: its own changes; its base is shared)">In memory</sort-th>
        <th><span class="sr-only">Actions</span></th>
      </tr></thead>
      <tbody>
        <tr v-for="row in rows" :key="(row.aliasOf ? '@' : '') + row.name" :data-db="row.name" :class="{ 'alias-row': row.aliasOf }">
          <td><a :href="href(row)" class="mono srv-name">{{ row.name }}</a>
            <span v-if="row.aliasOf" class="muted"> → <span class="srv-alias-target">{{ row.aliasOf }}</span></span>
            <div v-if="row.description" class="muted srv-sub">{{ row.description }}</div></td>
          <td><span class="badge" :class="row.pooled && !isOpenState(row.state) ? 'accent' : row.tone" :title="row.damage || row.reason || poolText(row)">{{ badgeText(row) }}</span>
            <div v-if="row.reason" class="muted srv-sub">{{ row.reason }}</div>
            <details v-if="row.damage" class="srv-sub srv-damage"><summary>Damage report</summary><pre class="mono">{{ row.damage }}</pre></details>
            <div v-if="row.model" class="muted srv-sub mono">{{ row.model }}</div>
            <div v-if="needsModelHint(row)" class="srv-sub srv-hint" :title="needsModelHint(row)">Add the model in the <a href="#" @click.prevent="openModels">Models</a> tab.</div>
            <div v-if="row.linkWarning" class="srv-sub srv-warn" :title="row.linkWarning">{{ row.linkWarning }}</div>
            <div v-if="!row.aliasOf && verifyBadge(row)" class="srv-sub" data-verify-badge><span class="badge" :class="verifyBadge(row).tone" :title="verifyBadge(row).title">{{ verifyBadge(row).text }}</span></div></td>
          <td class="srv-kind-cell" :title="kindCell(row).title || null"><span class="srv-kind-text">{{ kindCell(row).text }}</span><div v-if="kindCell(row).sub" class="muted srv-sub srv-kind-sub">{{ kindCell(row).sub }}</div></td>
          <td><input type="checkbox" :checked="row.readOnly" :disabled="!acts(row).readOnly.enabled" :title="acts(row).readOnly.title"
                     :aria-label="'Read-only: ' + row.name" @change="setFlag(row, 'readOnly', $event)"></td>
          <td><input type="checkbox" :checked="row.autoLoad" :disabled="!acts(row).autoLoad.enabled" :title="acts(row).autoLoad.title"
                     :aria-label="'Auto-load: ' + row.name" @change="setFlag(row, 'autoLoad', $event)"></td>
          <td v-if="showSize" class="num" :title="diskTitle(row) || null">{{ bytesText(row.sizeBytes) }}</td>
          <td v-if="showMemory" class="num" :title="memoryTitle(row) || null">{{ memoryText(row) }}</td>
          <td class="srv-menu-cell"><action-menu :items="rowItems(row)" :label="'Actions of ' + row.name"></action-menu>
          </td>
        </tr>
      </tbody>
    </table>
    </div>
    <details v-if="rows.length" class="srv-help-box srv-bases-help">
      <summary>Bases and layers</summary>
      <p>{{ baseHelp }}</p>
      <ul>
        <li>{{ kindHelpOf.plain }}</li>
        <li>{{ kindHelpOf.layer }}</li>
        <li>Base: frozen and always read-only. Create one with "New database…" ("A base database"), by importing a file as a base, or with "Promote to base…" on an unloaded plain database.</li>
      </ul>
      <p>{{ promoteHelp }}</p>
    </details>
    <p v-if="list.skipped" class="muted">{{ list.skipped }} entr{{ list.skipped === 1 ? 'y' : 'ies' }} of the server's answer could not be read.</p>

    <div class="srv-head">
      <h2>Aliases</h2>
      <span class="spacer"></span>
      <button class="btn small" type="button" @click="openAlias(null)" :disabled="!databaseNames.length">New alias…</button>
    </div>
    <p v-if="!aliases.length" class="muted">No aliases. An alias is a second name for a database (its pages are served under db/&lt;alias&gt;/ too); point it to another database to switch every link at once.</p>
    <div v-else class="srv-table-wrap">
    <table class="data-table srv-table srv-aliases">
      <thead><tr><sort-th :s="aliasSort" k="alias">Alias</sort-th><sort-th :s="aliasSort" k="target">Database</sort-th><th><span class="sr-only">Actions</span></th></tr></thead>
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
              <label class="inline" :title="bases.length ? kindHelpOf.layer : 'Create a base database first'"><input type="radio" value="layer" v-model="dialog.form.kind" :disabled="!bases.length"> Layer over a base</label>
            </fieldset>
            <label v-if="dialog.form.kind === 'layer'">Base
              <select v-model="dialog.form.base" name="base"><option v-for="b in bases" :key="b" :value="b">{{ b }}</option></select></label>
            <label v-else class="inline" :title="baseHelp"><input type="checkbox" name="role" :checked="dialog.form.role === 'base'" @change="dialog.form.role = $event.target.checked ? 'base' : 'normal'">
              A base database (always read-only; layers can be created over it)</label>
            <label>Description <input v-model="dialog.form.description" name="description" autocomplete="off"></label>
            <label>Schema (optional, JSON)
              <textarea v-model="dialog.form.schema" name="schema" class="mono" rows="5" spellcheck="false" placeholder='{"mode": "open", ...}'></textarea></label>
            <label class="inline" v-if="dialog.form.role !== 'base' || dialog.form.kind === 'layer'"><input type="checkbox" name="readOnly" v-model="dialog.form.readOnly"> Read-only</label>
            <label class="inline"><input type="checkbox" name="autoLoad" v-model="dialog.form.autoLoad"> Load when the server starts</label>
            <label class="inline" :title="mcpHelp"><input type="checkbox" name="mcp" v-model="dialog.form.mcp" data-create-mcp> Expose through MCP</label>
          </template>
          <template v-else-if="dialog.mode === 'clone'">
            <p>A copy of the latest state of "{{ dialog.source }}" with the same kind, base, description and read-only flag; auto-load off. It is independent afterwards.</p>
            <label>Name of the clone <input v-model="dialog.form.name" name="name" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
            <label class="inline" :title="mcpHelp"><input type="checkbox" name="mcp" v-model="dialog.form.mcp" data-clone-mcp> Expose through MCP</label>
          </template>
          <template v-else-if="dialog.mode === 'alias'">
            <label>Alias <input v-model="dialog.alias" name="alias" class="mono" autocomplete="off" spellcheck="false" :readonly="dialog.existing"><small v-if="!dialog.existing" class="srv-field-hint">{{ nameHint }}</small></label>
            <label>Database
              <select v-model="dialog.target" name="target"><option v-for="n in databaseNames" :key="n" :value="n">{{ n }}</option></select></label>
            <label v-if="!dialog.existing" class="inline" :title="mcpHelp"><input type="checkbox" name="mcp" v-model="dialog.mcp" data-alias-mcp> Expose through MCP</label>
          </template>
          <template v-else-if="dialog.mode === 'repair' && !dialog.repaired">
            <p>{{ repairHelp }}</p>
            <p class="muted">{{ restoreHint }}</p>
            <label>Name of the repaired copy <input v-model="dialog.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          </template>
          <template v-else-if="dialog.mode === 'repair'">
            <div class="notice srv-notice" :class="dialog.repaired.summary.tone === 'ok' ? 'info' : 'srv-warn-notice'" role="status"><div class="srv-notice-text"><span>{{ dialog.repaired.summary.text }}</span></div></div>
            <ul v-if="dialog.repaired.summary.lines.length" class="srv-repair-lines"><li v-for="l in dialog.repaired.summary.lines" :key="l">{{ l }}</li></ul>
            <p>Let "{{ dialog.repaired.name }}" take the name "{{ dialog.source }}" (the damaged one moves to the trash/ directory; its grants, aliases and settings stay with the name), or keep both databases.</p>
            <p class="muted">{{ restoreHint }}</p>
          </template>
          <template v-else-if="dialog.mode === 'defaultModel'">
            <p>The embedding model that <span class="mono">embed()</span> and text queries use when they name no model. One commit of the database's Store: a loaded database uses it from its next query on (a model already loaded stays loaded), an unloaded one at its next load.</p>
            <p v-if="dialog.loadingModels" class="muted">Reading its models…</p>
            <p v-else-if="!dialog.models.length && !dialog.error" class="muted">Its Store holds no embedding model. A model comes with an import or a restore, or is added to its Store by the engine's tools.</p>
            <label v-else-if="dialog.models.length">Default model
              <select v-model="dialog.target" name="model" data-default-model>
                <option value="">(none: embed() without a name uses hash-ngram/1)</option>
                <option v-for="m in dialog.models" :key="m" :value="m">{{ m }}{{ m === dialog.current ? ' (current)' : '' }}</option>
              </select></label>
          </template>
          <template v-else-if="dialog.mode === 'rename'">
            <p>The alias "{{ dialog.alias }}" (database "{{ dialog.target }}") gets a new name. Links to db/{{ dialog.alias }}/ stop working; open MCP sessions that selected it move to the new name.</p>
            <label>New name <input v-model="dialog.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          </template>
          <div v-if="dialog.mode === 'repair' && dialog.repaired" class="store-dialog-buttons">
            <button type="button" class="btn primary" data-swap :disabled="dialog.busy" @click="swapRepaired">{{ dialog.busy ? 'Working…' : 'Let it take the name…' }}</button>
            <button type="button" class="btn" data-keep :disabled="dialog.busy" @click="keepBoth">Keep both</button>
          </div>
          <div v-else class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy">{{ dialog.busy ? 'Working…' : dialog.mode === 'create' ? 'Create' : dialog.mode === 'clone' ? 'Clone' : dialog.mode === 'rename' ? 'Rename' : dialog.mode === 'repair' ? 'Repair' : dialog.mode === 'defaultModel' ? 'Set' : 'Save' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy" @click="closeDialog">Cancel</button>
          </div>
          <form-error :error="dialog.error" :problems="dialog.problems"></form-error>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
