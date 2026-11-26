// Node tests of the crash-recovery parts of the web UI's models (round 3 B-CRASH): the state
// `maintenance` in the database list, the verify/repair/swap requests and actions, the texts of
// a verify, of "Verify all" and of a repair report, the last unclean shutdown's rows
// (ui/admin-api.js, ui/databases.js) and the Database settings' "Damage and recovery" part
// (ui/settings-api.js), and a round against a mock of the admin API (node:http on port 0,
// shaped after book/src/reference/admin_api.md "Verify, repair and swap"): verify, repair, swap, recovery.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, createAdmin, isOpenState, damageActions, repairAction, repairedName, verifyDamaged,
  verifyText, verifyAllText, problemLines, repairSummary, uncleanRows, confirmText, outcomeText,
  restoreOverAction, promoteAction, poolText, NAME_MAX, newNameReason,
} from '../admin-api.js';
import { databaseRows, memoryText, SIZE_COLUMNS } from '../databases.js';
import { recoveryView } from '../settings-api.js';

const DAMAGE = '1 damaged item(s); readable state: snapshot 0 + WAL at commit 0; a repair LOSES data (see the items without a donor)\n  DAMAGE snap/...';

test('the state maintenance: loaded read-only with its damage report', () => {
  const { rows } = databaseRows({
    databases: [
      { name: 'acme', state: 'maintenance', kind: 'plain', role: 'normal', reason: 'its Store is damaged: served read-only in maintenance mode', damage: DAMAGE, memoryBytes: 2048 },
      { name: 'fine', state: 'loaded', kind: 'plain', role: 'normal' },
    ],
  });
  const acme = rows[0];
  assert.equal(acme.state, 'maintenance');
  assert.equal(acme.stateText, 'maintenance (read-only, damaged)');
  assert.equal(acme.tone, 'danger');
  assert.equal(acme.damage, DAMAGE);
  assert.equal(rows[1].damage, '');
  assert.ok(isOpenState('maintenance') && isOpenState('loaded'));
  assert.ok(!isOpenState('loading') && !isOpenState('unloaded') && !isOpenState('failed'));
  // Loaded: its memory shows, it must be unloaded before a restore over it or a promotion.
  assert.equal(memoryText(acme), '2.0 KB');
  assert.equal(SIZE_COLUMNS.memory.value(acme), 2048);
  assert.equal(restoreOverAction(acme).enabled, false);
  assert.match(promoteAction(acme).title, /Unload it first/);
  assert.equal(poolText(acme), acme.stateText);
});

test('requests of verify, repair, swap and the last unclean shutdown', () => {
  assert.deepEqual(adminRequest.verify('a b'), { method: 'POST', path: 'api/admin/databases/a%20b/verify' });
  assert.deepEqual(adminRequest.verifyAll(), { method: 'POST', path: 'api/admin/verify' });
  assert.deepEqual(adminRequest.repair('acme'), { method: 'POST', path: 'api/admin/databases/acme/repair', body: {} });
  assert.deepEqual(adminRequest.repair('acme', 'acme-fixed'), {
    method: 'POST', path: 'api/admin/databases/acme/repair', body: { name: 'acme-fixed' },
  });
  assert.deepEqual(adminRequest.swap('acme', 'acme-repaired'), {
    method: 'POST', path: 'api/admin/databases/acme/swap', body: { with: 'acme-repaired', confirm: true },
  });
  assert.deepEqual(adminRequest.recovery(), { method: 'GET', path: 'api/admin/recovery' });
});

test('damage actions: verify everything but a loading database, repair plain databases only', () => {
  const { rows } = databaseRows({
    databases: [
      { name: 'acme', state: 'maintenance', kind: 'plain', role: 'normal' },
      { name: 'off', state: 'unloaded', kind: 'plain', role: 'normal' },
      { name: 'busy', state: 'loading', kind: 'plain', role: 'normal' },
      { name: 'layer', state: 'loaded', kind: 'layer', role: 'normal', base: 'base' },
      { name: 'base', state: 'unloaded', kind: 'plain', role: 'base' },
      { name: 'bad', state: 'failed', reason: 'unreadable' },
      { name: 'alias', state: 'loaded', kind: 'plain', role: 'normal', aliasOf: 'off' },
    ],
  });
  const a = Object.fromEntries(rows.map((r) => [r.name, damageActions(r)]));
  assert.ok(a.acme.verify.enabled && a.acme.repair.enabled);
  assert.match(a.acme.repair.title, /damaged/);
  assert.ok(a.off.verify.enabled && a.off.repair.enabled);
  assert.ok(!a.busy.verify.enabled && !a.busy.repair.enabled);
  assert.ok(a.layer.verify.enabled);
  assert.match(a.layer.repair.title, /restore a backup/);
  assert.match(a.base.repair.title, /restore a backup/);
  assert.ok(a.bad.verify.enabled, 'a failed database: its files are verified');
  assert.match(a.bad.repair.title, /configuration cannot be read/);
  assert.match(a.alias.verify.title, /alias of off/);
  assert.equal(damageActions(rows[1], { busy: true }).verify.enabled, false);
  assert.equal(repairAction(rows[1], { busy: true }).enabled, false);
});

