/**
 * A C++ construction is placed by lookup over the headers its file can see.
 * nlohmann/json keeps an old release of the whole library in
 * `tests/abi/include/nlohmann/json_v3_10_5.hpp`, which spells `namespace
 * nlohmann {` out, while the library's own headers open it with a macro, so
 * only the old copy's types are indexed under `nlohmann::`. The docs
 * examples' `json::json_pointer p("/")`, through `using json =
 * nlohmann::json;`, reached that copy. A header of a test tree is now seen
 * only from inside that tree or by including it, and an alias lookup can't
 * follow leaves a member the project declares a class for to the name
 * strategies.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-test-tree-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

const lines = (...text: string[]): string => [...text, ''].join('\n');

/** The files of what a function's `instantiates` edges reach, as `qualifiedName in file`. */
function made(cg: CodeGraph, qualifiedName: string): string[] {
  const from = cg.getNodesByKind('function').find((n) => n.qualifiedName === qualifiedName && !n.filePath.includes('include'))!;
  return cg.getOutgoingEdgesFrom([from.id], ['instantiates'])
    .map((e) => cg.getNode(e.target)!)
    .map((n) => `${n.qualifiedName} in ${n.filePath}`)
    .sort();
}

const LIBRARY = lines(
  '#define LIB_BEGIN namespace lib {',
  '#define LIB_END }',
  'LIB_BEGIN',
  'class json_pointer { public: json_pointer(const char* s) {} };',
  'class basic_json { public: using json_pointer = ::lib::json_pointer; basic_json(int v) {} };',
  'using json = basic_json;',
  'LIB_END',
);

const OLD_RELEASE = lines(
  'namespace lib {',
  'class json_pointer { public: json_pointer(const char* s) {} };',
  'class basic_json { public: using json_pointer = ::lib::json_pointer; basic_json(int v) {} };',
  'using json = basic_json;',
  '}  // namespace lib',
);

const files = {
  // Two copies of the library, so `<lib/json.hpp>` resolves to neither.
  'include/lib/json.hpp': LIBRARY,
  'single_include/lib/json.hpp': LIBRARY,
  'tests/abi/include/lib/json_v1.hpp': OLD_RELEASE,
};

describe('C++ constructions and the headers of a test tree', () => {
  it('are not placed in a test tree code outside it does not include', async () => {
    const cg = await indexed({
      ...files,
      'docs/examples/pointer.cpp': lines(
        '#include <lib/json.hpp>',
        'using json = lib::json;',
        'int main() {',
        '  json::json_pointer p("/");',
        '  json j(1);',
        '  return 0;',
        '}',
      ),
      'tests/benchmarks/bench.cpp': lines(
        '#include <lib/json.hpp>',
        'using json = lib::json;',
        'int bench() {',
        '  json j(1);',
        '  return 0;',
        '}',
      ),
    });
    const examples = made(cg, 'main');
    expect(examples.some((t) => t.includes('tests/abi/'))).toBe(false);
    expect(examples.some((t) => t.startsWith('json_pointer in '))).toBe(true);
    expect(made(cg, 'bench')).toEqual(['json in tests/benchmarks/bench.cpp']);
    cg.close();
  });

  it('are placed there from inside the tree', async () => {
    const cg = await indexed({
      ...files,
      'tests/abi/abi.cpp': lines(
        'int abi() {',
        '  lib::json_pointer p("/");',
        '  return 0;',
        '}',
      ),
    });
    expect(made(cg, 'abi')).toEqual(['lib::json_pointer in tests/abi/include/lib/json_v1.hpp']);
    cg.close();
  });
});
