// Node tests of B-VERIFY in the web UI's models: Verify all database by database with its
// progress `x/y` and Cancel (ui/admin-api.js `runVerifyAll`, `verifyAllLabel`, `verifyAllText`),
// a missing embedding model blob named as its model with the remedy (`problemText`,
// `verifyText`, `verifyHelp`), the row badge of a last verify that found something
// (`verifyBadge`, ui/databases.js `lastVerifyOf`) and the Database settings' verify wording
// (ui/settings-api.js `verifyView`), plus the templates that use them.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  runVerifyAll, verifyAllLabel, verifyAllText, verifyVerdict, verifyText, verifyHelp, problemText, problemLines, verifyBadge,
} from '../admin-api.js';
import { databaseRows, lastVerifyOf } from '../databases.js';
import { verifyView } from '../settings-api.js';

const HASH = 'f0ed7648aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const MODEL_PROBLEM = {
  path: `blobs/f0/${HASH}`, offset: null, reason: 'the blob is missing', kind: 'model', model: 'acme/tiny', blob: HASH,
  remedy: 'download the model acme/tiny ...',
};
const NEEDS_MODEL = {
  database: 'kb-test', ok: false, verdict: 'needs-model', state: 'needs-model', how: 'files',
  modelsMissing: ['acme/tiny'], problems: [MODEL_PROBLEM], snapshotsChecked: 1, recordsChecked: 0,
};
const OK = { database: 'acme', ok: true, verdict: 'ok', state: 'unloaded', problems: [], snapshotsChecked: 1, recordsChecked: 3 };
const DAMAGED = {
  database: 'bad', ok: false, verdict: 'damaged', state: 'unloaded',
  problems: [{ path: 'snapshots/1/v.seg', offset: 12, reason: 'crc mismatch', kind: 'damage' }],
};

test('Verify all runs database by database: progress x/y after each, a failure is a result', async () => {
  const seen = [];
  const calls = [];
  const run = await runVerifyAll(['a', 'b', 'c'], async (name) => {
    calls.push(name);
    if (name === 'b') throw new Error('the server is gone');
    return { ...OK, database: name };
  }, { progress: (done, total) => seen.push(`${done}/${total}`) });
  assert.deepEqual(calls, ['a', 'b', 'c']);
  assert.deepEqual(seen, ['0/3', '1/3', '2/3', '3/3']);
  assert.equal(run.cancelled, false);
  assert.equal(run.results.length, 3);
  assert.deepEqual(run.results[1], { database: 'b', ok: false, error: 'the server is gone' });
  assert.equal(verifyVerdict(run.results[1]), 'failed');
  const empty = await runVerifyAll([], async () => OK);
  assert.deepEqual(empty, { results: [], cancelled: false });
});

test('Verify all: Cancel stops before the next database; the running one ends', async () => {
  const state = { cancel: false };
  const calls = [];
  const run = await runVerifyAll(['a', 'b', 'c', 'd'], async (name) => {
    calls.push(name);
    if (name === 'b') state.cancel = true; // pressed while b runs
    return { ...OK, database: name };
  }, { cancelled: () => state.cancel });
  assert.deepEqual(calls, ['a', 'b']);
  assert.equal(run.cancelled, true);
  const text = verifyAllText(run.results, { total: 4, cancelled: true });
  assert.equal(text.tone, 'warn');
  assert.equal(text.text, 'Every database verified so far verifies (2 of 4). Cancelled after 2 of 4.');
  assert.match(verifyAllText([], { total: 4, cancelled: true }).text, /cancelled before the first/);
});

test('the Verify all button shows the progress, then Cancelling…', () => {
  assert.equal(verifyAllLabel({ running: false, done: 0, total: 0 }), 'Verify all');
  assert.equal(verifyAllLabel({ running: true, done: 3, total: 10, cancel: false }), 'Verifying 3/10…');
  assert.equal(verifyAllLabel({ running: true, done: 3, total: 10, cancel: true }), 'Cancelling…');
  assert.equal(verifyAllLabel(null), 'Verify all');
});

