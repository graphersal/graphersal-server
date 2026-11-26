// The toast stack of the server UI (ui/toasts.js, shown by shared.js `ToastStack`): lifetimes by
// tone, errors until closed, pause while hovered, the limit, repeats counted, links; and the rule
// that every tab reports its outcomes through `notify` instead of a banner at the top.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import {
  createToasts, TOAST_LIFE_MS, TOAST_MAX, toastTone, toastClass, toastRole, isSticky, isLongHelp, HELP_CLAMP,
} from '../toasts.js';

const clock = () => {
  const c = { t: 1000 };
  c.now = () => c.t;
  return c;
};

test('tones: ok hides itself, warn later, error stays until closed', () => {
  assert.equal(toastTone('ok'), 'ok');
  assert.equal(toastTone('warn'), 'warn');
  assert.equal(toastTone('error'), 'error');
  assert.equal(toastTone(undefined), 'ok');
  assert.equal(toastTone('info'), 'ok');
  assert.ok(TOAST_LIFE_MS.ok >= 3000 && TOAST_LIFE_MS.ok <= 10000, 'a few seconds');
  assert.ok(TOAST_LIFE_MS.warn > TOAST_LIFE_MS.ok);
  assert.equal(TOAST_LIFE_MS.error, null);
  assert.equal(isSticky('error'), true);
  assert.equal(isSticky('ok'), false);
  assert.equal(toastClass('warn'), 'srv-toast-warn');
  assert.equal(toastRole('error'), 'alert');
  assert.equal(toastRole('ok'), 'status');
});

test('expiry: each toast by its own deadline; errors never expire', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  const ok = m.push({ tone: 'ok', text: 'done' });
  const warn = m.push({ tone: 'warn', text: 'careful' });
  const err = m.push({ tone: 'error', text: 'failed', help: 'why' });
  assert.ok(ok && warn && err);
  assert.equal(m.nextDeadline(), 1000 + TOAST_LIFE_MS.ok);
  c.t += TOAST_LIFE_MS.ok;
  assert.equal(m.expire(), true);
  assert.deepEqual(m.view().map((t) => t.text), ['careful', 'failed']);
  c.t = 1000 + TOAST_LIFE_MS.warn;
  m.expire();
  assert.deepEqual(m.view().map((t) => t.text), ['failed']);
  assert.equal(m.nextDeadline(), null, 'an error has no deadline');
  c.t += 10 * 60 * 1000;
  assert.equal(m.expire(), false);
  const [shown] = m.view();
  assert.deepEqual({ ...shown, links: undefined }, {
    id: err, tone: 'error', cls: 'srv-toast-error', role: 'alert', text: 'failed', help: 'why', longHelp: false, links: undefined, count: 1,
  });
  assert.equal(m.close(err), true);
  assert.equal(m.close(err), false);
  assert.deepEqual(m.view(), []);
});

test('hover pauses the timer and resumes with the time left', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  const id = m.push({ tone: 'ok', text: 'done' });
  c.t += 1000;
  assert.equal(m.hover(id, true), true);
  assert.equal(m.hover(id, true), false, 'already paused');
  assert.equal(m.nextDeadline(), null);
  c.t += 60000;
  m.expire();
  assert.equal(m.view().length, 1, 'kept while hovered');
  m.hover(id, false);
  assert.equal(m.nextDeadline(), c.t + TOAST_LIFE_MS.ok - 1000);
  c.t += TOAST_LIFE_MS.ok - 1000;
  m.expire();
  assert.equal(m.view().length, 0);
});

test('at most TOAST_MAX at once; the same notice again counts instead of stacking', () => {
  const c = clock();
  const m = createToasts({ now: c.now });
  for (let i = 0; i < TOAST_MAX + 2; i++) m.push({ tone: 'error', text: `e${i}` });
  assert.equal(m.view().length, TOAST_MAX);
  assert.equal(m.view()[0].text, 'e2', 'the oldest go first');
  m.clear();
  const a = m.push({ tone: 'ok', text: 'saved' });
  c.t += 4000;
  const b = m.push({ tone: 'ok', text: 'saved' });
  assert.equal(a, b);
  assert.equal(m.view()[0].count, 2);
  assert.equal(m.nextDeadline(), c.t + TOAST_LIFE_MS.ok, 'its time starts again');
  assert.notEqual(m.push({ tone: 'warn', text: 'saved' }), a, 'another tone is another toast');
});

test('nothing to show: no toast; links must be callable', () => {
  const m = createToasts();
  assert.equal(m.push(null), 0);
  assert.equal(m.push({ tone: 'ok', text: '' }), 0);
  const run = () => {};
  m.push({ tone: 'warn', text: 'imported', links: [{ text: 'Models', run }, { text: 'bad' }, null] });
  assert.deepEqual(m.view()[0].links, [{ text: 'Models', run }]);
});

