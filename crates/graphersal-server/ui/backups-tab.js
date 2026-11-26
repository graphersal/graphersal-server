// The Backups tab of the `server` profile: the backups of one database (`GET .../backups`,
// oldest first; also for the name of a deleted database), "Back up now" (`POST .../backup`) and
// restore of a backup over the database itself (unloaded only) or into a new database (`POST
// .../restore`). The database comes from the tab's address parameter `db` (the Databases tab's
// row button "Backups…") or the form. Rules and texts: ./admin-api.js. The server's
// `error`/`help` stay shown; a finished action reloads the database list. The backups and the
// database list are read again on every visit of the tab and when the window gets the focus back
// (./shared.js `useFreshData`; an open restore form keeps its input); the table sorts by any column.
//
// Below, "Download server backup" (`GET api/admin/server-backup`, ./admin-api.js
// `serverBackupPath`): one ZIP of the server's configuration, optionally with every database's
// data, shown with the warning that it holds the users' token hashes. Without host headers it is
// a plain link (the browser streams the file); with them it is fetched and saved
// (`fetchServerBackup`), a refusal showing the server's `error`/`help`.

import { reactive, computed, watch } from 'vue';
import {
  serverBackupPath, fetchServerBackup, SERVER_BACKUP_WARNING,
  backupRows, restoreOverAction, nameProblem, existingNameProblem, restoredName, NAME_HINT, RESTORE_GRANTS_NOTE, confirmText, outcomeText, outcomeTone, bytesText, realRows, kindHelp, kindCell,
} from './admin-api.js';
import {
  ROOT, headers, admin, list, reload, errorNotice, notify, useFreshData, useSort, SortTh, FormError,
} from './shared.js';
import { downloadBlob } from './databases.js';

/** The sortable columns of the backup table (a backup's name is its time: text order is time order). */
const BACKUP_COLUMNS = Object.freeze({
  backup: { type: 'text' },
  size: { type: 'number', value: (b) => b.bytes },
  kind: { type: 'text', value: (r) => kindCell(r).sortKey },
});