test('the default name of a repaired copy is a valid new name', () => {
  assert.equal(repairedName('acme'), 'acme-repaired');
  assert.equal(repairedName('Old..Name_'), 'old.name-repaired');
  const long = repairedName('a'.repeat(63));
  assert.equal(long.length, NAME_MAX);
  assert.equal(newNameReason(long), null);
  assert.equal(repairedName(''), 'repaired');
});

test('verify texts: ok, damaged, maintenance, a failure, every database', () => {
  const ok = { database: 'acme', ok: true, state: 'loaded', problems: [], snapshotsChecked: 1, recordsChecked: 30 };
  const bad = {
    database: 'acme', ok: false, state: 'maintenance',
    problems: [{ path: 'snapshots/1/v.seg', offset: 4096, reason: 'chunk crc mismatch' }, { path: 'wal/0.wal', offset: null, reason: 'gap' }],
  };
  assert.equal(verifyText(ok), '"acme" verifies: no problem found (1 snapshot, 30 journal records).');
  assert.ok(!verifyDamaged(ok));
  assert.equal(verifyText(bad), '"acme" is DAMAGED: 2 problems; first: snapshots/1/v.seg: chunk crc mismatch.');
  assert.ok(verifyDamaged(bad));
  assert.deepEqual(problemLines(bad), ['snapshots/1/v.seg at byte 4096: chunk crc mismatch', 'wal/0.wal: gap']);
  assert.match(verifyText({ ...ok, state: 'maintenance' }), /in maintenance/);
  assert.ok(verifyDamaged({ ...ok, state: 'maintenance' }));
  assert.match(verifyText({ database: 'x', ok: false, error: 'boom' }), /the verify failed: boom/);
  assert.match(verifyText(null), /nothing/);
  assert.deepEqual(verifyAllText({ results: [ok] }), { tone: 'ok', text: 'Every database verifies (1).', lines: [] });
  const all = verifyAllText({ results: [ok, bad, { database: 'x', ok: false, error: 'boom' }], ok: false });
  assert.equal(all.tone, 'error');
  assert.match(all.text, /^2 databases of 3 damaged or not verifiable: acme, x\./);
  // B-VERIFY: each database's line is followed by its problem lines (acme has two).
  assert.equal(all.lines.length, 4);
  assert.equal(verifyAllText({ results: [] }).text, 'No databases to verify.');
  assert.equal(outcomeText('verify', 'acme', ok), verifyText(ok));
});

