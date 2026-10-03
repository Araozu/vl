import assert from 'node:assert/strict';
import test from 'node:test';
import { createTypeLinks, renderType, renderSignature, signature } from '../src/lib/stdlib-links.mjs';

const links = createTypeLinks([
  { id: 'strings', module: 'std.string', type_names: ['String'], errors: [{ name: 'StringError' }] },
  { id: 'array', module: 'std.array', type_names: ['Array'] },
  { id: 'tcp', module: 'std.net.tcp', errors: [{ name: 'TcpError' }] },
]);

test('compound types link each documented identifier and preserve syntax', () => {
  assert.equal(renderType('TcpError!#(*Array[String], bool)', 'std.net.tcp', links),
    '<a href="/std/tcp#stdnettcptcperror">TcpError</a>!#(*<a href="/std/array">Array</a>[<a href="/std/strings">String</a>], <a href="/docs/basics#values-and-types">bool</a>)');
  assert.equal(renderType('std.string.StringError!String', 'std.net.tcp', links),
    '<a href="/std/strings#stdstringstringerror">std.string.StringError</a>!<a href="/std/strings">String</a>');
  assert.equal(renderType('Unknown[StringError]', 'std.net.tcp', links), 'Unknown[StringError]');
});

test('only type positions link, and generic names shadow documented types', () => {
  const fn = {
    name: 'String', type_params: ['String extends Comparable'],
    params: [{ name: 'Array', type: 'Array[String]' }], returns: { type: 'String' },
  };
  const html = renderSignature('std.string', fn, links);
  assert.equal(html,
    'std.string.String[String extends <a href="/docs/arrays#constrained-generics">Comparable</a>](Array: <a href="/std/array">Array</a>[String]) -&gt; String');
  assert.equal(html.replace(/<[^>]+>/g, '').replace(/&gt;/g, '>'), signature('std.string', fn));
});

test('unknown types are escaped and duplicate owners are rejected', () => {
  assert.equal(renderType('<Unknown & "type">', 'std', links), '&lt;Unknown &amp; &quot;type&quot;&gt;');
  assert.throws(() => createTypeLinks([
    { id: 'a', module: 'std.a', type_names: ['String'] },
    { id: 'b', module: 'std.b', type_names: ['String'] },
  ]), /Duplicate stdlib type link/);
});
