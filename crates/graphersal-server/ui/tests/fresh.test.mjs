// Node tests of the fresh data on every visit of a tab (ui/fresh.js): a read when the tab is set
// up, on every later visit (the playground keeps tabs alive), on focus or visibility while it is
// shown (at most one per FOCUS_GAP_MS), never while it is hidden or gone; one read at a time;
// and, against a mock admin API, a user added behind the page's back (the console) shows up on
// the next visit of the Users tab, with the open Add dialog's input untouched.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { createFreshness, listenForReturn, FOCUS_GAP_MS } from '../fresh.js';
import { createAdmin, userRows } from '../admin-api.js';

/** A clock the test moves by hand. */
function clock(start = 10_000) {
  let t = start;
  return { now: () => t, advance: (ms) => { t += ms; } };
}

/** A refresh that counts its calls and resolves when the test says so (or at once). */
function counter({ hold = false } = {}) {
  const c = { calls: 0, release: null };
  c.refresh = () => {
    c.calls++;
    if (!hold) return Promise.resolve();
    return new Promise((resolve) => { c.release = resolve; });
  };
  return c;
}

test('a tab reads its data when set up and on every later visit, not while hidden', async () => {
  const c = counter();
  const t = clock();
  const f = createFreshness(c.refresh, { now: t.now });
  assert.equal(f.active, false);
  await f.mounted();
  assert.equal(c.calls, 1, 'the first read');
  assert.equal(f.active, true);
  assert.equal(f.activated(), null, 'an activation right after the set-up reads nothing more');
  assert.equal(c.calls, 1);
  f.deactivated();
  assert.equal(f.active, false);
  t.advance(5000);
  assert.equal(f.shown(), null, 'focus while another tab is shown reads nothing');
  assert.equal(c.calls, 1);
  await f.activated();
  assert.equal(c.calls, 2, 'the tab is shown again: it reads again');
  f.deactivated();
  await f.activated();
  assert.equal(c.calls, 3, 'every visit, however soon after the last one');
  assert.equal(f.reads, 3);
});

test('focus and visibility read again while the tab is shown, once per gap', async () => {
  const c = counter();
  const t = clock();
  const f = createFreshness(c.refresh, { now: t.now });
  await f.mounted();
  assert.equal(f.shown(), null, 'right after a read: no second one');
  t.advance(FOCUS_GAP_MS);
  await f.shown();
  assert.equal(c.calls, 2);
  assert.equal(f.shown(), null, 'focus and visibilitychange arrive together: one read');
  assert.equal(c.calls, 2);
});

test('one read at a time: a trigger during a read gets that read', async () => {
  const c = counter({ hold: true });
  const t = clock();
  const f = createFreshness(c.refresh, { now: t.now });
  const first = f.mounted();
  f.deactivated();
  const again = f.activated();
  assert.equal(again, first, 'the running read');
  await Promise.resolve();
  assert.equal(c.calls, 1);
  c.release();
  await first;
  f.deactivated();
  const second = f.activated();
  assert.notEqual(second, first);
  await Promise.resolve();
  assert.equal(c.calls, 2, 'after it ended a visit reads again');
  c.release();
  await second;
});

test('a failing refresh never throws out of a trigger; a gone tab reads nothing', async () => {
  let calls = 0;
  const f = createFreshness(() => { calls++; throw new Error('offline'); });
  await f.mounted();
  assert.equal(calls, 1);
  f.unmounted();
  assert.equal(f.activated(), null);
  assert.equal(f.shown(), null);
  assert.equal(calls, 1);
});

test('listenForReturn: focus and becoming visible call shown, becoming hidden does not', () => {
  const win = new EventTarget();
  const doc = new EventTarget();
  doc.visibilityState = 'visible';
  let shown = 0;
  const unlisten = listenForReturn({ shown: () => { shown++; } }, win, doc);
  win.dispatchEvent(new Event('focus'));
  assert.equal(shown, 1);
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(shown, 2);
  doc.visibilityState = 'hidden';
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(shown, 2, 'hidden: no read');
  unlisten();
  doc.visibilityState = 'visible';
  win.dispatchEvent(new Event('focus'));
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(shown, 2, 'removed with the tab');
  assert.doesNotThrow(() => listenForReturn({ shown() {} }, undefined, undefined)());
});

/** A mock of `GET /api/admin/users` over `users` (the test changes it like the console does). */
async function usersServer(users) {
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/api/admin/users') {
      res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
      res.end(JSON.stringify({
        users: users.map((name) => ({ name, roles: [], admin: false, token: '…abcd', grants: [] })),
        grants: [],
        revision: String(users.length),
      }));
      return;
    }
    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end('{"error": "unknown"}');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, base: `http://127.0.0.1:${server.address().port}/` };
}

test('a user added in the console shows up on the next visit; the open dialog keeps its input', async () => {
  const users = ['alice'];
  const { server, base } = await usersServer(users);
  try {
    const admin = createAdmin({ base, fetch: (...a) => globalThis.fetch(...a), headers: {} });
    // What the Users tab holds: the rows it shows, and the Add dialog the admin is typing in.
    const tab = { users: [], add: { name: 'car', admin: false } };
    const refresh = async () => { tab.users = userRows(await admin.users()).map((u) => u.name); };
    const t = clock();
    const f = createFreshness(refresh, { now: t.now });
    await f.mounted();
    assert.deepEqual(tab.users, ['alice']);
    f.deactivated(); // the admin opens another tab
    users.push('bob'); // `graphersal-server user add bob` in the console
    await f.activated(); // and comes back
    assert.deepEqual(tab.users, ['alice', 'bob'], 'the console\'s user is shown without a page reload');
    assert.deepEqual(tab.add, { name: 'car', admin: false }, 'the dialog input is untouched');
    users.push('dave'); // another browser adds one while this window is in the background
    t.advance(FOCUS_GAP_MS);
    await f.shown(); // the window gets the focus back
    assert.deepEqual(tab.users, ['alice', 'bob', 'dave']);
  } finally {
    server.close();
  }
});
