// The `server` profile's top-bar module (profile key `header`): who is signed in (the root
// `GET /api/info` `principal`, from `api.info.host`) with an "admin" badge, and "Log out" for a
// user's browser session (`POST api/logout`, then the root again: the server answers the
// "Sign in" tab). Anonymous sees nothing here: its page is the login page. Model:
// ./access-api.js.

import { h, ref } from 'vue';
import { headerModel, logout, principalOf, rootHref } from './access-api.js';
import { hostInfo } from './databases.js';
import { ROOT, headers } from './shared.js';

export default {
  props: { api: { type: Object, required: true } },
  setup(props) {
    const busy = ref(false);
    const failed = ref('');

    async function out() {
      busy.value = true;
      failed.value = '';
      try {
        // No refusal hook: a 401 here means the session had ended already, which is the goal.
        await logout({ base: ROOT, fetch: (...a) => globalThis.fetch(...a), headers });
        globalThis.location.assign(rootHref(ROOT));
      } catch (err) {
        failed.value = err && err.message ? err.message : String(err);
        busy.value = false;
      }
    }

    return () => {
      const model = headerModel(principalOf(hostInfo(props.api)));
      if (!model.show) return null;
      return h('span', { class: 'srv-header' }, [
        h('span', { class: 'srv-header-name hide-narrow', title: `Signed in as ${model.name}` }, [
          h('span', { class: 'muted' }, 'Signed in as '),
          h('strong', { class: 'mono' }, model.name),
        ]),
        model.role ? h('span', { class: 'badge accent' }, model.role) : null,
        failed.value ? h('span', { class: 'srv-warn', title: failed.value }, 'logout failed') : null,
        model.logout
          ? h('button', { type: 'button', class: 'btn small', disabled: busy.value, onClick: out }, busy.value ? 'Logging out…' : 'Log out')
          : null,
      ]);
    };
  },
};