export default {
  components: { SortTh, FormError },
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({
      name: '', typed: '', list: [], listed: false, error: null, busy: '', restoring: null, target: 'over', newName: '',
      showError: null, backupError: null, restoreError: null,
    });
    // The server backup (below the database's backups).
    const sb = reactive({ full: false, busy: false, error: null });
    const withHeaders = Object.keys(headers).length > 0;
    const serverBackupHref = computed(() => new URL(serverBackupPath({ withDatabases: sb.full }).path, ROOT).href);
    function serverBackupStarted() {
      sb.error = null;
      notify({ tone: 'ok', text: 'The server backup is being prepared; the download starts when it is written.' });
    }
    async function downloadServerBackup() {
      sb.busy = true;
      sb.error = null;
      try {
        const got = await fetchServerBackup({ base: ROOT, headers, withDatabases: sb.full });
        downloadBlob(got.blob, got.fileName);
        const left = got.skipped.length ? ` Left out (their backup failed): ${got.skipped.join(', ')}.` : '';
        notify({ tone: got.skipped.length ? 'warn' : 'ok', text: `Saved ${got.fileName}.${left}` });
      } catch (err) {
        sb.error = errorNotice(err);
      } finally {
        sb.busy = false;
      }
    }

    const names = computed(() => realRows(list.rows).map((r) => r.name));
    const row = computed(() => list.rows.find((r) => r.name === s.name && !r.aliasOf) || null);
    const over = computed(() => restoreOverAction(row.value));
    const backupSort = useSort('backups', BACKUP_COLUMNS);
    const backups = computed(() => backupSort.rows(s.list));

    async function refresh() {
      if (!s.name) return;
      s.error = null;
      try {
        s.list = backupRows(await admin.backups(s.name));
        s.listed = true;
      } catch (err) {
        s.list = [];
        s.listed = false;
        s.error = errorNotice(err);
      }
    }

    /** Shows the backups of `name` ('' = none chosen). */
    async function choose(name) {
      Object.assign(s, {
        name: name || '', typed: name || '', list: [], listed: false, error: null, busy: '', restoring: null, showError: null, backupError: null, restoreError: null,
      });
      await refresh();
    }
    /** The form: a database of the list or any name (a deleted database's backups). */
    function show() {
      // Any name a database had (one made before 0.3.0 may break the rule of new names).
      const problem = existingNameProblem(s.typed, 'database name');
      s.showError = problem ? { tone: 'error', text: problem, help: '' } : null;
      if (problem) return;
      const name = s.typed.trim().toLowerCase();
      if (name === (props.params.db || '')) choose(name);
      else props.api.navigate('backups', { db: name });
    }

    async function backupNow() {
      s.busy = 'backup';
      s.backupError = null;
      try {
        const answer = await admin.backup(s.name);
        notify({ tone: 'ok', text: outcomeText('backup', s.name, answer), help: '' });
        await refresh();
      } catch (err) {
        s.backupError = errorNotice(err);
      } finally {
        s.busy = '';
      }
    }

    function startRestore(b) {
      s.restoring = b.backup;
      s.target = over.value.enabled ? 'over' : 'new';
      s.newName = restoredName(s.name);
      s.restoreError = null;
    }
    async function restore() {
      const backup = s.restoring;
      let newName = '';
      if (s.target === 'new') {
        const problem = nameProblem(s.newName, 'name for the restored database');
        if (problem) {
          s.restoreError = { tone: 'error', text: problem, help: '' };
          return;
        }
        newName = s.newName.trim().toLowerCase();
      } else if (!window.confirm(confirmText('restoreOver', s.name, backup))) {
        return;
      }
      s.busy = 'restore';
      s.restoreError = null;
      try {
        const answer = await admin.restore(s.name, backup, newName || undefined);
        notify({ tone: outcomeTone(answer), text: outcomeText('restore', newName || s.name, answer), help: '' });
        s.restoring = null;
        await reload();
      } catch (err) {
        s.restoreError = errorNotice(err);
      } finally {
        s.busy = '';
      }
    }

    watch(() => props.params.db, (db) => choose(db || ''));
    // The first visit chooses the address's database; every later one reads its backups again
    // (the restore form and the typed name stay).
    let chosen = false;
    useFreshData(async () => {
      await reload();
      if (!chosen) {
        chosen = true;
        await choose(props.params.db || '');
      } else {
        await refresh();
      }
    });

    return { sb, withHeaders, serverBackupHref, serverBackupStarted, downloadServerBackup, serverBackupWarning: SERVER_BACKUP_WARNING, s, backups, backupSort, kindCell, kindHelp, names, row, over, show, backupNow, startRestore, restore, bytesText, refresh, nameHint: NAME_HINT, grantsNote: RESTORE_GRANTS_NOTE };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>{{ s.name ? 'Backups of "' + s.name + '"' : 'Backups' }}</h1></div>
    <form class="srv-inline-form srv-form" @submit.prevent="show">
      <label>Database (also the name of a deleted one)
        <input v-model="s.typed" name="database" class="mono" list="srv-backup-names" autocomplete="off" spellcheck="false"></label>
      <datalist id="srv-backup-names"><option v-for="n in names" :key="n" :value="n"></option></datalist>
      <button type="submit" class="btn">Show</button>
      <form-error :error="s.showError"></form-error>
    </form>
    <div v-if="s.name" class="srv-bar">
      <span class="muted">Verified ZIP backups in the server's backups/{{ s.name }}/ directory.</span>
      <span class="spacer"></span>
      <button type="button" class="btn small" :disabled="!!s.busy" @click="refresh">Refresh</button>
      <button v-if="row && row.kind" type="button" class="btn small primary" :disabled="!!s.busy" @click="backupNow">{{ s.busy === 'backup' ? 'Backing up…' : 'Back up now' }}</button>
    </div>
    <form-error v-if="s.name" :error="s.backupError"></form-error>
    <p v-else class="muted">Choose a database to see its backups.</p>
    <p v-if="s.listed && !s.list.length" class="muted">No backups yet.</p>
    <div v-if="s.list.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-backups">
      <thead><tr><sort-th :s="backupSort" k="backup">Backup</sort-th><sort-th :s="backupSort" k="size" num>Size</sort-th><sort-th :s="backupSort" k="kind">Kind</sort-th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="b in backups" :key="b.backup" :data-backup="b.backup">
          <td class="mono">{{ b.backup }}<div v-if="b.file" class="muted srv-sub">{{ b.file }}</div></td>
          <td class="num">{{ bytesText(b.bytes) }}</td>
          <td class="srv-kind-cell" :title="kindCell(b).title || null"><span class="srv-kind-text">{{ kindCell(b).text }}</span><div v-if="kindCell(b).sub" class="muted srv-sub srv-kind-sub">{{ kindCell(b).sub }}</div></td>
          <td class="srv-actions"><button type="button" class="btn small" :disabled="!!s.busy" @click="startRestore(b)">Restore…</button></td>
        </tr>
      </tbody>
    </table>
    </div>
    <form v-if="s.restoring" class="srv-restore srv-form" @submit.prevent="restore">
      <fieldset>
        <legend>Restore {{ s.restoring }}</legend>
        <label class="inline" :title="over.title"><input type="radio" value="over" v-model="s.target" :disabled="!over.enabled"> Over "{{ s.name }}" itself</label>
        <p v-if="!over.enabled" class="muted srv-sub">{{ over.title }}</p>
        <label class="inline"><input type="radio" value="new" v-model="s.target"> Into a new database</label>
        <label v-if="s.target === 'new'">Name <input v-model="s.newName" name="newName" class="mono" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
        <p class="muted srv-sub srv-grants-note">{{ grantsNote }}</p>
      </fieldset>
      <div class="store-dialog-buttons">
        <button type="submit" class="btn primary" :disabled="!!s.busy">{{ s.busy === 'restore' ? 'Restoring…' : 'Restore' }}</button>
        <button type="button" class="btn" :disabled="!!s.busy" @click="s.restoring = null">Cancel</button>
      </div>
      <form-error :error="s.restoreError"></form-error>
    </form>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>
    <section class="srv-server-backup srv-form srv-narrow" aria-labelledby="srv-server-backup-title">
      <h2 id="srv-server-backup-title">Server backup</h2>
      <p class="muted">One ZIP of the whole server: server.toml, the users and grants, the aliases, the MCP switches and every database's settings. Restore it on a new machine with <code>graphersal-server restore FILE --dir NEWDIR</code>.</p>
      <label class="inline"><input type="checkbox" name="withDatabases" v-model="sb.full"> With every database's data <span class="muted">(a fresh verified backup of each and the shared models: a full backup, can be large)</span></label>
      <p class="srv-secret" role="note">{{ serverBackupWarning }}</p>
      <div class="store-dialog-buttons">
        <a v-if="!withHeaders" class="btn" :href="serverBackupHref" download @click="serverBackupStarted">Download server backup</a>
        <button v-else type="button" class="btn" :disabled="sb.busy" @click="downloadServerBackup">{{ sb.busy ? 'Preparing…' : 'Download server backup' }}</button>
      </div>
      <form-error :error="sb.error"></form-error>
    </section>
  </div>
  `,
};
