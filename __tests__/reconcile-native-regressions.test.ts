import { afterEach, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close(); graph = undefined;
  if (root) fs.rmSync(root, { recursive: true, force: true, maxRetries: 5 });
  root = undefined;
});
async function index(files: Record<string, string>) {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-reconcile-'));
  for (const [file, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), source);
  }
  graph = await CodeGraph.init(root, { index: true });
  return graph;
}

it('does not retain a Python field type after an opaque reassignment', async () => {
  const cg = await index({ 'main.py': `class Store:
    def fetch(self): pass
class App:
    def __init__(self, replacement):
        self.cache = Store()
        self.cache = replacement
    def run(self):
        self.cache.fetch()
` });
  const run = cg.getNodesByName('run')[0]!;
  expect(cg.getOutgoingEdgesFrom([run.id], ['calls'])).toEqual([]);
});

it('lets a lexical TypeScript type shadow an imported type', async () => {
  const cg = await index({
    'a.ts': 'export interface Foo { id: string }\n',
    'main.ts': `import { Foo } from './a';
export function local() {
  type Foo = string;
  let x: Foo;
}
export function imported(x: Foo) {}
`,
  });
  const imported = cg.getNodesByName('Foo').find(n => n.filePath === 'a.ts')!;
  const local = cg.getNodesByName('local')[0]!;
  expect(cg.getOutgoingEdgesFrom([local.id], ['references']).some(e => e.target === imported.id)).toBe(false);
  expect(cg.getOutgoingEdgesFrom([cg.getNodesByName('imported')[0]!.id], ['references']).some(e => e.target === imported.id)).toBe(true);
});

it.each([false, true])('keeps sibling Rust impl owners distinct (own method %s)', async (own) => {
  const cg = await index({ 'main.rs': `mod a {
    pub struct S;
    ${own ? 'impl S { pub fn hit(&self) {} }' : ''}
    pub fn run(s: S) { s.hit(); }
}
mod b {
    pub struct S;
    impl S { pub fn hit(&self) {} }
}
` });
  const run = cg.getNodesByName('run')[0]!;
  const hits = cg.getOutgoingEdgesFrom([run.id], ['calls']).map(e => cg.getNode(e.target)!).filter(n => n.name === 'hit');
  expect(hits.map(n => n.startLine)).toEqual(own ? [3] : []);
});

it('keeps Go external-test packages separate within the same directory', async () => {
  const cg = await index({
    'go.mod': 'module example.com/check\n\ngo 1.22\n',
    'foo.go': 'package foo\ntype Sealed interface { seal() }\ntype Own struct{}\nfunc (Own) seal() {}\n',
    'foo_test.go': 'package foo_test\ntype Other struct{}\nfunc (Other) seal() {}\n',
  });
  const iface = cg.getNodesByName('Sealed')[0]!;
  const implementations = ['Own', 'Other'].map(name => {
    const node = cg.getNodesByName(name)[0]!;
    return cg.getOutgoingEdgesFrom([node.id], ['implements']).some(e => e.target === iface.id);
  });
  expect(implementations).toEqual([true, false]);
});

it('keeps implementation targets when a local package also declares its API', async () => {
  const cg = await index({
    'package.json': JSON.stringify({ name: 'local-library', exports: { '.': { types: './lib.d.ts', default: './lib.mjs' } } }),
    'lib.mjs': 'export function flush() {}\n',
    'lib.d.ts': "declare module 'local-library' { export function flush(): void; }\n",
    'use.js': "import { flush } from 'local-library';\nexport function run() { flush(); }\n",
  });
  const run = cg.getNodesByName('run')[0]!;
  const calls = cg.getOutgoingEdgesFrom([run.id], ['calls']).map(e => cg.getNode(e.target)!.filePath);
  expect(calls).toEqual(['lib.mjs']);
});

it('prefers a callable implementation over its adjacent declaration file', async () => {
  const cg = await index({
    'package.json': JSON.stringify({ name: 'consumer', workspaces: ['packages/*'] }),
    'packages/lib/package.json': JSON.stringify({ name: 'local-lib', exports: { '.': { types: './lib.d.ts', default: './lib.mjs' } } }),
    'packages/lib/lib.mjs': 'export function lib() { return 1; }\n',
    'packages/lib/lib.d.ts': 'export function lib(): number;\n',
    'use.ts': "import { lib } from 'local-lib';\nexport function run() { return lib(); }\n",
  });
  const run = cg.getNodesByName('run')[0]!;
  expect(cg.getOutgoingEdgesFrom([run.id], ['calls']).map(e => cg.getNode(e.target)!.filePath)).toEqual(['packages/lib/lib.mjs']);
});

it('chooses an included C++ definition over an unreferenced historical namesake', async () => {
  const cg = await index({
    'include/current.hpp': 'namespace lib { namespace detail { inline int read() { return 1; } } }\n',
    'historical/old.hpp': 'namespace lib { namespace detail { inline int read() { return 2; } } }\n',
    'main.cpp': '#include "include/current.hpp"\nint run() { return ::lib::detail::read(); }\n',
  });
  const run = cg.getNodesByName('run')[0]!;
  expect(cg.getOutgoingEdgesFrom([run.id], ['calls']).map(e => cg.getNode(e.target)!.filePath)).toEqual(['include/current.hpp']);
});

it('does not call a JSON overload when swapping byte-array elements', async () => {
  const cg = await index({
    'json.hpp': 'struct Json {};\nnamespace std { void swap(Json& a, Json& b) {} }\n',
    'main.cpp': '#include "json.hpp"\nvoid run(unsigned char* ptr, int i, int j) { std::swap(ptr[i], ptr[j]); }\n',
  });
  const run = cg.getNodesByName('run')[0]!;
  expect(cg.getOutgoingEdgesFrom([run.id], ['calls'])).toEqual([]);
});

it('requires concrete std reference overloads to match the argument types', async () => {
  const cg = await index({
    'value.hpp': 'struct Value {};\nnamespace std { void swap(Value& a, Value& b) {} }\n',
    'main.cpp': '#include "value.hpp"\nenum class Tag { Empty };\nunion Payload { int number; double real; };\nstruct Storage { Tag tag; Payload payload; };\nvoid wrong(Storage& a, Storage& b) { std::swap(a.tag, b.tag); std::swap(a.payload, b.payload); }\nvoid right(Value& a, Value& b) { std::swap(a, b); }\n',
  });
  const calls = (name: string) => cg.getOutgoingEdgesFrom([cg.getNodesByName(name)[0]!.id], ['calls'])
    .map(e => cg.getNode(e.target)!.qualifiedName);
  expect(calls('wrong')).toEqual([]);
  expect(calls('right')).toEqual(['std::swap']);
});

it('calls an adjacent implementation when a relative import resolves to declarations', async () => {
  const cg = await index({
    'lib.js': 'export function lib() { return 1; }\n',
    'lib.d.ts': 'export function lib(): number;\n',
    'use.ts': "import { lib } from './lib';\nexport function run() { return lib(); }\n",
  });
  const run = cg.getNodesByName('run')[0]!;
  expect(cg.getOutgoingEdgesFrom([run.id], ['calls']).map(e => cg.getNode(e.target)!.filePath)).toEqual(['lib.js']);
});
