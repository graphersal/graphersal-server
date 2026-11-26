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
// B-OPENAPI: each row links its database's OpenAPI description (`db/<name>/openapi.json`) and
// the head the server's own (`openapi.json`), for HTTP clients.

import { reactive, ref, computed, nextTick } from 'vue';
import {
  createBody, nameProblem, existingNameProblem, renameProblem, NAME_HINT, rowActions, confirmText, outcomeText, outcomeTone, loadAndWatch, baseNames, realRows,
  bytesText, transferActions, needsModelHint, poolText, promoteAction, isConflict, kindHelp, BASE_HELP, PROMOTE_HELP, KIND_HELP,
  isOpenState, damageActions, verifyText, verifyDamaged, verifyAllText, problemLines, repairSummary, repairedName, uncleanRows, REPAIR_HELP, RESTORE_HINT,
} from './admin-api.js';
import {
  admin, list, reload, patchRow, errorNotice, noticeClass, pageHref, openapiHref, serverOpenapiHref, useFreshData, useSort, SortTh,
} from './shared.js';
import { diskTitle, memoryText, memoryTitle, memoryRows, SIZE_COLUMNS } from './databases.js';

/** The kind column's text: the kind, and "base" for a base. */
const kindText = (row) => [row.kind, row.role === 'base' ? 'base' : ''].filter(Boolean).join(' · ');

/** The sortable columns of the database table. */
const DATABASE_COLUMNS = Object.freeze({
  name: { type: 'text' },
  state: { type: 'text', value: (r) => r.stateText },
  kind: { type: 'text', value: kindText },
  readOnly: { type: 'bool' },
  autoLoad: { type: 'bool' },
  ...SIZE_COLUMNS,
});

/** The sortable columns of the alias table. */
const ALIAS_COLUMNS = Object.freeze({ alias: { type: 'text' }, target: { type: 'text' } });

const emptyCreate = () => ({ name: '', kind: 'plain', role: 'normal', base: '', description: '', schema: '', readOnly: false, autoLoad: false });
const freshDialog = () => ({ mode: null, source: '', form: emptyCreate(), alias: '', target: '', newName: '', existing: false, problems: [], error: null, busy: false, repaired: null });

