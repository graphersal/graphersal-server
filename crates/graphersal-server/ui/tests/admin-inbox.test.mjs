// Node tests of the Import tab's import inbox (ui/admin-api.js): `adminRequest.imports`, the
// listing model `inboxRows`, the inbox name rule `inboxNameProblem` and `importRequest` with
// `source: 'inbox'`, against a mock (node:http on port 0) shaped after docs/admin-api.md
// (`GET /api/admin/imports`, `POST /api/admin/import {"inbox", ...}`).
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { adminRequest, createAdmin, AdminError, importRequest, inboxRows, inboxNameProblem, suggestName, outcomeText } from '../admin-api.js';

const DIR = '/srv/graphersal/imports';
const LISTING = {
  dir: DIR,
  imports: [
    { name: 'people.json', kind: 'graphson', directory: false, bytes: 2048, modified: '2026-11-25T10:00:00Z' },
    { name: 'acme.gsnap', kind: 'snapshot', directory: false, bytes: 3 * 1024 * 1024, modified: '2026-11-24T09:00:00Z' },
    { name: 'store', kind: 'store', directory: true, bytes: 4096, modified: null },
    { name: 'photos', kind: null, directory: true, bytes: 10, modified: '2026-11-20T00:00:00Z' },
    { name: 'notes.txt', kind: null, directory: false, bytes: 0, modified: '2026-11-20T00:00:00Z' },
    { name: '../evil', kind: 'graphson', directory: false, bytes: 1 },
    { kind: 'graphson' },
  ],
};

/** The inbox routes: listing and import by inbox name (one entry: as the server's rule). */
async function mockServer() {
  const log = [];
  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks);
    const url = new URL(req.url, 'http://x');
    const body = raw.length ? JSON.parse(raw.toString()) : null;
    log.push({ method: req.method, path: url.pathname, body });
    const send = (status, answer) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(answer));
    };
    if (url.pathname === '/api/admin/imports' && req.method === 'GET') return send(200, LISTING);
    if (url.pathname === '/api/admin/import' && req.method === 'POST') {
      if (!!body.inbox === !!body.path) return send(400, { error: 'import: name the source with either "path" or "inbox", not both' });
      if (body.inbox.startsWith('.') || /[/\\]/.test(body.inbox)) return send(400, { error: `'imports/${body.inbox}': the name holds a path separator` });
      if (!LISTING.imports.some((e) => e.name === body.inbox)) {
        return send(404, { error: `the import source ${DIR}/${body.inbox} does not exist`, help: 'GET /api/admin/imports lists the import inbox' });
      }
      return send(201, { name: body.name, state: 'unloaded', kind: 'plain', role: 'normal', import: { format: 'graphson', modelsMissing: [], modelsResolved: [], skipped: 0, notes: [] } });
    }
    return send(404, { error: 'unknown endpoint' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { base, log, close: () => new Promise((resolve) => server.close(resolve)) };
}

test('the inbox request and its listing model', () => {
  assert.deepEqual(adminRequest.imports(), { method: 'GET', path: 'api/admin/imports' });
  const { dir, rows } = inboxRows(LISTING);
  assert.equal(dir, DIR);
  assert.deepEqual(rows.map((r) => r.name), ['acme.gsnap', 'notes.txt', 'people.json', 'photos', 'store'], 'sorted, malformed and non-entry names left out');
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by['acme.gsnap'].kindText, 'snapshot (.gsnap)');
  assert.equal(by['acme.gsnap'].sizeText, '3.0 MB');
  assert.equal(by['people.json'].kindText, 'GraphSON');
  assert.equal(by['notes.txt'].kindText, 'unknown (detected on import)');
  assert.equal(by['notes.txt'].sizeText, '0 B');
  assert.equal(by['notes.txt'].importable, true, 'the server detects the content');
  assert.equal(by.store.label, 'store/');
  assert.equal(by.store.kindText, 'Store');
  assert.equal(by.store.modified, '');
  assert.equal(by.photos.importable, false, 'a directory without a Store cannot be imported');
  assert.equal(by.photos.kindText, 'directory (no Store)');
  assert.deepEqual(inboxRows(null), { dir: '', rows: [] });
  assert.deepEqual(inboxRows({ imports: 'x' }), { dir: '', rows: [] });
  assert.equal(suggestName('acme.gsnap'), 'acme');
});

test('the inbox name rule and the import request from the inbox', () => {
  for (const bad of ['', '.', '..', '../x', 'a/b', 'a\\b', 'C:x', '.hidden']) {
    assert.ok(inboxNameProblem(bad), bad);
  }
  assert.equal(inboxNameProblem('people.json'), null);
  assert.deepEqual(importRequest({ source: 'inbox', inbox: 'people.json', name: ' People ', format: 'graphson', role: 'base', base: 'ignored' }), {
    request: { method: 'POST', path: 'api/admin/import', body: { inbox: 'people.json', name: 'people', format: 'graphson', role: 'base' } },
  });
  assert.deepEqual(importRequest({ source: 'inbox', inbox: 'x.gsnap', name: 'x', base: 'shared', description: ' d ' }).request.body,
    { inbox: 'x.gsnap', name: 'x', base: 'shared', description: 'd' });
  assert.deepEqual(importRequest({ source: 'inbox', inbox: '', name: 'x' }), { problems: ['Pick a file of the import inbox.'] });
  assert.deepEqual(importRequest({ source: 'inbox', inbox: '../server.toml', name: 'x' }), { problems: ['"../server.toml" is not a file of the import inbox.'] });
  const both = importRequest({ source: 'inbox', inbox: '', name: 'Bad Name' });
  assert.equal(both.problems.length, 2);
  // The path form and the upload are unchanged.
  assert.deepEqual(importRequest({ source: 'path', path: '/a.json', inbox: 'ignored.json', name: 'a' }).request.body, { path: '/a.json', name: 'a' });
});

test('the inbox is listed and a picked file imported through the admin client', async () => {
  const mock = await mockServer();
  try {
    const admin = createAdmin({ base: mock.base, fetch: globalThis.fetch });
    const { rows } = inboxRows(await admin.imports());
    assert.equal(rows.length, 5);
    const picked = rows.find((r) => r.name === 'people.json');
    const built = importRequest({ source: 'inbox', inbox: picked.name, name: suggestName(picked.name) });
    const answer = await admin.importPath(built.request.body);
    assert.equal(answer.name, 'people');
    assert.equal(outcomeText('import', 'people', answer), 'Imported "people" (graphson, plain): unloaded.');
    const sent = mock.log.find((r) => r.method === 'POST');
    assert.deepEqual(sent.body, { inbox: 'people.json', name: 'people' });
    // A file gone meanwhile: the server's error and help are shown as sent.
    await assert.rejects(admin.importPath({ inbox: 'gone.json', name: 'gone' }),
      (e) => e instanceof AdminError && e.status === 404 && /does not exist/.test(e.message) && /GET \/api\/admin\/imports/.test(e.help));
  } finally {
    await mock.close();
  }
});
