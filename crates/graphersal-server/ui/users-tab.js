// The Users tab of the `server` profile (admins only: the server answers this tab to admins):
// the users of `access.toml` from `GET api/admin/users` (admin badge, the token as `…last4`, the
// user's grants) with their actions (new token, revoke token, give or take the admin role,
// remove), "Add user…" (the new token is shown ONCE, with a copy button), and the grants of
// every subject, anonymous (= everyone) included (grant read or write on a database or on
// every database, revoke). The last admin cannot be removed, demoted or left without a token:
// the row disables those actions and the server refuses them too (409 with help, shown
// verbatim). Requests, rules and texts: ./admin-api.js.

import { reactive, ref, computed, nextTick, onMounted } from 'vue';
import {
  userRows, grantRows, userActions, addUserForm, grantForm, userConfirmText, userOutcomeText,
  shownToken, realRows, GRANT_LEVELS, NAME_HINT,
} from './admin-api.js';
import { admin, list, ensureLoaded, errorNotice, noticeClass } from './shared.js';

const freshAdd = () => ({ name: '', admin: false, problems: [], error: null, busy: false });

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup() {
    const s = reactive({ users: [], grants: [], read: false, error: null });
    const busy = reactive({});
    const notice = ref(null);
    const token = ref(null);
    const copied = ref(false);
    const dlg = ref(null);
    const add = reactive(freshAdd());
    const grant = reactive({ subject: 'anonymous', database: '', level: 'read', problems: [], busy: false });

    const databaseNames = computed(() => realRows(list.rows).map((r) => r.name));

    async function refresh() {
      try {
        const answer = await admin.users();
        s.users = userRows(answer);
        s.grants = grantRows(answer);
        s.error = null;
      } catch (err) {
        s.error = errorNotice(err);
      } finally {
        s.read = true;
      }
    }

    /** Runs one action of `subject`: busy marker, outcome or error notice, the token when the
     * answer carries one, the list again. */
    async function run(subject, action, fn) {
      busy[subject] = action;
      notice.value = null;
      try {
        const answer = (await fn()) || {};
        notice.value = { tone: 'ok', text: userOutcomeText(action, subject, answer), help: '' };
        const shown = shownToken(answer);
        if (shown) {
          token.value = shown;
          copied.value = false;
        }
        return true;
      } catch (err) {
        notice.value = errorNotice(err);
        return false;
      } finally {
        delete busy[subject];
        await refresh();
      }
    }

    function confirmed(action, name, extra) {
      const text = userConfirmText(action, name, extra);
      return !text || window.confirm(text);
    }

    const newToken = (u) => confirmed('newToken', u.name) && run(u.name, 'newToken', () => admin.newToken(u.name));
    const revokeToken = (u) => confirmed('revokeToken', u.name) && run(u.name, 'revokeToken', () => admin.revokeToken(u.name));
    const toggleAdmin = (u) => (u.admin ? confirmed('demote', u.name) : true)
      && run(u.name, 'setAdmin', () => admin.setAdmin(u.name, !u.admin));
    const removeUser = (u) => confirmed('removeUser', u.name) && run(u.name, 'removeUser', () => admin.removeUser(u.name));
    const revokeGrant = (g) => confirmed('revokeGrant', g.subject, g.database)
      && run(`${g.subject}/${g.database}`, 'revokeGrant', () => admin.revokeGrant(g.subject, g.database));

    async function openAdd() {
      Object.assign(add, freshAdd());
      await nextTick();
      if (dlg.value && !dlg.value.open) dlg.value.showModal();
    }
    function closeAdd() {
      if (add.busy) return;
      if (dlg.value && dlg.value.open) dlg.value.close();
    }
    async function submitAdd() {
      const form = addUserForm(add);
      add.problems = form.problems || [];
      add.error = null;
      if (form.problems) return;
      add.busy = true;
      try {
        const answer = await admin.addUser(form.name, form.admin);
        token.value = shownToken(answer);
        copied.value = false;
        notice.value = { tone: 'ok', text: userOutcomeText('addUser', form.name, answer), help: '' };
        add.busy = false;
        closeAdd();
      } catch (err) {
        add.error = errorNotice(err);
        add.busy = false;
      }
      await refresh();
    }

    async function submitGrant() {
      const form = grantForm(grant);
      grant.problems = form.problems || [];
      if (form.problems) return;
      grant.busy = true;
      await run(`${form.subject}/${form.database}`, 'grant', () => admin.grant(form.subject, form.database, form.level));
      grant.busy = false;
    }

    async function copyToken() {
      if (!token.value) return;
      try {
        await navigator.clipboard.writeText(token.value.token);
        copied.value = true;
      } catch {
        const field = document.querySelector('.srv-token input');
        if (field) field.select();
      }
    }

    onMounted(async () => {
      await Promise.all([refresh(), ensureLoaded()]);
      if (!grant.database && databaseNames.value.length) grant.database = databaseNames.value[0];
    });

    return {
      s, busy, notice, token, copied, dlg, add, grant, databaseNames, GRANT_LEVELS, noticeClass, nameHint: NAME_HINT,
      acts: (u) => userActions(u, s.users, { busy: !!busy[u.name] }),
      grantsText: (u) => u.grants.map((g) => `${g.database === '*' ? 'every database' : g.database}: ${g.level}`).join(', '),
      newToken, revokeToken, toggleAdmin, removeUser, revokeGrant, openAdd, closeAdd, submitAdd, submitGrant, copyToken, refresh,
    };
  },
  template: `
  <div class="srv-tab srv-users">
    <div class="srv-head">
      <h1>Users</h1>
      <span class="spacer"></span>
      <button type="button" class="btn small" @click="refresh">Refresh</button>
      <button type="button" class="btn small primary" @click="openAdd">Add user…</button>
    </div>
    <p class="muted">Who may sign in, with which token, and which databases each one reaches. An admin reaches every database and these tabs.</p>

    <div v-if="token" class="notice srv-notice srv-token" role="status">
      <div class="srv-notice-text">
        <span>The token of <strong class="mono">{{ token.name }}</strong>. It is shown only once: copy it now and hand it over safely.</span>
        <div class="srv-token-row">
          <input class="mono" readonly :value="token.token" aria-label="Token" @focus="$event.target.select()">
          <button type="button" class="btn small" @click="copyToken">{{ copied ? 'Copied' : 'Copy' }}</button>
        </div>
      </div>
      <button type="button" class="icon-btn" @click="token = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="notice" class="notice srv-notice" :class="noticeClass(notice)" role="status">
      <div class="srv-notice-text"><span>{{ notice.text }}</span>
        <p v-if="notice.help" class="srv-help">{{ notice.help }}</p></div>
      <button type="button" class="icon-btn" @click="notice = null" aria-label="Dismiss">×</button>
    </div>
    <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
      <div class="srv-notice-text"><span>{{ s.error.text }}</span>
        <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
    </div>

    <p v-if="s.read && !s.users.length && !s.error" class="muted">No users.</p>
    <div v-if="s.users.length" class="srv-table-wrap">
    <table class="data-table srv-table">
      <thead><tr><th>Name</th><th>Token</th><th>Grants</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="u in s.users" :key="u.name" :data-user="u.name">
          <td><span class="mono">{{ u.name }}</span> <span v-if="u.admin" class="badge accent">admin</span></td>
          <td class="mono" :title="u.hasToken ? 'The last four characters of its token' : 'It cannot sign in'">{{ u.tokenText }}</td>
          <td>{{ u.admin ? 'every database (admin)' : (grantsText(u) || 'none') }}</td>
          <td class="srv-actions">
            <button type="button" class="btn small" :disabled="!acts(u).newToken.enabled" :title="acts(u).newToken.title" @click="newToken(u)">New token…</button>
            <button type="button" class="btn small" :disabled="!acts(u).revokeToken.enabled" :title="acts(u).revokeToken.title" @click="revokeToken(u)">Revoke token…</button>
            <button type="button" class="btn small" :disabled="!acts(u).admin.enabled" :title="acts(u).admin.title" @click="toggleAdmin(u)">{{ u.admin ? 'Remove admin…' : 'Make admin' }}</button>
            <button type="button" class="btn small danger" :disabled="!acts(u).remove.enabled" :title="acts(u).remove.title" @click="removeUser(u)">Remove…</button>
          </td>
        </tr>
      </tbody>
    </table>
    </div>

    <div class="srv-head"><h2>Grants</h2></div>
    <p class="muted">A grant gives read or write on one database (or every database) to a user, or to everyone: "everyone (anonymous)" also covers visitors who are not signed in.</p>
    <form class="srv-inline-form srv-form" @submit.prevent="submitGrant">
      <label>Who
        <select v-model="grant.subject" name="subject">
          <option value="anonymous">everyone (anonymous)</option>
          <option v-for="u in s.users" :key="u.name" :value="u.name">{{ u.name }}</option>
        </select></label>
      <label>Database
        <select v-model="grant.database" name="database">
          <option v-for="n in databaseNames" :key="n" :value="n">{{ n }}</option>
          <option value="*">every database</option>
        </select></label>
      <label>Level
        <select v-model="grant.level" name="level">
          <option v-for="l in GRANT_LEVELS" :key="l" :value="l">{{ l }}</option>
        </select></label>
      <button type="submit" class="btn small" :disabled="grant.busy">Grant</button>
    </form>
    <ul v-if="grant.problems.length" class="srv-problems" role="alert"><li v-for="p in grant.problems" :key="p">{{ p }}</li></ul>
    <p v-if="s.read && !s.grants.length" class="muted">No grants: only admins reach a database.</p>
    <div v-else-if="s.grants.length" class="srv-table-wrap">
    <table class="data-table srv-table srv-grants">
      <thead><tr><th>Who</th><th>Database</th><th>Level</th><th><span class="sr-only">Actions</span></th></tr></thead>
      <tbody>
        <tr v-for="g in s.grants" :key="g.subject + '/' + g.database" :data-grant="g.subject + '/' + g.database">
          <td>{{ g.subjectText }}</td>
          <td class="mono">{{ g.databaseText }}</td>
          <td>{{ g.level }}</td>
          <td class="srv-actions"><button type="button" class="btn small danger" :disabled="!!busy[g.subject + '/' + g.database]" @click="revokeGrant(g)">Revoke…</button></td>
        </tr>
      </tbody>
    </table>
    </div>

    <dialog ref="dlg" class="store-dialog srv-dialog" aria-labelledby="srv-user-dialog-title" @cancel.prevent="closeAdd" @click.self="closeAdd">
      <form @submit.prevent="submitAdd">
        <div class="dialog-head">
          <h2 id="srv-user-dialog-title">Add user</h2><span class="spacer"></span>
          <button type="button" class="icon-btn" @click="closeAdd" aria-label="Close">×</button>
        </div>
        <div class="dialog-body srv-form">
          <label>Name <input v-model="add.name" class="mono" name="name" autocomplete="off" spellcheck="false"><small class="srv-field-hint">{{ nameHint }}</small></label>
          <label class="inline"><input type="checkbox" name="admin" v-model="add.admin"> Admin (every database, the admin API and these tabs)</label>
          <p class="muted">The server makes a token for the user and shows it once.</p>
          <ul v-if="add.problems.length" class="srv-problems" role="alert"><li v-for="p in add.problems" :key="p">{{ p }}</li></ul>
          <div v-if="add.error" class="notice srv-notice srv-error" role="alert">
            <div class="srv-notice-text"><span>{{ add.error.text }}</span>
              <p v-if="add.error.help" class="srv-help">{{ add.error.help }}</p></div>
          </div>
          <div class="store-dialog-buttons">
            <button type="submit" class="btn primary" :disabled="add.busy">{{ add.busy ? 'Working…' : 'Add' }}</button>
            <button type="button" class="btn" :disabled="add.busy" @click="closeAdd">Cancel</button>
          </div>
        </div>
      </form>
    </dialog>
  </div>
  `,
};
