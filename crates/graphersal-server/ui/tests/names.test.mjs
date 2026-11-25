// Node tests of the name rule the web UI shares with the server (ui/admin-api.js
// `newNameReason`, `existingNameReason`, `nameProblem`, `existingNameProblem`, `userNameProblem`,
// `suggestName`, `restoredName`; the server's src/names.rs): the same checks in the same order
// with the same texts, the rule of NEW names (0.3.0: at most 32 characters, a letter or digit
// last, no '..') and the older rule names made before keep.
// Run: node --test crates/graphersal-server/ui/tests/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  NAME_RULE, NAME_MAX, NAME_HINT, RESERVED_NAMES, newNameReason, existingNameReason, nameProblem,
  existingNameProblem, userNameProblem, suggestName, restoredName, createBody, importRequest,
} from '../admin-api.js';

const SERVER_NAMES = readFileSync(new URL('../../src/names.rs', import.meta.url), 'utf8');

/** A name breaking each check of a NEW name, with the server's reason. */
const NEW_REFUSALS = [
  ['', 'a name cannot be empty'],
  ['x'.repeat(33), 'a name has at most 32 characters'],
  ['-acme', 'a name starts with a lower-case letter or a digit'],
  ['a b', "a name holds only lower-case letters, digits, '.', '_' and '-'"],
  ['acme-', 'a name ends with a lower-case letter or a digit'],
  ['acme_', 'a name ends with a lower-case letter or a digit'],
  ['acme.', 'a name ends with a lower-case letter or a digit'],
  ['a..b', "a name cannot hold '..'"],
  ['con', 'the name is reserved for a device on Windows'],
  ['nul.db', 'the name is reserved for a device on Windows'],
];

test('the rule of new names: every refusal with the server text, in the server order', () => {
  for (const [name, reason] of NEW_REFUSALS) {
    assert.equal(newNameReason(name), reason, JSON.stringify(name));
    assert.ok(SERVER_NAMES.includes(`"${reason}"`), `names.rs holds the text: ${reason}`);
  }
  // The order: length before the characters, the characters before the last character.
  assert.equal(newNameReason('-'.repeat(40)), 'a name has at most 32 characters');
  assert.equal(newNameReason('a-b c-'), "a name holds only lower-case letters, digits, '.', '_' and '-'");
  // The length counts bytes as the server does.
  assert.equal(newNameReason('é'.repeat(17)), 'a name has at most 32 characters');
  // The reserved database names; a user may take them.
  assert.deepEqual(RESERVED_NAMES, ['jobs']);
  const jobs = newNameReason('jobs');
  assert.match(jobs, /^the name is reserved: backup jobs write into backups\/jobs\/ by default/);
  assert.ok(SERVER_NAMES.includes("pick another name, such as 'jobs-db'"));
  assert.equal(newNameReason('jobs', { reserved: false }), null);
  for (const name of ['a', '0', 'acme', 'acme-2026.3', 'product_v2', 'a.b-c_d', 'con-1', 'jobs-db', 'x'.repeat(32)]) {
    assert.equal(newNameReason(name), null, name);
    assert.ok(NAME_RULE.test(name), name);
  }
  assert.equal(NAME_MAX, 32);
  assert.ok(SERVER_NAMES.includes('pub const MAX_NAME_LEN: usize = 32;'));
  assert.ok(!NAME_RULE.test('acme-') && !NAME_RULE.test('x'.repeat(33)));
  assert.match(NAME_HINT, /^Up to 32 characters: lower-case letters, digits/);
  assert.match(NAME_HINT, /starts and ends with a letter or digit\.$/);
});

test('names made before 0.3.0 keep the older rule', () => {
  for (const name of ['x'.repeat(40), 'x'.repeat(63), 'acme-', 'acme_', 'a..b', 'jobs']) {
    assert.equal(existingNameReason(name), null, name);
    assert.equal(existingNameProblem(name.toUpperCase()), null, name);
  }
  assert.equal(existingNameReason('x'.repeat(64)), 'a name has at most 63 characters');
  assert.equal(existingNameReason('acme.'), "a name cannot end with '.'");
  assert.equal(existingNameReason('-a'), 'a name starts with a lower-case letter or a digit');
  assert.equal(existingNameReason('con.db'), 'the name is reserved for a device on Windows');
  for (const reason of ['a name has at most 63 characters', "a name cannot end with '.'"]) {
    assert.ok(SERVER_NAMES.includes(`"${reason}"`), reason);
  }
  assert.equal(existingNameProblem('  '), 'Enter a name.');
});

test('the form checks show the server error text and are checked before sending', () => {
  assert.equal(nameProblem(' Acme- ', 'name for the clone'),
    'invalid name "acme-": a name ends with a lower-case letter or a digit');
  assert.equal(nameProblem('', 'new alias name'), 'Enter a new alias name.');
  assert.equal(nameProblem('A..B'), `invalid name "a..b": a name cannot hold '..'`);
  assert.equal(nameProblem('acme'), null);
  assert.deepEqual(createBody({ name: 'x'.repeat(33) }).problems,
    [`invalid name "${'x'.repeat(33)}": a name has at most 32 characters`]);
  const imported = importRequest({ source: 'path', path: '/data/a.gsnap', name: 'a_' });
  assert.deepEqual(imported.problems, ['invalid name "a_": a name ends with a lower-case letter or a digit']);
  // Users: the new rule without the reserved database names, then the reserved user names.
  assert.equal(userNameProblem('bob-'), 'invalid name "bob-": a name ends with a lower-case letter or a digit');
  assert.equal(userNameProblem('u'.repeat(33)), `invalid name "${'u'.repeat(33)}": a name has at most 32 characters`);
  assert.equal(userNameProblem('jobs'), null);
  assert.equal(userNameProblem('Anonymous'), "'anonymous': a reserved name (a grant subject or principal of its own)");
});

test('a suggested name is always a valid new name', () => {
  for (const file of [
    'a'.repeat(80) + '.gstore',
    'a'.repeat(31) + '-tail.gsnap',
    'my..data...set.json',
    'Report 2026 (final)_.graphml',
    '___.gsnap',
    'con.gsnap',
    'jobs.gsnap',
    'x'.repeat(31) + '.y.layer.gsnap',
  ]) {
    const name = suggestName(file);
    if (name) assert.equal(newNameReason(name), null, `${file} -> ${name}`);
  }
  assert.equal(suggestName('a'.repeat(31) + '-tail.gsnap'), 'a'.repeat(31), 'cut to 32, then the trailing - dropped');
  assert.equal(suggestName('my..data...set.json'), 'my.data.set');
  assert.equal(suggestName('Report 2026 (final)_.graphml'), 'report-2026-final');
  assert.equal(suggestName('con.gsnap'), '');
  assert.equal(suggestName('jobs.gsnap'), '');
  assert.equal(suggestName('acme.gsnap'), 'acme');
});

test('the default name of a restore into a new database is a valid new name', () => {
  assert.equal(restoredName('acme'), 'acme-restored');
  for (const old of ['a-database-name-of-forty-characters-0040', 'legacy-', 'two..dots', 'x'.repeat(63), '']) {
    const name = restoredName(old);
    assert.equal(newNameReason(name), null, `${old} -> ${name}`);
    assert.ok(name.endsWith('restored'), name);
  }
  assert.equal(restoredName('legacy-'), 'legacy-restored');
  assert.equal(restoredName('two..dots'), 'two.dots-restored');
});
