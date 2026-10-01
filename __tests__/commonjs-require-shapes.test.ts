import { it, expect } from 'vitest';
import { extractFromSource } from '../src/extraction';
import { commonJsRequireRefs } from '../src/extraction/commonjs-requires';

it('imports real literal requires and refuses quoted examples, packages, properties and local shadows', () => {
  const source = `const text = "require('./ghost-string')";
const template = \`require('./ghost-template')\`;
// require('./ghost-comment')
function local(require) { require('./ghost-local'); }
function scoped() { const require = () => 1; require('./ghost-scoped'); }
other.require('./ghost-property');
require.resolve('./ghost-resolve');
require('express');
require('./real');
require(\`./literal\`);
require(\`./dynamic-\${text}\`);
`;
  const result = extractFromSource('entry.js', source);
  const refs = result.unresolvedReferences.filter((ref) => ref.referenceKind === 'imports' && ref.fromNodeId === 'file:entry.js');
  expect(refs.map((ref) => ref.referenceName)).toEqual(['./real', './literal']);
});

it('keeps a global require outside a same-line local scope', () => {
  const source = "function scoped(require) { require('./local'); } require('./global');";
  const result = extractFromSource('entry.js', source);
  expect(result.unresolvedReferences.filter((ref) => ref.referenceKind === 'imports' && ref.fromNodeId === 'file:entry.js').map((ref) => ref.referenceName)).toEqual(['./global']);
});


it('does not treat a TypeScript import-equals binding named require as the module loader', () => {
  const result = extractFromSource('entry.ts', "import require = require('./helper'); require('./ghost');");
  expect(result.unresolvedReferences.filter((ref) => ref.referenceKind === 'imports' && ref.fromNodeId === 'file:entry.ts').map((ref) => ref.referenceName)).toEqual(['./helper']);
});


it('does not mistake method names or static-block vars for module-wide require bindings', () => {
  const source = "class Loader { require() { return require('./class-real'); } static { var require = other; require('./local'); } } const object = { require() { return require('./object-real'); } }; require('./global');";
  const result = extractFromSource('entry.js', source);
  expect(result.unresolvedReferences.filter((ref) => ref.referenceKind === 'imports' && ref.fromNodeId === 'file:entry.js').map((ref) => ref.referenceName)).toEqual(['./class-real', './object-real', './global']);
});


it('cooks escaped module literals and retains configurable path prefixes for native alias resolution', () => {
  const source = String.raw`require('./re\u0061l'); require('@lib/helper'); require('tools/helper'); require('external');`;
  const result = extractFromSource('entry.js', source);
  expect(result.unresolvedReferences.filter((ref) => ref.referenceKind === 'imports' && ref.fromNodeId === 'file:entry.js').map((ref) => ref.referenceName)).toEqual(['./real', '@lib/helper', 'tools/helper']);
});


it('distinguishes loop bindings, parameter defaults, type-only imports and identity escapes', () => {
  const cases: Array<[string, string[]]> = [
    ["for (const require of loaders) require('./ghost'); require('./real');", ['./real']],
    ["for (const {require} of loaders) require('./ghost'); require('./real');", ['./real']],
    ["function f(a=require('./real')) { var require; require('./ghost'); }", ['./real']],
    ["import type { require } from './types'; require('./real');", ['./real']],
    ["import { type require } from './types'; require('./real');", ['./real']],
    [String.raw`require('./re\U0061l'); require('./re\X61l');`, ['./reU0061l', './reX61l']],
  ];
  for (const [source, expected] of cases) {
    expect(commonJsRequireRefs('entry.ts', source, 'typescript').map((ref) => ref.referenceName), source).toEqual(expected);
  }
});
