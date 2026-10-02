import assert from 'node:assert/strict';
import test from 'node:test';
import { mergeCatalog } from './stdlib-catalog.mjs';

const fn = {
  name: 'max', type_params: ['T extends Numeric'],
  params: [{ name: 'a', type: 'T' }, { name: 'b', type: 'T' }],
  returns: { type: 'T' }, implementation: 'source',
};
const api = [{
  module: 'std.math', functions: [fn],
  errors: [{ name: 'MathError', variants: ['Overflow', 'DivisionByZero'] }],
}];

test('new modules and exports are published without editorial entries', () => {
  const [module] = mergeCatalog(api, []);
  assert.equal(module.id, 'math');
  assert.equal(module.import_example, 'use std.math;\n');
  assert.deepEqual(module.functions[0].type_params, ['T extends Numeric']);
  assert.deepEqual(module.functions[0].params.map(({ name, type }) => ({ name, type })), fn.params);
  assert.deepEqual(module.errors[0].variants, api[0].errors[0].variants);
  assert.ok(module.functions[0].detail);
});

test('prose cannot override signatures or error variants', () => {
  const [module] = mergeCatalog(api, [{
    module: 'std.math', id: 'math', functions: [{
      name: 'max', type_params: ['Wrong'], detail: 'Finds the larger value.',
      params: [{ name: 'a', type: 'Wrong', detail: 'First value.' }],
      returns: { type: 'Wrong', detail: 'Larger value.' },
    }], errors: [{ name: 'MathError', variants: ['Wrong'], detail: 'Checked failure.' }],
  }]);
  assert.deepEqual(module.functions[0].type_params, fn.type_params);
  assert.equal(module.functions[0].params[0].type, 'T');
  assert.equal(module.functions[0].params[0].detail, 'First value.');
  assert.deepEqual(module.functions[0].returns, { type: 'T', detail: 'Larger value.' });
  assert.deepEqual(module.errors[0].variants, api[0].errors[0].variants);
});

test('removed exports and renamed parameters reject stale prose', () => {
  assert.throws(() => mergeCatalog(api, [{ module: 'std.old' }]), /Stale stdlib prose/);
  for (const kind of ['functions', 'errors']) {
    assert.throws(() => mergeCatalog(api, [{ module: 'std.math', [kind]: [{ name: 'removed' }] }]), /Stale stdlib prose/);
  }
  assert.throws(() => mergeCatalog(api, [{ module: 'std.math', functions: [{
    name: 'max', params: [{ name: 'renamed', detail: 'Outdated.' }],
  }] }]), /Stale parameter prose/);
});

test('new exports appear alongside existing prose, and page IDs stay unique', () => {
  const added = { ...fn, name: 'min' };
  const [module] = mergeCatalog([{ ...api[0], functions: [fn, added] }], [{
    module: 'std.math', functions: [{ name: 'max', detail: 'Existing description.' }],
  }]);
  assert.deepEqual(module.functions.map((item) => item.name), ['max', 'min']);
  assert.equal(module.functions[0].detail, 'Existing description.');
  assert.throws(() => mergeCatalog([...api, { ...api[0], module: 'std.other.math' }], []), /Duplicate stdlib page IDs/);
});