test('a missing model blob is named as its model, with the remedy; other blobs keep their path', () => {
  assert.equal(problemText(MODEL_PROBLEM), 'the embedding model acme/tiny (blob f0ed7648) is missing');
  assert.equal(
    problemText({ ...MODEL_PROBLEM, model: undefined, candidates: ['x/one', 'x/two'] }),
    'the blob f0ed7648 of one of the embedding models x/one, x/two is missing',
  );
  assert.equal(problemText({ path: 'blobs/aa/aa11', offset: null, reason: 'the blob is missing', kind: 'damage' }), 'blobs/aa/aa11: the blob is missing');
  assert.equal(verifyVerdict(NEEDS_MODEL), 'needs-model');
  assert.equal(verifyText(NEEDS_MODEL), '"kb-test" needs its embedding model: acme/tiny is missing (the graph data is intact).');
  const help = verifyHelp(NEEDS_MODEL);
  assert.match(help, /^the embedding model acme\/tiny \(blob f0ed7648\) is missing\n/);
  assert.match(help, /Download acme\/tiny .*Models tab.*graphersal-server models add SOURCE/);
  assert.doesNotMatch(help, /Repair/);
  const shared = { ...NEEDS_MODEL, problems: [{ ...MODEL_PROBLEM, shared: true }] };
  assert.match(verifyHelp(shared), /shared models hold acme\/tiny.*models add acme\/tiny/);
  assert.match(verifyHelp(DAMAGED, 'Or restore a backup.'), /snapshots\/1\/v.seg at byte 12: crc mismatch\nRepair… .*Or restore a backup\./);
  assert.equal(verifyHelp(OK), '');
  // An older server (no verdict, no kind): as before.
  assert.equal(verifyVerdict({ database: 'x', ok: false, problems: [] }), 'damaged');
  assert.equal(verifyVerdict({ database: 'x', ok: true }), 'ok');
});

test('Verify all: one toast text with every problem line, models apart from damage', () => {
  const summary = verifyAllText([OK, NEEDS_MODEL, DAMAGED], { total: 3 });
  assert.equal(summary.tone, 'error');
  assert.equal(
    summary.text,
    '1 database of 3 damaged or not verifiable: bad. Repair one into a new database, or restore a backup. '
      + '1 database of 3 needs an embedding model: kb-test. Add the missing model on the Models tab.',
  );
  assert.deepEqual(summary.lines.slice(0, 3), [
    verifyText(NEEDS_MODEL),
    '  the embedding model acme/tiny (blob f0ed7648) is missing',
    summary.lines[2],
  ]);
  assert.match(summary.lines[2], /^ {2}Download acme\/tiny/);
  assert.ok(summary.lines.includes('  snapshots/1/v.seg at byte 12: crc mismatch'));
  // Only a missing model: a warning, no repair advice.
  const models = verifyAllText({ results: [OK, NEEDS_MODEL] });
  assert.equal(models.tone, 'warn');
  assert.doesNotMatch(models.text, /Repair/);
  assert.deepEqual(problemLines(NEEDS_MODEL), ['the embedding model acme/tiny (blob f0ed7648) is missing']);
});

test('a row shows a badge when its last verify found something', () => {
  const at = Date.UTC(2026, 10, 26, 10, 0, 0);
  const { rows } = databaseRows({
    databases: [
      { name: 'a', state: 'unloaded', lastVerify: { atMs: at, verdict: 'ok', problemCount: 0, modelsMissing: [] } },
      { name: 'b', state: 'unloaded', lastVerify: { atMs: at, verdict: 'damaged', problemCount: 3, modelsMissing: [] } },
      { name: 'c', state: 'needs-model', lastVerify: { atMs: at, verdict: 'needs-model', problemCount: 1, modelsMissing: ['acme/tiny'] } },
      { name: 'd', state: 'unloaded', lastVerify: null },
      { name: 'e', state: 'unloaded' },
    ],
  });
  assert.equal(verifyBadge(rows[0]), null);
  assert.deepEqual(verifyBadge(rows[1]), {
    text: 'verify: 3 problems', tone: 'danger', title: 'Last verify 2026-11-26T10:00:00Z: 3 problems; the Database settings show them',
  });
  assert.equal(verifyBadge(rows[2]).text, 'verify: needs model');
  assert.match(verifyBadge(rows[2]).title, /acme\/tiny is missing; add it on the Models tab/);
  assert.equal(rows[3].lastVerify, null);
  assert.equal(rows[4].lastVerify, null);
  assert.equal(verifyBadge(rows[4]), null);
  assert.equal(lastVerifyOf({ verdict: 7 }), null);
  assert.equal(verifyBadge({ lastVerify: { atMs: at, verdict: 'failed', problemCount: 0, modelsMissing: [] } }).text, 'verify failed');
});

