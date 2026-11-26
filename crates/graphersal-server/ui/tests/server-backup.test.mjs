// Node tests of the server backup download (ui/admin-api.js `serverBackupPath`,
// `fileNameOf`, `fetchServerBackup`, `SERVER_BACKUP_WARNING`) against a mock of
// `GET /api/admin/server-backup` (node:http on port 0, shaped after book/src/reference/admin_api.md), and the
// Backups tab's "Download server backup" button with its warning (ui/backups-tab.js).
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { readFileSync } from 'node:fs';
import {
  serverBackupPath, fileNameOf, fetchServerBackup, SERVER_BACKUP_WARNING, AdminError,
} from '../admin-api.js';

/** A mock: admins (`Bearer t`) get a ZIP, a user (`Bearer u`) the unknown-endpoint 404, nobody 401. */
async function mockServer() {
  const log = [];
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, query: url.search });
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    if (url.pathname !== '/api/admin/server-backup') return send(404, { error: 'unknown endpoint' });
    if (!req.headers.authorization) return send(401, { error: 'log in to use the admin API', login: '/' });
    if (req.headers.authorization !== 'Bearer t') return send(404, { error: 'unknown endpoint /api/admin/server-backup' });
    const full = url.searchParams.get('withDatabases') === '1';
    res.writeHead(200, {
      'Content-Type': 'application/octet-stream',
      'Content-Disposition': `attachment; filename="srv-server-backup-2026-11-26T10-00-00Z.zip"`,
      'X-Graphersal-Server-Backup': full ? 'with-databases' : 'config',
      'X-Graphersal-Skipped': full ? 'broken' : '',
    });
    res.end(Buffer.from('PK\x03\x04 a zip', 'latin1'));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  return { base: `http://127.0.0.1:${port}/`, log, close: () => new Promise((r) => server.close(r)) };
}

test('the path asks for the databases only when wanted; the file name comes from the answer', () => {
  assert.deepEqual(serverBackupPath(), { path: 'api/admin/server-backup', fileName: 'server-backup.zip' });
  assert.deepEqual(serverBackupPath({ withDatabases: true }), {
    path: 'api/admin/server-backup?withDatabases=1', fileName: 'server-backup-full.zip',
  });
  assert.equal(fileNameOf('attachment; filename="a-server-backup-x.zip"', 'f.zip'), 'a-server-backup-x.zip');
  assert.equal(fileNameOf('', 'f.zip'), 'f.zip');
  assert.equal(fileNameOf('attachment; filename="../evil.zip"', 'f.zip'), 'f.zip', 'no path in a name');
});

test('the warning says the archive is a secret', () => {
  assert.match(SERVER_BACKUP_WARNING, /token hashes/);
  assert.match(SERVER_BACKUP_WARNING, /password/);
});

test('an admin downloads the archive; others get the server\'s refusal', async () => {
  const mock = await mockServer();
  try {
    const got = await fetchServerBackup({ base: mock.base, headers: { Authorization: 'Bearer t' } });
    assert.equal(got.fileName, 'srv-server-backup-2026-11-26T10-00-00Z.zip');
    assert.deepEqual(got.skipped, []);
    assert.equal((await got.blob.text()).slice(0, 2), 'PK');
    const full = await fetchServerBackup({ base: mock.base, headers: { Authorization: 'Bearer t' }, withDatabases: true });
    assert.deepEqual(full.skipped, ['broken']);
    assert.equal(mock.log.at(-1).query, '?withDatabases=1');
    await assert.rejects(
      fetchServerBackup({ base: mock.base, headers: { Authorization: 'Bearer u' } }),
      (err) => err instanceof AdminError && err.status === 404,
    );
    await assert.rejects(
      fetchServerBackup({ base: mock.base }),
      (err) => err instanceof AdminError && err.status === 401,
    );
    await assert.rejects(
      fetchServerBackup({ base: 'http://127.0.0.1:1/' }),
      (err) => err instanceof AdminError && /could not be reached/.test(err.message),
    );
  } finally {
    await mock.close();
  }
});

test('the Backups tab offers "Download server backup" with the warning, as a link or a fetch', () => {
  const source = readFileSync(new URL('../backups-tab.js', import.meta.url), 'utf8');
  assert.match(source, /Download server backup/);
  assert.match(source, /serverBackupWarning/);
  assert.match(source, /SERVER_BACKUP_WARNING/);
  assert.match(source, /serverBackupPath\(\{ withDatabases: sb\.full \}\)/);
  assert.match(source, /fetchServerBackup\(/);
  assert.match(source, /name="withDatabases"/);
  // The link form streams the file (no Blob in memory); the fetch form carries the host headers.
  assert.match(source, /<a v-if="!withHeaders" class="btn" :href="serverBackupHref" download/);
});