test('every tab reports outcomes as toasts: no notice banner at the top of a tab', () => {
  const dir = new URL('../', import.meta.url);
  const tabs = readdirSync(dir).filter((f) => f.endsWith('-tab.js'));
  assert.ok(tabs.length >= 10);
  for (const file of tabs) {
    const src = readFileSync(new URL(file, dir), 'utf8');
    assert.ok(!/notice\.value\s*=/.test(src), `${file}: sets a banner notice`);
    assert.ok(!/v-if="notice"/.test(src), `${file}: renders a banner notice`);
    assert.ok(!/sb\.notice/.test(src), `${file}: renders a banner notice`);
  }
  for (const file of ['databases-tab.js', 'users-tab.js', 'mcp-tab.js', 'backups-tab.js', 'import-tab.js', 'export-tab.js', 'models-tab.js', 'runs-tab.js', 'settings-tab.js']) {
    const src = readFileSync(new URL(file, dir), 'utf8');
    assert.match(src, /\bnotify\(/, `${file} uses notify`);
  }
  const shared = readFileSync(new URL('shared.js', dir), 'utf8');
  assert.match(shared, /export function notify\(notice\)/);
  assert.match(shared, /createApp\(ToastStack\)/, 'one stack per page, outside the tabs');
  const css = readFileSync(new URL('server-ui.css', dir), 'utf8');
  assert.match(css, /\.srv-toasts \{[^}]*position: fixed/, 'fixed: the page never moves');
});

test('every form shows its error under its own submit button', () => {
  const dir = new URL('../', import.meta.url);
  const forms = {
    'databases-tab.js': 1, 'users-tab.js': 2, 'backups-tab.js': 4, 'import-tab.js': 1, 'export-tab.js': 1,
    'models-tab.js': 1, 'settings-tab.js': 2, 'login-tab.js': 1,
  };
  for (const [file, count] of Object.entries(forms)) {
    const src = readFileSync(new URL(file, dir), 'utf8');
    const errors = [...src.matchAll(/<form-error\b/g)];
    assert.equal(errors.length, count, `${file}: ${errors.length} form errors`);
    assert.match(src, /FormError/, `${file} registers FormError`);
    // Each one follows a submit/action button (nothing but closing tags between).
    for (const m of errors) {
      const before = src.slice(0, m.index);
      const lastButton = before.lastIndexOf('</button>');
      const lastLink = before.lastIndexOf('</a>');
      const between = before.slice(Math.max(lastButton + 9, lastLink + 4));
      assert.match(between, /^(\s*<\/(div|form|template)>)*\s*$/, `${file}: a form error not right under its button: ${JSON.stringify(between.slice(0, 120))}`);
    }
  }
  // The grant form's error: inside the form, under the Grant button.
  const users = readFileSync(new URL('users-tab.js', dir), 'utf8');
  assert.match(users, /Grant<\/button>\s*<form-error :error="grant\.error" :problems="grant\.problems"><\/form-error>\s*<\/form>/);
});

test('compact toasts: a long help is clamped with a "More" expander; the stack sits bottom right', () => {
  assert.equal(isLongHelp(''), false);
  assert.equal(isLongHelp(undefined), false);
  assert.equal(isLongHelp('one short line'), false);
  assert.equal(isLongHelp('a\nb\nc'), false, 'exactly the clamp lines');
  assert.equal(isLongHelp('a\nb\nc\nd'), true);
  assert.equal(isLongHelp('x'.repeat(HELP_CLAMP.chars + 1)), true);
  const m = createToasts({ now: clock().now });
  m.push({ tone: 'error', text: 'Verify found problems', help: ['db-1: blob missing', 'db-2: blob missing', 'db-3: ok?', 'db-4: blob missing'].join('\n') });
  m.push({ tone: 'ok', text: 'Saved.', help: 'short' });
  assert.deepEqual(m.view().map((t) => t.longHelp), [true, false]);
  const dir = new URL('../', import.meta.url);
  const shared = readFileSync(new URL('shared.js', dir), 'utf8');
  assert.match(shared, /srv-toast-clamped/, 'the view clamps a long help');
  assert.match(shared, /class="srv-toast-more"[^>]*aria-expanded/, 'the expander says whether it is open');
  const css = readFileSync(new URL('server-ui.css', dir), 'utf8');
  const stack = css.match(/\.srv-toasts \{([^}]*)\}/)[1];
  assert.match(stack, /bottom: 14px/, 'bottom right: never over the top bar or the tab bar');
  assert.doesNotMatch(stack, /\btop:/);
  const width = Number(stack.match(/width: min\((\d+)px/)[1]);
  assert.ok(width <= 360, `a capped width (${width}px)`);
  assert.match(css, /\.srv-toast-help\.srv-toast-clamped \{[^}]*line-clamp: 3/);
});