test('Database settings: the last verify, the needs-model state and never verified in one wording', () => {
  const at = Date.UTC(2026, 10, 26, 10, 0, 0);
  const never = verifyView({ recovery: { maintenance: false, damage: null, needsModel: null, lastVerify: null } });
  assert.deepEqual(never, { tone: '', text: "Never verified: Verify (Databases tab, the row's … menu) checks the whole Store.", problems: [], more: 0, remedy: '' });
  assert.deepEqual(verifyView({}), never);
  const ok = verifyView({ recovery: { lastVerify: { atMs: at, verdict: 'ok', problemCount: 0, problems: [] } } });
  assert.equal(ok.text, 'Last verify 2026-11-26T10:00:00Z: no problem found.');
  assert.equal(ok.tone, 'ok');
  // The defect of the user's report: a needs-model database whose verify found the blob.
  const model = verifyView({
    recovery: {
      maintenance: false, needsModel: 'acme/tiny',
      lastVerify: { atMs: at, verdict: 'needs-model', problemCount: 1, problems: [MODEL_PROBLEM], modelsMissing: ['acme/tiny'] },
    },
  });
  assert.equal(model.tone, 'warn');
  assert.equal(model.text, 'Last verify 2026-11-26T10:00:00Z: the embedding model acme/tiny is missing (the graph data is intact; not damage).');
  assert.deepEqual(model.problems, ['the embedding model acme/tiny (blob f0ed7648) is missing']);
  assert.match(model.remedy, /Download acme\/tiny .*Models tab/);
  // Needs a model, never verified: still says so.
  const unverified = verifyView({ recovery: { needsModel: 'acme/tiny', lastVerify: null } });
  assert.equal(unverified.tone, 'warn');
  assert.match(unverified.text, /^It needs the embedding model acme\/tiny: .*Never verified/);
  // Damage: the count, the first problems, "and N more", repair or restore.
  const damaged = verifyView({
    recovery: { lastVerify: { atMs: at, verdict: 'damaged', problemCount: 150, problems: [{ path: 'wal/0.wal', offset: 9, reason: 'gap', kind: 'damage' }] } },
  });
  assert.equal(damaged.text, 'Last verify 2026-11-26T10:00:00Z: 150 problems: its Store is damaged.');
  assert.deepEqual(damaged.problems, ['wal/0.wal at byte 9: gap']);
  assert.equal(damaged.more, 149);
  assert.match(damaged.remedy, /Repair it into a new database/);
  const failed = verifyView({ recovery: { lastVerify: { atMs: at, verdict: 'failed', error: 'not a store' } } });
  assert.equal(failed.tone, 'error');
  assert.match(failed.text, /the verify itself failed: not a store/);
});

test('the templates: the progress button with Cancel, the row badge, the settings verify block', () => {
  const tab = readFileSync(new URL('../databases-tab.js', import.meta.url), 'utf8');
  assert.match(tab, /\{\{ verifyAllLabel\(verifyRun\) \}\}/);
  assert.match(tab, /v-if="verifyRun\.running"[^>]*@click="cancelVerifyAll"[^>]*data-verify-cancel/);
  assert.match(tab, /runVerifyAll\(names, \(name\) => admin\.verify\(name\)/);
  assert.doesNotMatch(tab, /admin\.verifyAll\(\)/);
  assert.match(tab, /data-verify-badge/);
  const settings = readFileSync(new URL('../settings-tab.js', import.meta.url), 'utf8');
  assert.match(settings, /data-settings-verify/);
  assert.doesNotMatch(settings, /No damage known/);
});