test('a repair report in words; the swap confirmation and outcome', () => {
  const lossless = repairSummary({
    name: 'acme-repaired', repairedFrom: 'acme',
    repair: { commitSeq: 12, lossless: true, repaired: ['vertex ids "1"..="6" from snapshot 0 + WAL'], lostCommits: [], lostRanges: [], lostElements: [], divergedCount: 0, modelsNotCarried: [], notCarried: 'the history before commit 12 and the store id (backups start anew)' },
  });
  assert.equal(lossless.tone, 'ok');
  assert.match(lossless.text, /^Repaired "acme" into the new database "acme-repaired" at commit 12: lossless, verified\.$/);
  assert.deepEqual(lossless.lines, ['rebuilt: vertex ids "1"..="6" from snapshot 0 + WAL', 'never carried by a repair: the history before commit 12 and the store id (backups start anew)']);
  const lossy = repairSummary({
    name: 'r', repairedFrom: 'acme',
    repair: { commitSeq: 3, lossless: false, lostCommits: [{ first: 4, last: 9 }, { first: 11, last: null }], lostRanges: ['vertex ids "1"..="3"'], lostElements: ['edge 7'], divergedCount: 2, modelsNotCarried: ['org/m'] },
  });
  assert.equal(lossy.tone, 'warn');
  assert.match(lossy.text, /DATA LOST/);
  assert.deepEqual(lossy.lines, [
    'lost: commits 4..9', 'lost: commits from 11 on', 'lost: vertex ids "1"..="3"', 'lost: edge 7',
    'diverged: 2 elements, check them by hand', 'model not carried: org/m',
  ]);
  assert.match(confirmText('swap', 'acme', 'acme-repaired'), /Let "acme-repaired" take the name "acme"\?.*trash\//);
  assert.equal(
    outcomeText('swap', 'acme', { name: 'acme', swappedWith: 'acme-repaired', trash: 'trash/acme-2026', reloaded: true }),
    '"acme-repaired" took the name "acme"; the old one moved to trash/acme-2026; loaded again.',
  );
});

const REPORT = {
  detectedAtMs: Date.UTC(2026, 10, 26, 10, 0, 0, 123),
  previousPid: 4242,
  runFileLeft: true,
  databasesKnown: true,
  done: true,
  databases: [
    { name: 'acme', outcome: 'ok', state: 'loaded', recovery: { cleanClose: false, commitsReplayed: 25, snapshotCommitSeq: 0, commitSeq: 25, tornTail: '41 bytes cut at offset 9000 of the WAL segment 1' }, verify: { ok: true, problems: [] } },
    { name: 'beta', outcome: 'damaged', state: 'maintenance', recovery: { cleanClose: false, commitsReplayed: 0, snapshotCommitSeq: 0, commitSeq: 0, maintenance: true }, verify: { ok: false, problems: [{ path: 'x', reason: 'y' }] }, damage: DAMAGE },
    { name: 'gone', outcome: 'missing', message: 'no database gone' },
  ],
};

test('the last unclean shutdown as rows', () => {
  const u = uncleanRows(REPORT);
  assert.equal(u.when, '2026-11-26T10:00:00Z');
  assert.equal(u.pid, 4242);
  assert.ok(u.done && u.known);
  assert.deepEqual(u.rows.map((r) => [r.name, r.outcome, r.tone, r.damaged]), [
    ['acme', 'ok', 'ok', false], ['beta', 'damaged', 'danger', true], ['gone', 'missing', 'warn', false],
  ]);
  assert.equal(u.rows[0].text, 'not closed cleanly; 25 journal commits replayed, at commit 25; torn tail cut (41 bytes cut at offset 9000 of the WAL segment 1); verify ok');
  assert.match(u.rows[1].text, /verify found 1 problem$/);
  assert.equal(u.rows[2].text, 'no database gone');
  assert.equal(uncleanRows(null), null);
  assert.equal(uncleanRows({ databasesKnown: false, databases: [], done: false }).known, false);
});

test('the settings view: damage and this database\'s unclean-shutdown check', () => {
  const view = recoveryView({
    recovery: {
      maintenance: true, damage: DAMAGE, help: 'repair it ...',
      uncleanShutdown: { detectedAtMs: REPORT.detectedAtMs, previousPid: 4242, check: REPORT.databases[1] },
    },
  });
  assert.equal(view.maintenance, true);
  assert.equal(view.damage, DAMAGE);
  assert.equal(view.unclean.outcome, 'damaged');
  assert.deepEqual(view.unclean.rows.map((r) => r[0]), ['Outcome', 'State afterwards', 'Closed cleanly', 'Journal replayed', 'Opened', 'Verify']);
  assert.deepEqual(recoveryView({}), { maintenance: false, damage: '', help: '', unclean: null });
  assert.equal(recoveryView({ recovery: { maintenance: false, damage: null, uncleanShutdown: null } }).unclean, null);
});

test('a round against a mock admin API: verify, repair, swap, recovery', async () => {
  const seen = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      seen.push([req.method, req.url, body ? JSON.parse(body) : null]);
      const send = (status, json) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(json)); };
      if (req.url === '/api/admin/databases/acme/verify') return send(200, { database: 'acme', ok: false, state: 'maintenance', problems: [{ path: 'p', offset: 1, reason: 'r' }], help: 'repair it' });
      if (req.url === '/api/admin/verify') return send(200, { results: [{ database: 'acme', ok: true, state: 'loaded', problems: [] }], ok: true });
      if (req.url === '/api/admin/databases/acme/repair') return send(201, { name: 'acme-repaired', state: 'unloaded', repairedFrom: 'acme', repair: { commitSeq: 1, lossless: true } });
      if (req.url === '/api/admin/databases/acme/swap') {
        const parsed = JSON.parse(body);
        if (!parsed.confirm) return send(400, { error: 'confirm it' });
        return send(200, { name: 'acme', swappedWith: parsed.with, trash: 'trash/acme-x', reloaded: false });
      }
      if (req.url === '/api/admin/recovery') return send(200, { uncleanShutdown: REPORT });
      return send(404, { error: 'unknown' });
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}/`;
    const admin = createAdmin({ base, fetch: globalThis.fetch });
    const verified = await admin.verify('acme');
    assert.ok(verifyDamaged(verified));
    assert.ok((await admin.verifyAll()).ok);
    const repaired = await admin.repair('acme', 'acme-repaired');
    assert.equal(repairSummary(repaired).tone, 'ok');
    const swapped = await admin.swap('acme', repaired.name);
    assert.equal(swapped.swappedWith, 'acme-repaired');
    const recovery = await admin.recovery();
    assert.equal(uncleanRows(recovery.uncleanShutdown).rows.length, 3);
    assert.deepEqual(seen.map((s) => [s[0], s[1]]), [
      ['POST', '/api/admin/databases/acme/verify'], ['POST', '/api/admin/verify'],
      ['POST', '/api/admin/databases/acme/repair'], ['POST', '/api/admin/databases/acme/swap'],
      ['GET', '/api/admin/recovery'],
    ]);
    assert.deepEqual(seen[2][2], { name: 'acme-repaired' });
    assert.deepEqual(seen[3][2], { with: 'acme-repaired', confirm: true });
  } finally {
    server.close();
  }
});
