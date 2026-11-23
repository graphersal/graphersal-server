// Node tests of the run administration in the admin panel's model (ui/admin-api.js, V25): the
// requests, the rows, the texts and the Stop rule, against a stateful mock (node:http on port 0)
// shaped after docs/admin-api.md "Runs".
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {
  adminRequest, createAdmin, AdminError, runRows, durationText, runTextHead, cancelRunAction,
  confirmText, outcomeText, RUNS_POLL_MS,
} from '../admin-api.js';

const LONG = `g.v().repeat(__.both()).times(${'9'.repeat(200)})`.slice(0, 200);

/** A mock of the run routes: database `acme` loaded with two runs, `idle` loaded with none,
 * `cold` unloaded. Ids are per database (both `acme` and `other` have a run 1). */
async function mockServer() {
  const runs = {
    acme: [
      { database: 'acme', id: 1, client: 'page:a', alias: 'r1', kind: 'query', state: 'running', cancelled: false, startedAtMs: 1700000000000, durationMs: 12345, text: 'let n = 0; loop { n += 1; }' },
      { database: 'acme', id: 2, client: 'mcp:s1', alias: null, kind: 'tool', state: 'queued', cancelled: false, startedAtMs: 1700000001000, durationMs: 20, text: LONG },
    ],
    idle: [],
    other: [{ database: 'other', id: 1, client: 'page:b', alias: null, kind: 'query', state: 'running', cancelled: false, startedAtMs: 1700000002000, durationMs: 5, text: null }],
  };
  const loaded = new Set(['acme', 'idle', 'other']);
  const known = new Set([...loaded, 'cold']);
  const log = [];
  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks).toString();
    const url = new URL(req.url, 'http://x');
    log.push({ method: req.method, path: url.pathname, body: raw });
    const send = (status, body) => {
      res.writeHead(status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(body));
    };
    const path = decodeURIComponent(url.pathname).replace(/^\/api\/admin\//, '');
    const parts = path.split('/');
    if (path === 'runs' && req.method === 'GET') {
      const all = [...loaded].sort().flatMap((name) => runs[name] || []);
      return send(200, { runs: all, textHeadChars: 200 });
    }
    if (parts[0] === 'databases' && parts[2] === 'runs') {
      const name = parts[1];
      if (!known.has(name)) return send(404, { error: `no database '${name}'`, help: 'GET /api/admin/databases lists the databases and aliases' });
      if (parts.length === 3 && req.method === 'GET') {
        return send(200, { database: name, loaded: loaded.has(name), runs: runs[name] || [], textHeadChars: 200 });
      }
      if (parts[3] === 'cancel' && req.method === 'POST') {
        let body;
        try {
          body = JSON.parse(raw);
        } catch {
          return send(400, { error: 'the body is not JSON' });
        }
        if (Object.keys(body).some((k) => k !== 'id') || !Number.isInteger(body.id)) return send(400, { error: 'invalid body' });
        const run = (runs[name] || []).find((r) => r.id === body.id && !r.cancelled);
        if (run) run.cancelled = true;
        return send(200, { database: name, id: body.id, cancelled: !!run });
      }
      return send(405, { error: `method ${req.method} is not allowed` });
    }
    return send(404, { error: 'unknown admin endpoint' });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}/`;
  return { base, log, runs, close: () => new Promise((resolve) => server.close(resolve)) };
}

test('the run requests: all loaded databases, one database, a cancel by id', () => {
  assert.deepEqual(adminRequest.runs(), { method: 'GET', path: 'api/admin/runs' });
  assert.deepEqual(adminRequest.runs(''), { method: 'GET', path: 'api/admin/runs' });
  assert.deepEqual(adminRequest.runs('acme'), { method: 'GET', path: 'api/admin/databases/acme/runs' });
  assert.deepEqual(adminRequest.runs('a b'), { method: 'GET', path: 'api/admin/databases/a%20b/runs' });
  assert.deepEqual(adminRequest.cancelRun('acme', 7), {
    method: 'POST', path: 'api/admin/databases/acme/runs/cancel', body: { id: 7 },
  });
  assert.equal(RUNS_POLL_MS, 2000);
});

test('list and stop runs against the mock: ids are per database', async () => {
  const mock = await mockServer();
  try {
    const admin = createAdmin({ base: mock.base, fetch: globalThis.fetch });
    const all = runRows(await admin.runs());
    assert.deepEqual(all.map((r) => `${r.database}/${r.id}`), ['acme/1', 'acme/2', 'other/1']);
    const one = await admin.runs('acme');
    assert.equal(one.loaded, true);
    assert.deepEqual(runRows(one).map((r) => r.id), [1, 2]);
    const cold = await admin.runs('cold');
    assert.equal(cold.loaded, false);
    assert.deepEqual(runRows(cold), []);
    await assert.rejects(admin.runs('none'), (err) => err instanceof AdminError && err.status === 404 && /databases/.test(err.help));

    const stopped = await admin.cancelRun('acme', 1);
    assert.deepEqual(stopped, { database: 'acme', id: 1, cancelled: true });
    assert.equal(mock.log.at(-1).body, JSON.stringify({ id: 1 }), 'only the documented field');
    assert.equal(mock.runs.other[0].cancelled, false, 'run 1 of another database is untouched');
    const again = await admin.cancelRun('acme', 1);
    assert.equal(again.cancelled, false);
    assert.equal(outcomeText('cancelRun', 'acme', stopped), 'Run 1 of "acme" is stopping.');
    assert.equal(outcomeText('cancelRun', 'acme', again), 'Run 1 of "acme" had already ended.');
    const after = runRows(await admin.runs('acme'));
    assert.equal(after[0].cancelled, true);
    assert.deepEqual(cancelRunAction(after[0]), { enabled: false, title: 'Already stopping: it ends at its next check' });
  } finally {
    await mock.close();
  }
});

test('run rows: fields kept, defaults for missing ones, malformed rows skipped', () => {
  const rows = runRows({
    runs: [
      { database: 'acme', id: 3, client: 'page:x', alias: null, kind: 'query', state: 'running', cancelled: false, startedAtMs: 1, durationMs: 2, text: null },
      { database: 'acme', id: '4' },
      { id: 5 },
      null,
      { database: 'b', id: 6 },
    ],
  });
  assert.deepEqual(rows, [
    { database: 'acme', id: 3, client: 'page:x', alias: '', kind: 'query', state: 'running', cancelled: false, startedAtMs: 1, durationMs: 2, text: '' },
    { database: 'b', id: 6, client: '', alias: '', kind: '', state: '', cancelled: false, startedAtMs: null, durationMs: null, text: '' },
  ]);
  assert.deepEqual(runRows(null), []);
  assert.deepEqual(runRows({ runs: 'x' }), []);
});

test('durations, query heads, the Stop rule and its confirmation', () => {
  assert.equal(durationText(850), '850 ms');
  assert.equal(durationText(12345), '12.3 s');
  assert.equal(durationText(245000), '4 min 5 s');
  assert.equal(durationText(7380000), '2 h 3 min');
  assert.equal(durationText(null), '');
  assert.equal(durationText(-1), '');

  assert.equal(runTextHead({ text: 'g.v()\n  .count()' }), 'g.v() .count()');
  assert.equal(runTextHead({ text: LONG }), `${LONG}…`, 'cut by the server at 200 characters');
  assert.equal(runTextHead({ text: 'abc' }, 3), 'abc…');
  assert.equal(runTextHead({ text: '' }), '');
  assert.equal(runTextHead({}), '');

  assert.equal(cancelRunAction({ cancelled: false }).enabled, true);
  assert.equal(cancelRunAction({ cancelled: false }, { busy: true }).enabled, false);
  assert.match(confirmText('cancelRun', 'acme', '7'), /Stop run 7 of "acme"\?.*roll back/);
});
