// Node tests of the KB migration fixes (0.3.1): "Default model…" in the Databases tab (the
// request, when it is enabled, the dialog's choices from the settings view, the outcome) and the
// stored jobs of an import (the "Move backup jobs under the backup root" option, the outcome
// naming the jobs that write outside the backup root).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  adminRequest, defaultModelAction, defaultModelChoices, defaultModelOutcomeText, importJobsText, importRequest, outcomeText,
} from '../admin-api.js';

test('the default-model request: PUT with the repository, or null for none', () => {
  assert.deepEqual(adminRequest.defaultModel('my kb', 'acme/tiny'), {
    method: 'PUT', path: 'api/admin/databases/my%20kb/default-model', body: { model: 'acme/tiny' },
  });
  assert.deepEqual(adminRequest.defaultModel('kb', ''), {
    method: 'PUT', path: 'api/admin/databases/kb/default-model', body: { model: null },
  });
  assert.deepEqual(adminRequest.defaultModel('kb', null).body, { model: null });
});

test('the default model is enabled for a plain database, loaded or not, and disabled with a reason otherwise', () => {
  const plain = { name: 'kb', kind: 'plain', role: 'normal', state: 'unloaded', readOnly: false };
  assert.equal(defaultModelAction(plain).enabled, true);
  assert.equal(defaultModelAction({ ...plain, state: 'loaded' }).enabled, true);
  assert.equal(defaultModelAction({ ...plain, state: 'needs-model' }).enabled, true);
  const off = (row, words) => {
    const a = defaultModelAction(row);
    assert.equal(a.enabled, false, JSON.stringify(row));
    assert.match(a.title, words);
  };
  off({ ...plain, kind: 'layer' }, /base's models/);
  off({ ...plain, role: 'base' }, /frozen/);
  off({ ...plain, readOnly: true }, /read-only off first/);
  off({ ...plain, state: 'loading' }, /once it is loaded/);
  off({ ...plain, state: 'failed' }, /failed/);
  off({ name: 'cur', aliasOf: 'kb' }, /alias of kb/);
  assert.equal(defaultModelAction(plain, { busy: true }).enabled, false);
});

test('the dialog takes its choices from the settings view', () => {
  assert.deepEqual(defaultModelChoices({ store: { models: ['acme/tiny', 'acme/other', 3], defaultModel: 'acme/other' } }), {
    models: ['acme/tiny', 'acme/other'], current: 'acme/other', error: null,
  });
  assert.deepEqual(defaultModelChoices({ store: { models: [], defaultModel: null } }), { models: [], current: '', error: null });
  assert.deepEqual(defaultModelChoices({ storeError: 'gone' }), { models: [], current: '', error: 'gone' });
  assert.deepEqual(defaultModelChoices(null), { models: [], current: '', error: null });
});

test('the default-model outcome says what changed and when it applies', () => {
  assert.equal(
    defaultModelOutcomeText('kb', { name: 'kb', defaultModel: 'acme/tiny', changed: true, loaded: true }),
    'The default model of "kb" is now acme/tiny. Its next queries use it.',
  );
  assert.equal(
    outcomeText('defaultModel', 'kb', { defaultModel: 'acme/tiny', changed: true, loaded: false }),
    'The default model of "kb" is now acme/tiny. It applies at its next load.',
  );
  assert.match(defaultModelOutcomeText('kb', { defaultModel: null, changed: true, loaded: true }), /no default model now: embed\(\) without a model name uses hash-ngram\/1\.$/);
  assert.equal(defaultModelOutcomeText('kb', { defaultModel: 'acme/tiny', changed: false }), 'acme/tiny is the default model of "kb" already.');
});

test('the Databases tab offers "Default model…" on plain rows and sends the chosen model', () => {
  const tab = readFileSync(new URL('../databases-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes("key: 'defaultModel', text: 'Default model…'"));
  assert.ok(tab.includes("row.kind === 'layer' || row.role === 'base' ? []"));
  assert.ok(tab.includes('admin.defaultModel(subject, model)'));
  assert.ok(tab.includes('defaultModelChoices(await settings.view(row.name))'));
  assert.ok(tab.includes('data-default-model'));
});

test('an import sends rebaseJobs only when asked', () => {
  const path = { source: 'path', path: '/data/kb', name: 'kb' };
  assert.deepEqual(importRequest(path).request.body, { path: '/data/kb', name: 'kb' });
  assert.deepEqual(importRequest({ ...path, rebaseJobs: true }).request.body, { path: '/data/kb', name: 'kb', rebaseJobs: true });
  const upload = importRequest({ source: 'upload', file: { size: 10 }, name: 'kb', rebaseJobs: true });
  assert.equal(upload.upload.path, 'api/admin/import?name=kb&rebaseJobs=true');
  const inbox = importRequest({ source: 'inbox', inbox: 'kb.gsnap', name: 'kb', rebaseJobs: true });
  assert.equal(inbox.request.body.rebaseJobs, true);
});

test('the import outcome names the jobs outside the backup root', () => {
  const answer = (jobs) => ({ name: 'kb', state: 'unloaded', kind: 'plain', import: { format: 'store', jobsOutside: jobs } });
  assert.equal(importJobsText(answer([])), '');
  assert.equal(importJobsText({}), '');
  const dropped = importJobsText(answer([{ job: 'nightly', path: '/old', willRun: false, rebasedTo: null }]));
  assert.match(dropped, /^ 1 stored job writes outside the backup root and will not run/);
  assert.match(dropped, /Move backup jobs under the backup root/);
  const moved = importJobsText(answer([{ job: 'a', willRun: true }, { job: 'b', willRun: true }]));
  assert.equal(moved, ' 2 backup jobs moved under the backup root.');
  assert.match(outcomeText('import', 'kb', answer([{ job: 'a', willRun: false }])), /^Imported "kb" \(store, plain\): unloaded\. 1 stored job writes/);
  const tab = readFileSync(new URL('../import-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes('v-model="form.rebaseJobs"'));
  assert.ok(tab.includes('data-import-rebase-jobs'));
});
