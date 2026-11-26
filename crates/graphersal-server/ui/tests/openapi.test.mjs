// The OpenAPI links of the `server` profile (B-OPENAPI): "My databases" links each database's
// description (`db/<name>/openapi.json`), the admin's Databases tab each row's and the server's
// own (`openapi.json`), all relative to the server root (a proxy prefix keeps working); the
// paths are the ones the router answers (src/router.rs, /api/info `openapi` and
// `databaseOpenapi`, the fixture root-info.json).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { mineRows } from '../access-api.js';
import { databaseOpenapiHref, databaseHref, SERVER_OPENAPI_HREF } from '../databases.js';

const read = (file) => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');

test('the description paths are the ones /api/info names', () => {
  const info = JSON.parse(read('tests/root-info.json'));
  assert.equal(`/${SERVER_OPENAPI_HREF}`, info.openapi);
  assert.equal(`/${databaseOpenapiHref('acme')}`, info.databaseOpenapi.replace('{name}', 'acme'));
  assert.equal(databaseOpenapiHref('a b'), `${databaseHref('a b')}openapi.json`);
  assert.equal(databaseOpenapiHref('a b'), 'db/a%20b/openapi.json');
  // Relative to the root: under a proxy prefix the link keeps it.
  assert.equal(new URL(databaseOpenapiHref('acme'), 'https://h.example/graph/').href,
    'https://h.example/graph/db/acme/openapi.json');
});

test('my databases: every row links its description, loaded or not, an alias by its own name', () => {
  const rows = mineRows({ databases: [
    { name: 'acme', state: 'loaded', level: 'write' },
    { name: 'cur', state: 'unloaded', level: 'read', kind: 'alias', aliasOf: 'acme' },
  ] });
  assert.deepEqual(rows.map((r) => r.openapiHref), ['db/acme/openapi.json', 'db/cur/openapi.json']);
  const tab = read('mine-tab.js');
  assert.ok(tab.includes(':href="apiHref(row)"') && tab.includes('data-openapi'), 'the tab shows the link');
  assert.ok(/<a :href="apiHref\(row\)"[^>]*>OpenAPI<\/a>/.test(tab));
  assert.ok(!/v-if="row.open"[^>]*data-openapi/.test(tab), 'not only for loaded rows');
  assert.match(tab, /Bearer/);
});

test('the admin Databases tab links the server description and each row\'s', () => {
  const tab = read('databases-tab.js');
  assert.ok(tab.includes(':href="serverApiHref"') && tab.includes('data-server-openapi'));
  assert.ok(/<a :href="apiHref\(row\)"[^>]*data-openapi[^>]*>OpenAPI<\/a>/.test(tab));
  const shared = read('shared.js');
  assert.match(shared, /export const openapiHref = \(row\) => new URL\(databaseOpenapiHref\(row\.name\), ROOT\)\.href/);
  assert.match(shared, /export const serverOpenapiHref = \(\) => new URL\(SERVER_OPENAPI_HREF, ROOT\)\.href/);
});
