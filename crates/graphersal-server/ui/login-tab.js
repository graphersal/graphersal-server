// The "Sign in" tab of the `server` profile: the page anonymous gets at the server root. Paste a
// user token, `POST api/login` (the server answers the `gs_session` cookie), then the page goes
// to the return path `next` of its address (only a path from the server root: ./access-api.js
// `safeNext`, no open redirect) or loads the root again, where the server answers the signed-in
// principal's tabs. Below the form: the databases open to everyone (anonymous grants), when
// there are any. Model: ./access-api.js.

import { reactive, onMounted } from 'vue';
import {
  login, tokenProblem, afterLoginTarget, nextFromSearch, fetchMine, mineNote, emptyMineText,
} from './access-api.js';
import { ROOT, headers, errorNotice, pageHref } from './shared.js';

export default {
  props: { api: { type: Object, required: true }, params: { type: Object, default: () => ({}) } },
  setup() {
    const s = reactive({ token: '', busy: false, problem: '', error: null, open: [], openRead: false });
    const next = nextFromSearch(globalThis.location ? globalThis.location.search : '');
    const options = { base: ROOT, fetch: (...a) => globalThis.fetch(...a), headers };

    async function submit() {
      s.problem = tokenProblem(s.token) || '';
      s.error = null;
      if (s.problem) return;
      s.busy = true;
      try {
        await login(options, s.token);
        s.token = '';
        globalThis.location.assign(afterLoginTarget(globalThis.location.search, ROOT));
      } catch (err) {
        s.error = errorNotice(err);
        s.busy = false;
      }
    }

    onMounted(async () => {
      try {
        s.open = await fetchMine(options);
      } catch {
        s.open = [];
      }
      s.openRead = true;
    });

    return { s, next, submit, mineNote, emptyMineText, href: pageHref };
  },
  template: `
  <div class="srv-tab srv-login">
    <div class="srv-head"><h1>Sign in</h1></div>
    <p class="muted">Paste the user token an admin gave you. The server keeps you signed in with a cookie until you log out.</p>
    <p v-if="next" class="srv-sub">Then you go on to <span class="mono">{{ next }}</span>.</p>
    <form class="srv-form srv-narrow" @submit.prevent="submit">
      <label>Token
        <input v-model="s.token" name="token" type="password" class="mono" autocomplete="current-password" spellcheck="false" autofocus></label>
      <p v-if="s.problem" class="srv-problems" role="alert">{{ s.problem }}</p>
      <div v-if="s.error" class="notice srv-notice srv-error" role="alert">
        <div class="srv-notice-text"><span>{{ s.error.text }}</span>
          <p v-if="s.error.help" class="srv-help">{{ s.error.help }}</p></div>
      </div>
      <div class="store-dialog-buttons">
        <button type="submit" class="btn primary" :disabled="s.busy">{{ s.busy ? 'Signing in…' : 'Sign in' }}</button>
      </div>
    </form>
    <template v-if="s.openRead && s.open.length">
      <h2>Open to everyone</h2>
      <ul class="srv-mine-list">
        <li v-for="row in s.open" :key="row.name" :data-db="row.name">
          <a v-if="row.open" :href="href(row)" class="mono">{{ row.name }}</a><span v-else class="mono">{{ row.name }}</span>
          <span class="muted"> · {{ row.levelText }}</span>
          <span v-if="mineNote(row)" class="muted"> · {{ mineNote(row) }}</span>
          <div v-if="row.description" class="muted srv-sub">{{ row.description }}</div>
        </li>
      </ul>
    </template>
  </div>
  `,
};
