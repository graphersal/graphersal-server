// Node tests of the MCP exposure of new names (round 4 B-MCPDEF): the server-wide default
// (`exposeNew` of `GET/PUT api/admin/mcp`, the MCP tab's checkbox "Expose new databases and
// aliases") and the "Expose through MCP" checkbox of the create, clone, new alias and import
// forms, which sends `mcp: true` (an upload: `mcp=true`) and nothing otherwise (the server's
// default applies); the outcome toast says whether the new name is offered.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { mcpRequest, exposeNewOf, EXPOSE_NEW_HELP } from '../mcp-api.js';
import {
  adminRequest, createBody, importRequest, outcomeText, withMcp, MCP_NEW_HELP,
} from '../admin-api.js';

const src = (file) => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8');

test('the server-wide default: its request with the revision, its value, its help', () => {
  assert.deepEqual(mcpRequest.setExposeNew(true, 'r1'), {
    method: 'PUT', path: 'api/admin/mcp', body: { exposeNew: true }, ifMatch: 'r1',
  });
  assert.deepEqual(mcpRequest.setExposeNew(0), { method: 'PUT', path: 'api/admin/mcp', body: { exposeNew: false } });
  assert.equal(exposeNewOf({ exposeNew: true }), true);
  assert.equal(exposeNewOf({ exposeNew: false }), false);
  assert.equal(exposeNewOf({}), false, 'missing (an older server): off');
  assert.match(EXPOSE_NEW_HELP, /keep their settings/);
  const tab = src('mcp-tab.js');
  assert.ok(tab.includes('data-mcp-expose-new'), 'the checkbox');
  assert.ok(tab.includes('Expose new databases and aliases'));
  assert.ok(tab.includes('mcp.setExposeNew(on, s.revision)'), 'sent with the revision');
});

test('a new name is offered to MCP only when the form asks; otherwise nothing is sent', () => {
  assert.deepEqual(withMcp({ name: 'a' }, true), { name: 'a', mcp: true });
  assert.deepEqual(withMcp({ name: 'a' }, false), { name: 'a' });
  assert.deepEqual(withMcp({ name: 'a' }), { name: 'a' });
  assert.deepEqual(createBody({ name: 'acme', kind: 'plain', mcp: true }).body, { name: 'acme', mcp: true });
  assert.deepEqual(createBody({ name: 'acme', kind: 'plain', mcp: false }).body, { name: 'acme' });
  assert.deepEqual(adminRequest.clone('a', 'b', true).body, { name: 'b', mcp: true });
  assert.deepEqual(adminRequest.clone('a', 'b').body, { name: 'b' });
  assert.deepEqual(adminRequest.setAlias('cur', 'a', 'rev', true), {
    method: 'PUT', path: 'api/admin/aliases/cur', body: { target: 'a', mcp: true }, ifMatch: 'rev',
  });
  assert.deepEqual(adminRequest.setAlias('cur', 'a').body, { target: 'a' });
  const upload = importRequest({ source: 'upload', file: { size: 3 }, name: 'gs', mcp: true });
  assert.match(upload.upload.path, /[?&]mcp=true(&|$)/);
  assert.doesNotMatch(importRequest({ source: 'upload', file: { size: 3 }, name: 'gs' }).upload.path, /mcp=/);
  assert.deepEqual(importRequest({ source: 'path', path: '/d/a.gsnap', name: 'gs', mcp: true }).request.body, {
    path: '/d/a.gsnap', name: 'gs', mcp: true,
  });
  assert.match(MCP_NEW_HELP, /hidden from MCP until it is exposed on the MCP tab/);
});

test('the outcome says whether the new name is offered to MCP', () => {
  assert.equal(outcomeText('create', 'acme', { mcpExposed: false }),
    'Created "acme" (unloaded). Not offered to MCP (expose it on the MCP tab).');
  assert.equal(outcomeText('clone', 'b', { mcpExposed: true }), 'Cloned into "b" (unloaded). Offered to MCP.');
  assert.equal(outcomeText('create', 'acme', {}), 'Created "acme" (unloaded).', 'an answer without it: no text');
  assert.ok(outcomeText('setAlias', 'cur', { alias: 'cur', target: 'a', mcpExposed: false }).endsWith('Not offered to MCP (expose it on the MCP tab).'));
});

test('the forms carry the checkbox: create, clone, a NEW alias (not a flip), import', () => {
  const dbs = src('databases-tab.js');
  for (const id of ['data-create-mcp', 'data-clone-mcp', 'data-alias-mcp']) assert.ok(dbs.includes(id), id);
  assert.match(dbs, /<label v-if="!dialog\.existing"[^>]*><input type="checkbox" name="mcp" v-model="dialog\.mcp" data-alias-mcp>/,
    'a flip keeps the alias\'s settings: no checkbox');
  assert.ok(dbs.includes('admin.clone(source, subject, mcp)'));
  assert.ok(dbs.includes('admin.setAlias(subject, target, revision, mcp)'));
  assert.ok(src('import-tab.js').includes('data-import-mcp'));
});
