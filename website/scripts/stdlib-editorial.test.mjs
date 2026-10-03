import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadEditorial } from './stdlib-editorial.mjs';

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'vl-stdlib-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test('module documents are discovered in deterministic order, ignoring other files', (t) => {
  const directory = fixture(t);
  writeFileSync(join(directory, 'z.yaml'), 'module: std.z\n');
  writeFileSync(join(directory, 'a.yml'), 'module: std.a\n');
  writeFileSync(join(directory, 'README.md'), 'Editorial documentation.');
  assert.deepEqual(loadEditorial(directory), [{ module: 'std.a' }, { module: 'std.z' }]);
});

test('invalid documents and duplicate modules fail with actionable errors', (t) => {
  const directory = fixture(t);
  writeFileSync(join(directory, 'a.yaml'), '- module: std.a\n');
  assert.throws(() => loadEditorial(directory), /Invalid stdlib prose in a.yaml/);
  writeFileSync(join(directory, 'a.yaml'), 'module: std.a\n');
  writeFileSync(join(directory, 'b.yaml'), 'module: std.a\n');
  assert.throws(() => loadEditorial(directory), /Duplicate stdlib prose: module std.a/);
});