export default {
  components: { SortTh },
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

    const dbSort = useSort('databases', DATABASE_COLUMNS);
    const aliasSort = useSort('aliases', ALIAS_COLUMNS);
    const rows = computed(() => dbSort.rows(list.rows));
    const aliases = computed(() => aliasSort.rows(list.aliases));
    const aliasTargets = computed(() => list.aliases.map((a) => a.target));
    const acts = (row) => rowActions(row, { busy: !!busy[row.name], aliasTargets: aliasTargets.value });
    const more = (row) => transferActions(row, { busy: !!busy[row.name] });
    const promo = (row) => promoteAction(row, { busy: !!busy[row.name] });
    const dmg = (row) => damageActions(row, { busy: !!busy[row.name] });
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
    /** Verifies one database: the result as the notice, its problems as lines. */
    async function verifyDb(row) {
      busy[row.name] = 'verify';
      notice.value = null;
      try {
        const answer = await admin.verify(row.name);
        notice.value = {
          tone: verifyDamaged(answer) ? 'error' : 'ok',
          text: verifyText(answer),
          help: verifyDamaged(answer) ? `${problemLines(answer).join('\n')}${problemLines(answer).length ? '\n' : ''}Repair… builds a repaired copy as a new database. ${RESTORE_HINT}` : '',
        };
      } catch (err) {
        notice.value = errorNotice(err);
      } finally {
        delete busy[row.name];
        await reload();
      }
    }
    const verifyingAll = ref(false);
    async function verifyAll() {
      verifyingAll.value = true;
      notice.value = null;
      try {
        const summary = verifyAllText(await admin.verifyAll());
        notice.value = { tone: summary.tone, text: summary.text, help: summary.lines.join('\n') };
      } catch (err) {
        notice.value = errorNotice(err);
      } finally {
        verifyingAll.value = false;
        await reload();
      }
    }
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
        notice.value = { tone: 'ok', text: outcomeText('swap', done.source, answer), help: '' };
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
      if (done) notice.value = { tone: 'ok', text: `Kept both: "${done.source}" as it is and the repaired copy "${done.name}".`, help: 'Delete either one later with Delete… on its row.' };
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
      if (dialog.mode === 'repair') {
        const problem = nameProblem(dialog.newName, 'name for the repaired copy');
        if (problem) return { problems: [problem] };
        const subject = dialog.newName.trim().toLowerCase();
        const source = dialog.source;
        return { subject, action: 'repair', keepOpen: true, send: () => admin.repair(source, subject) };
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
        if (req.keepOpen) {
          // A repair: the dialog shows the report and offers the swap or keeping both.
          dialog.repaired = { source: answer.repairedFrom || dialog.source, name: answer.name || req.subject, summary: repairSummary(answer) };
          await reload();
          return;
        }
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

    useFreshData(() => Promise.all([reload(), readMemory(), readRecovery()]));

    return {
      list, rows, aliases, memory, diskTitle, memoryText, memoryTitle, dbSort, aliasSort, kindHelp, baseHelp: BASE_HELP, promoteHelp: PROMOTE_HELP, kindHelpOf: KIND_HELP, busy, notice, dlg, dialog, acts, more, promo, dmg, bases, databaseNames, showSize, showMemory,
      bytesText, needsModelHint, poolText, noticeClass, href: pageHref, apiHref: openapiHref, serverApiHref: serverOpenapiHref(), reload, nameHint: NAME_HINT,
      unclean, hideUnclean, isOpenState, verifyDb, verifyAll, verifyingAll, openRepair, swapRepaired, keepBoth, repairHelp: REPAIR_HELP, restoreHint: RESTORE_HINT,
      loadDb, unloadDb, deleteDb, promoteDb, setFlag, openCreate, openClone, openAlias, openRename, closeDialog, submitDialog, removeAlias,
      openBackups: (name) => props.api.navigate('backups', name ? { db: name } : {}),
      openExport: (row) => props.api.navigate('export', { db: row.name }),
      openRuns: (row) => props.api.navigate('runs', { db: row.name }),
      openSettings: (row) => props.api.navigate('settings', { db: row.aliasOf || row.name }),
      openImport: () => props.api.navigate('import'),
      openModels: () => props.api.navigate('models'),
      kindText,
      dialogTitle: computed(() => ({
        create: 'New database',
        clone: `Clone "${dialog.source}"`,
        alias: dialog.existing ? `Alias "${dialog.alias}"` : 'New alias',
        rename: `Rename alias "${dialog.alias}"`,
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
      <button class="btn" type="button" @click="verifyAll" :disabled="!list.loaded || !!list.error || verifyingAll" title="Verify the Store of every database (after a crash, or regularly: it finds damage while older snapshots can still repair it)">{{ verifyingAll ? 'Verifying…' : 'Verify all' }}</button>
      <button class="btn" type="button" @click="openImport" :disabled="!list.loaded || !!list.error">Import…</button>
      <button class="btn primary" type="button" @click="openCreate" :disabled="!list.loaded || !!list.error">New database…</button>
    </div>
    <p class="muted srv-api-note">HTTP clients: <a :href="serverApiHref" target="_blank" rel="noopener" data-server-openapi>the server's API (OpenAPI)</a> (login, databases, this admin API); each database's link "OpenAPI" describes its own API. Send a token as <span class="mono">Authorization: Bearer &lt;token&gt;</span>.</p>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" :role="notice.tone === 'ok' ? 'status' : 'alert'">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help srv-pre">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
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
          <td><a :href="href(row)" class="mono">{{ row.name }}</a>
            <span v-if="row.aliasOf" class="muted"> → {{ row.aliasOf }}</span>
            <a :href="apiHref(row)" class="srv-api-link" target="_blank" rel="noopener" data-openapi title="The OpenAPI description of this database's HTTP API (to generate a client)">OpenAPI</a>
            <div v-if="row.description" class="muted srv-sub">{{ row.description }}</div></td>
          <td><span class="badge" :class="row.pooled && !isOpenState(row.state) ? 'accent' : row.tone" :title="row.damage || row.reason || poolText(row)">{{ busy[row.name] === 'unload' ? 'unloading…' : busy[row.name] === 'verify' ? 'verifying…' : poolText(row) }}</span>
            <div v-if="row.reason" class="muted srv-sub">{{ row.reason }}</div>
            <details v-if="row.damage" class="srv-sub srv-damage"><summary>Damage report</summary><pre class="mono">{{ row.damage }}</pre></details>
            <div v-if="row.model" class="muted srv-sub mono">{{ row.model }}</div>
            <div v-if="needsModelHint(row)" class="srv-sub srv-hint" :title="needsModelHint(row)">Add the model in the <a href="#" @click.prevent="openModels">Models</a> tab.</div>
            <div v-if="row.linkWarning" class="srv-sub srv-warn" :title="row.linkWarning">{{ row.linkWarning }}</div></td>
          <td :title="kindHelp(row) || null">{{ kindText(row) }}<div v-if="row.base" class="muted srv-sub">over {{ row.base }}</div></td>
          <td><input type="checkbox" :checked="row.readOnly" :disabled="!acts(row).readOnly.enabled" :title="acts(row).readOnly.title"
                     :aria-label="'Read-only: ' + row.name" @change="setFlag(row, 'readOnly', $event)"></td>
          <td><input type="checkbox" :checked="row.autoLoad" :disabled="!acts(row).autoLoad.enabled" :title="acts(row).autoLoad.title"
                     :aria-label="'Auto-load: ' + row.name" @change="setFlag(row, 'autoLoad', $event)"></td>
          <td v-if="showSize" class="num" :title="diskTitle(row) || null">{{ bytesText(row.sizeBytes) }}</td>
          <td v-if="showMemory" class="num" :title="memoryTitle(row) || null">{{ memoryText(row) }}</td>
          <td class="srv-actions">
            <template v-if="!row.aliasOf">
              <button v-if="isOpenState(row.state)" type="button" class="btn small" :disabled="!acts(row).unload.enabled" :title="acts(row).unload.title" @click="unloadDb(row)">Unload…</button>
              <button v-else type="button" class="btn small" :disabled="!acts(row).load.enabled" :title="acts(row).load.title" @click="loadDb(row)">{{ busy[row.name] === 'load' || row.state === 'loading' ? 'Loading…' : 'Load' }}</button>
              <button type="button" class="btn small" title="Everything about it: identity, state, sizes, query limits, MCP, grants, backups, Store; change its description, flags and limits" data-settings-open @click="openSettings(row)">Settings…</button>
              <button type="button" class="btn small" :disabled="!acts(row).clone.enabled" :title="acts(row).clone.title" @click="openClone(row)">Clone…</button>
              <button type="button" class="btn small" :disabled="!more(row).backups.enabled" :title="more(row).backups.title" @click="openBackups(row.name)">Backups…</button>
              <button type="button" class="btn small" :disabled="!more(row).export.enabled" :title="more(row).export.title" @click="openExport(row)">Export…</button>
              <button v-if="isOpenState(row.state) && !row.aliasOf" type="button" class="btn small" title="The queries that wait or run on it, whatever their client; stop one" @click="openRuns(row)">Runs…</button>
              <button type="button" class="btn small" data-verify :disabled="!dmg(row).verify.enabled" :title="dmg(row).verify.title" @click="verifyDb(row)">{{ busy[row.name] === 'verify' ? 'Verifying…' : 'Verify' }}</button>
              <button type="button" class="btn small" :class="{ primary: row.state === 'maintenance' }" data-repair :disabled="!dmg(row).repair.enabled" :title="dmg(row).repair.title" @click="openRepair(row)">Repair…</button>
              <button type="button" class="btn small" :disabled="!promo(row).enabled" :title="promo(row).title" @click="promoteDb(row)">{{ busy[row.name] === 'promote' ? 'Promoting…' : 'Promote to base…' }}</button>
              <button type="button" class="btn small danger" :disabled="!acts(row).delete.enabled" :title="acts(row).delete.title" @click="deleteDb(row)">Delete…</button>
            </template>
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
          <template v-else-if="dialog.mode === 'rename'">
            <p>The alias "{{ dialog.alias }}" (database "{{ dialog.target }}") gets a new name. Links to db/{{ dialog.alias }}/ stop working; open MCP sessions that selected it move to the new name.</p>
            <label>New name <input v-model="dialog.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          </template>
          <ul v-if="dialog.problems.length" class="srv-problems" role="alert"><li v-for="p in dialog.problems" :key="p">{{ p }}</li></ul>
          <div v-if="dialog.error" class="notice srv-notice srv-error" role="alert">
            <div class="srv-notice-text"><span>{{ dialog.error.text }}</span>
              <p v-if="dialog.error.help" class="srv-help">{{ dialog.error.help }}</p></div>
          </div>
          <div v-if="dialog.mode === 'repair' && dialog.repaired" class="store-dialog-buttons">
            <button type="button" class="btn primary" data-swap :disabled="dialog.busy" @click="swapRepaired">{{ dialog.busy ? 'Working…' : 'Let it take the name…' }}</button>
            <button type="button" class="btn" data-keep :disabled="dialog.busy" @click="keepBoth">Keep both</button>
          </div>
          <div v-else class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="dialog.busy">{{ dialog.busy ? 'Working…' : dialog.mode === 'create' ? 'Create' : dialog.mode === 'clone' ? 'Clone' : dialog.mode === 'rename' ? 'Rename' : dialog.mode === 'repair' ? 'Repair' : 'Save' }}</button>
            <button type="button" class="btn" :disabled="dialog.busy" @click="closeDialog">Cancel</button>
          </div>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
