// Node tests of "Reset layer…" (round 4 B-LAYER): the request, the menu item in the Databases
// tab (layer rows only), when it is enabled, the confirmation (what is lost, what is kept) and
// the outcome toast.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  adminRequest, confirmText, outcomeText, resetLayerAction, resetLayerConfirmText, resetLayerOutcomeText,
} from '../admin-api.js';

test('the request: POST reset-layer with the confirmation in the body', () => {
  assert.deepEqual(adminRequest.resetLayer('my layer'), {
    method: 'POST', path: 'api/admin/databases/my%20layer/reset-layer', body: { confirm: true },
  });
});

test('enabled for a layer (loaded or not), disabled with a reason for every other row', () => {
  const layer = { name: 'l', kind: 'layer', role: 'normal', base: 'b', state: 'unloaded' };
  assert.equal(resetLayerAction(layer).enabled, true);
  assert.match(resetLayerAction(layer).title, /"b"/);
  assert.equal(resetLayerAction({ ...layer, state: 'loaded' }).enabled, true);
  assert.equal(resetLayerAction({ ...layer, state: 'maintenance' }).enabled, true);
  const off = (row, re) => {
    const a = resetLayerAction(row);
    assert.equal(a.enabled, false);
    assert.match(a.title, re);
  };
  off({ ...layer, state: 'loading' }, /Loading/);
  off({ ...layer, state: 'failed' }, /failed/);
  off({ name: 'p', kind: 'plain', role: 'normal', state: 'unloaded' }, /Only a layer/);
  off({ name: 'b', kind: 'plain', role: 'base', state: 'loaded' }, /base has no changes/);
  off({ name: 'a', aliasOf: 'l', kind: 'layer' }, /alias/);
  assert.equal(resetLayerAction(layer, { busy: true }).enabled, false);
});

test('the confirmation names what is lost and what is kept', () => {
  const text = confirmText('resetLayer', 'scratch', 'world');
  assert.equal(text, resetLayerConfirmText('scratch', 'world'));
  assert.match(text, /^Reset the layer "scratch"\?/);
  assert.match(text, /over its base "world" is discarded/);
  for (const lost of ['added, changed and removed', 'schema and saved queries', 'history']) assert.ok(text.includes(lost), lost);
  assert.match(text, /base and the other layers stay unchanged/);
  assert.match(text, /trash\/ directory \(recoverable by hand only\)/);
  for (const kept of ['name', 'description', 'read-only and auto-load flags', 'query limits', 'grants', 'aliases', 'MCP settings', 'scheduled jobs']) {
    assert.ok(text.split('Kept:')[1].includes(kept), kept);
  }
  assert.match(text, /unloaded and loaded again/);
});

test('the outcome toast: counts, trash, reload', () => {
  const answer = {
    name: 'scratch', base: 'world', trash: 'trash/scratch-2026-11-26T10-00-00Z', reloaded: true,
    discarded: { added: { vertices: 2, edges: 1 }, modified: { vertices: 1, edges: 0 }, removed: { vertices: 0, edges: 1 } },
  };
  const text = outcomeText('resetLayer', 'scratch', answer);
  assert.equal(text, resetLayerOutcomeText('scratch', answer));
  assert.equal(text, '"scratch" was reset and reads exactly its base "world" now: discarded 3 added elements, 1 changed element and 1 removal; the old layer moved to trash/scratch-2026-11-26T10-00-00Z; loaded again.');
  const none = outcomeText('resetLayer', 'scratch', { name: 'scratch', base: 'world', trash: 'trash/x', discarded: {} });
  assert.equal(none, '"scratch" was reset and reads exactly its base "world" now: it had no data changes; the old layer moved to trash/x.');
});

test('the Databases tab: "Reset layer…" in a layer row\'s menu, confirmed, outcome as a toast', () => {
  const tab = readFileSync(new URL('../databases-tab.js', import.meta.url), 'utf8');
  assert.match(tab, /row\.kind === 'layer' \? \[\{ key: 'resetLayer', text: 'Reset layer…', danger: true/);
  assert.ok(tab.includes("window.confirm(confirmText('resetLayer', row.name, row.base || ''))"));
  assert.ok(tab.includes("run(row.name, 'resetLayer', () => admin.resetLayer(row.name))"));
  // After "Promote to base…" (the store actions group).
  assert.ok(tab.indexOf("key: 'resetLayer'") > tab.indexOf("key: 'promote'"));
  assert.ok(tab.indexOf("key: 'resetLayer'") < tab.indexOf("key: 'sep-api'"));
});
