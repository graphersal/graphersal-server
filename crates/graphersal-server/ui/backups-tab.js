// The Backups tab of the `server` profile: the backups of one database (`GET .../backups`,
// oldest first; also for the name of a deleted database), "Back up now" (`POST .../backup`) and
// restore of a backup over the database itself (unloaded only) or into a new database (`POST
// .../restore`). The database comes from the tab's address parameter `db` (the Databases tab's
// row button "Backups…") or the form. Rules and texts: ./admin-api.js. The server's
// `error`/`help` stay shown; a finished action reloads the database list.

import { reactive, ref, computed, watch, onMounted } from 'vue';
import {
  backupRows, restoreOverAction, nameProblem, existingNameProblem, restoredName, NAME_HINT, confirmText, outcomeText, bytesText, realRows,
} from './admin-api.js';
import { admin, list, ensureLoaded, reload, errorNotice, noticeClass } from './shared.js';

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup(props) {
    const s = reactive({
      name: '', typed: '', list: [], listed: false, error: null, busy: '', restoring: null, target: 'over', newName: '',
    });
    const notice = ref(null);

    const names = computed(() => realRows(list.rows).map((r) => r.name));
    const row = computed(() => list.rows.find((r) => r.name === s.name && !r.aliasOf) || null);
    const over = computed(() => restoreOverAction(row.value));

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
      Object.assign(s, { name: name || '', typed: name || '', list: [], listed: false, error: null, busy: '', restoring: null });
      await refresh();
    }
    /** The form: a database of the list or any name (a deleted database's backups). */
    function show() {
      // Any name a database had (one made before 0.3.0 may break the rule of new names).
      const problem = existingNameProblem(s.typed, 'database name');
      if (problem) {
        s.error = { tone: 'error', text: problem, help: '' };
        return;
      }
      const name = s.typed.trim().toLowerCase();
      if (name === (props.params.db || '')) choose(name);
      else props.api.navigate('backups', { db: name });
    }

    async function backupNow() {
      s.busy = 'backup';
      s.error = null;
      notice.value = null;
      try {
        const answer = await admin.backup(s.name);
        notice.value = { tone: 'ok', text: outcomeText('backup', s.name, answer), help: '' };
        await refresh();
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.busy = '';
      }
    }

    function startRestore(b) {
      s.restoring = b.backup;
      s.target = over.value.enabled ? 'over' : 'new';
      s.newName = restoredName(s.name);
      s.error = null;
    }
    async function restore() {
      const backup = s.restoring;
      let newName = '';
      if (s.target === 'new') {
        const problem = nameProblem(s.newName, 'name for the restored database');
        if (problem) {
          s.error = { tone: 'error', text: problem, help: '' };
          return;
        }
        newName = s.newName.trim().toLowerCase();
      } else if (!window.confirm(confirmText('restoreOver', s.name, backup))) {
        return;
      }
      s.busy = 'restore';
      s.error = null;
      notice.value = null;
      try {
        const answer = await admin.restore(s.name, backup, newName || undefined);
        notice.value = { tone: 'ok', text: outcomeText('restore', newName || s.name, answer), help: '' };
        s.restoring = null;
        await reload();
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.busy = '';
      }
    }

    watch(() => props.params.db, (db) => choose(db || ''));
    onMounted(async () => {
      await ensureLoaded();
      await choose(props.params.db || '');
    });

    return { s, notice, names, row, over, show, backupNow, startRestore, restore, bytesText, refresh, noticeClass, nameHint: NAME_HINT };
  },
  template: `
  <div class="srv-tab">
    <div class="srv-head"><h1>{{ s.name ? 'Backups of "' + s.name + '"' : 'Backups' }}</h1></div>
    <form class="srv-inline-form srv-form" @submit.prevent="show">
      <label>Database (also the name of a deleted one)
        <input v-model="s.typed" name="database" class="mono" list="srv-backup-names" autocomplete="off" spellcheck="false"></label>
      <datalist id="srv-backup-names"><option v-for="n in names" :key="n" :value="n"></option></datalist>
      <button type="submit" class="btn">Show</button>
    </form>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" role="status">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="s.name" class="srv-bar">
      <span class="muted">Verified ZIP backups in the server's backups/{{ s.name }}/ directory.</span>
      <span class="spacer"></span>
      <button type="button" class="btn small" :disabled="!!s.busy" @click="refresh">Refresh</button>
      <button v-if="row && row.kind" type="button" class="btn small primary" :disabled="!!s.busy" @click="backupNow">{{ s.busy === 'backup' ? 'Backing up…' : 'Back up now' }}</button>
    </div>
    <p v-else class="muted">Choose a database to see its backups.</p>
    <p v-if="s.listed && !s.list.length" class="muted">No backups yet.</p>
    <div v-if="s.list.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-backups">
      <thead><tr><th>Backup</th><th>Size</th><th>Kind</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="b in s.list" :key="b.backup" :data-backup="b.backup">
          <td class="mono">{{ b.backup }}<div v-if="b.file" class="muted srv-sub">{{ b.file }}</div></td>
          <td class="num">{{ bytesText(b.bytes) }}</td>
          <td>{{ [b.kind, b.role === 'base' ? 'base' : ''].filter(Boolean).join(' · ') }}<div v-if="b.base" class="muted srv-sub">over {{ b.base }}</div></td>
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
      </fieldset>
      <div class="store-dialog-buttons">
        <button type="submit" class="btn primary" :disabled="!!s.busy">{{ s.busy === 'restore' ? 'Restoring…' : 'Restore' }}</button>
        <button type="button" class="btn" :disabled="!!s.busy" @click="s.restoring = null">Cancel</button>
      </div>
    </form>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>
  </div>
  `,
};
