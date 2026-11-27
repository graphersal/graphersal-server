// Node tests of the Models tab's Hugging Face download (S1): the requests, the form's rules (the
// server's refusals, src/ops_models.rs), the state in words (progress, licence and warnings,
// failure, cancel), the toast of an ended download, and the tab's template (the form, the
// progress with Cancel, the poll, the "Download…" of a waiting database).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { adminRequest, downloadRequest, downloadView, downloadNotice } from '../admin-api.js';

test('the requests: start (revision as modelRevision), progress, cancel', () => {
  assert.deepEqual(adminRequest.startModelDownload('acme/tiny'), {
    method: 'POST', path: 'api/admin/models/download', body: { repo: 'acme/tiny' },
  });
  assert.deepEqual(adminRequest.startModelDownload('acme/tiny', 'v1').body, { repo: 'acme/tiny', modelRevision: 'v1' });
  assert.deepEqual(adminRequest.modelDownload(), { method: 'GET', path: 'api/admin/models/download' });
  assert.deepEqual(adminRequest.cancelModelDownload(), { method: 'POST', path: 'api/admin/models/download/cancel' });
});

test('the form: the same refusals as the server, before anything is sent', () => {
  assert.deepEqual(downloadRequest({ repo: ' acme/tiny ', revision: '' }), { repo: 'acme/tiny', revision: null, problems: [] });
  assert.equal(downloadRequest({ repo: 'acme/tiny', revision: 'refs/pr/1' }).revision, 'refs/pr/1');
  assert.match(downloadRequest({ repo: '' }).problems[0], /Enter the repository/);
  for (const repo of ['tiny', 'a/b/c', '.x/y', 'a b/c']) {
    const r = downloadRequest({ repo });
    assert.equal(r.repo, null, repo);
    assert.match(r.problems[0], /not a Hugging Face repository name/, repo);
  }
  assert.match(downloadRequest({ repo: 'acme/tiny', revision: '../x' }).problems[0], /not a revision/);
  const rust = readFileSync(new URL('../../src/ops_models.rs', import.meta.url), 'utf8');
  assert.ok(rust.includes('is not a Hugging Face repository name: owner/name of letters'));
});

const running = {
  id: 3, repo: 'acme/tiny', revision: 'main', state: 'running', phase: 'download', cancelRequested: false,
  file: { name: 'model.safetensors', bytes: 512 * 1024, total: 1024 * 1024 }, filesDone: [{ name: 'config.json', bytes: 20 }],
  receivedBytes: 512 * 1024 + 20, report: null, added: null, error: null,
};

test('a running download: the file, its percent, the bytes', () => {
  const v = downloadView(running);
  assert.equal(v.running, true);
  assert.equal(v.headline, 'Downloading acme/tiny (main)…');
  assert.equal(v.file, 'model.safetensors');
  assert.equal(v.percent, 50);
  assert.equal(v.progress, 'model.safetensors: 512 KB of 1.0 MB (50 %)');
  assert.equal(downloadView({ ...running, file: { name: 'x', bytes: 5, total: null } }).percent, null, 'size unknown: indeterminate');
  assert.match(downloadView({ ...running, cancelRequested: true }).headline, /^Cancelling/);
  assert.match(downloadView({ ...running, phase: 'install', file: null }).headline, /^Adding acme\/tiny/);
  assert.equal(downloadNotice(running), null, 'no toast while it runs');
  assert.equal(downloadView(null), null);
  assert.equal(downloadView({}), null);
});

test('a finished download: the licence, its warnings and base models; failures and cancels in words', () => {
  const done = {
    ...running, state: 'done', phase: 'install', file: null,
    report: { identity: 'model2vec/acme/tiny@0123', license: 'cc-by-nc-4.0', gated: true, warnings: ['non-commercial use only'], baseModels: [{ repo: 'acme/base', license: 'mit' }, { repo: 'acme/other' }] },
  };
  const v = downloadView(done);
  assert.equal(v.headline, 'Downloaded acme/tiny (main) into the shared models.');
  assert.equal(v.license, 'cc-by-nc-4.0');
  assert.equal(v.gated, true);
  assert.deepEqual(v.warnings, ['non-commercial use only']);
  assert.deepEqual(v.baseModels, ['acme/base (mit)', 'acme/other']);
  assert.equal(v.tone, 'warn', 'a licence warning makes it a warning');
  assert.deepEqual(downloadNotice(done), {
    tone: 'warn', text: 'Downloaded acme/tiny (main) into the shared models.', help: 'Licence: cc-by-nc-4.0. non-commercial use only',
  });
  assert.equal(downloadView({ ...done, report: { license: 'mit', warnings: [] } }).tone, 'ok');
  const failed = { ...running, state: 'failed', file: null, error: 'acme/tiny has no model.safetensors', errorKind: 'not-found' };
  assert.equal(downloadView(failed).headline, 'The download of acme/tiny (main) failed; nothing was kept.');
  assert.deepEqual(downloadNotice(failed), { tone: 'error', text: downloadView(failed).headline, help: 'acme/tiny has no model.safetensors' });
  const cancelled = { ...failed, state: 'cancelled', error: 'cancelled' };
  assert.match(downloadView(cancelled).headline, /was cancelled; nothing was kept/);
  assert.equal(downloadNotice(cancelled).tone, 'warn');
});

test('the Models tab: the form, progress with Cancel, the poll, Download… for a waiting database', () => {
  const tab = readFileSync(new URL('../models-tab.js', import.meta.url), 'utf8');
  assert.ok(tab.includes('data-model-download'));
  assert.ok(tab.includes('@submit.prevent="startDownload"'));
  assert.ok(tab.includes('data-download-cancel'));
  assert.ok(tab.includes('@click="cancelDownload"'));
  assert.ok(tab.includes('setTimeout(readDownload, DOWNLOAD_POLL_MS)'));
  assert.ok(tab.includes('alive && fresh.active'), 'the poll runs only while the tab is shown');
  assert.ok(tab.includes('@click="prefillDownload(m.model)"'));
  assert.ok(tab.includes('Licence:'));
});
