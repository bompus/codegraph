/**
 * A C++ library that opens its namespace with a macro — fmt's
 * `FMT_BEGIN_NAMESPACE` (`namespace fmt { inline namespace v12 {`),
 * pybind11's `PYBIND11_NAMESPACE_BEGIN(PYBIND11_NAMESPACE)` — has its
 * declarations indexed without that namespace, so `fmt::format(…)` found
 * nothing: 1,728 calls on fmt. A qualified name now reaches a declaration the
 * project's own namespace macros put there, through a namespace alias
 * (`namespace py = pybind11;`) too.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const lines = (...l: string[]): string => [...l, ''].join('\n');

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cpp-macro-ns-'));
  const files: Record<string, string> = {
    'overloads.h': `#define OVERLOAD_BEGIN namespace overloaded {
#define OVERLOAD_END }
OVERLOAD_BEGIN
inline int choose(int value) { return 1; }
inline int choose(const char* value) { return 2; }
OVERLOAD_END
`,
    'overloads.cc': `#include "overloads.h"
int overload_call() { return overloaded::choose(1); }
int overload_unread(int n) { return overloaded::choose(mystery(n)); }
`,
    'alpha.h': `#define PROJECT_BEGIN namespace alpha {
#define PROJECT_END }
PROJECT_BEGIN
inline int unique() { return 1; }
PROJECT_END
`,
    'beta.h': `#define PROJECT_BEGIN namespace beta {
#define PROJECT_END }
PROJECT_BEGIN
inline int unique() { return 2; }
PROJECT_END
`,
    'alpha.cc': `#include "alpha.h"
int alpha_call() { return alpha::unique(); }
`,
    'beta.cc': `#include "beta.h"
int beta_call() { return beta::unique(); }
`,
    'aaa.cc': `namespace py = fmt;
`,
    'include/fmt/base.h': `#pragma once
#define FMT_BEGIN_NAMESPACE \\
  namespace fmt {           \\
  inline namespace v12 {
#define FMT_END_NAMESPACE \\
  }                       \\
  }

FMT_BEGIN_NAMESPACE
namespace detail {
inline int to_unsigned(int value) { return value; }
}

inline int format(const char* spec) { return detail::to_unsigned(0); }
FMT_END_NAMESPACE
`,
    'include/pybind11/detail/macros.h': `#pragma once
#define PYBIND11_NAMESPACE pybind11
#define PYBIND11_NAMESPACE_BEGIN(name) \\
    namespace name {                   \\
    PYBIND11_WARNING_PUSH
#define PYBIND11_NAMESPACE_END(name) \\
    PYBIND11_WARNING_POP             \\
    }

PYBIND11_NAMESPACE_BEGIN(PYBIND11_NAMESPACE)
inline void print(const char* text) {}
PYBIND11_NAMESPACE_END(PYBIND11_NAMESPACE)
`,
    'test/format-test.cc': `#include "fmt/base.h"
#include "pybind11/detail/macros.h"

namespace py = pybind11;

int main() {
  fmt::format("{}");
  fmt::detail::to_unsigned(1);
  py::print("hi");
  return 0;
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('C++ namespaces opened by a macro', () => {
  it('picks the visible overload its arguments fit, and declines one they leave undecided', () => {
    // A literal `1` fits `choose(int)`, never `choose(const char*)` (upstream #2612's argument
    // types); an argument whose type can't be read leaves the overload set undecided.
    const calls = (fn: string) => {
      const caller = cg.getNodesByKind('function').find(n => n.name === fn)!;
      return cg.getOutgoingEdgesFrom([caller.id]).filter(e => e.kind === 'calls')
        .map(e => `${cg.getNode(e.target)!.filePath}:${cg.getNode(e.target)!.startLine}`);
    };
    expect(calls('overload_call')).toEqual(['overloads.h:4']);
    expect(calls('overload_unread')).toEqual([]);
  });
  it('uses the namespace macro definition visible in each translation unit', () => {
    for (const owner of ['alpha', 'beta']) {
      const ids = cg.getNodesInFile(`${owner}.cc`).map(n => n.id);
      const targets = cg.getOutgoingEdgesFrom(ids).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)?.filePath);
      expect(targets).toEqual([`${owner}.h`]);
    }
  });
  it('put their declarations under the qualified names callers use', () => {
    const ids = cg.getNodesInFile('test/format-test.cc').map((n) => n.id);
    const targets = cg
      .getOutgoingEdgesFrom(ids)
      .filter((e) => e.kind === 'calls')
      .map((e) => `${cg.getNode(e.target)!.filePath}:${cg.getNode(e.target)!.name}`)
      .sort();
    expect(targets).toEqual([
      'include/fmt/base.h:format',
      'include/fmt/base.h:to_unsigned',
      'include/pybind11/detail/macros.h:print',
    ]);
  });
});

/**
 * A name macro spells a namespace only from its `#define` on: a reference written
 * before `#define ROCKSDB_NAMESPACE rocksdb` is not `rocksdb::DB`.
 */
describe('a C++ name macro applies from its definition on', () => {
  const FILES: Record<string, string> = {
    'db.h': lines(
      'namespace rocksdb { struct DB { void Get() {} }; }',
    ),
    'use.cc': lines(
      '#include "db.h"',
      'void Before(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
      '#define ROCKSDB_NAMESPACE rocksdb',
      'void After(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
      '#undef ROCKSDB_NAMESPACE',
      'void Undone(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
    ),
  };

  let root: string;
  let cg: CodeGraph;

  beforeAll(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-macro-order-'));
    for (const [file, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      fs.writeFileSync(path.join(root, file), text);
    }
    cg = await CodeGraph.init(root, { index: true });
  }, 120_000);

  afterAll(() => {
    cg?.close();
    if (root) fs.rmSync(root, { recursive: true, force: true });
  });

  /** What a function or method calls, as `qualifiedName@file:line`. */
  const callees = (caller: string): string[] => {
    const fn = [...cg.getNodesByKind('function'), ...cg.getNodesByKind('method')].find((n) => n.qualifiedName === caller);
    expect(fn, caller).toBeDefined();
    return cg.getCallees(fn!.id).filter((c) => c.edge.kind === 'calls').map((c) => `${c.node.qualifiedName}@${c.node.filePath}:${c.node.startLine}`).sort();
  };

  it('not before its `#define`, nor after its `#undef`', () => {
    expect(callees('Before')).toEqual([]);
    expect(callees('Undone')).toEqual([]);
    expect(callees('After')).toEqual(['rocksdb::DB::Get@db.h:1']);
  });
});

/**
 * Which `#define` or `#undef` of a name macro is the last one in effect: one in a branch no
 * build compiles (`#if 0`) is not, and an `#undef` does not hide a definition an `#include`
 * after it brings in.
 */
describe('a C++ name macro follows the directives that are in effect', () => {
  const FILES: Record<string, string> = {
    'db.h': lines('namespace rocksdb { struct DB { void Get() {} }; }'),
    'config.h': lines('#define ROCKSDB_NAMESPACE rocksdb'),
    'inactive.cc': lines(
      '#include "db.h"',
      '#define ROCKSDB_NAMESPACE rocksdb',
      '#if 0',
      '#undef ROCKSDB_NAMESPACE',
      '#endif',
      'void Active(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
    ),
    'redefined.cc': lines(
      '#include "db.h"',
      '#undef ROCKSDB_NAMESPACE',
      '#include "config.h"',
      'void Redefined(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
    ),
    'hidden.cc': lines(
      '#include "db.h"',
      '#include "config.h"',
      '#undef ROCKSDB_NAMESPACE',
      'void Hidden(ROCKSDB_NAMESPACE::DB* db) { db->Get(); }',
    ),
  };

  let root: string;
  let cg: CodeGraph;

  beforeAll(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-macro-effect-'));
    for (const [file, text] of Object.entries(FILES)) {
      fs.writeFileSync(path.join(root, file), text);
    }
    cg = await CodeGraph.init(root, { index: true });
  }, 120_000);

  afterAll(() => {
    cg?.close();
    if (root) fs.rmSync(root, { recursive: true, force: true });
  });

  const callees = (caller: string): string[] => {
    const fn = cg.getNodesByKind('function').find((n) => n.qualifiedName === caller);
    expect(fn, caller).toBeDefined();
    return cg.getCallees(fn!.id).filter((c) => c.edge.kind === 'calls').map((c) => `${c.node.qualifiedName}@${c.node.filePath}:${c.node.startLine}`).sort();
  };

  it('ignores an `#undef` in a branch no build compiles', () => {
    expect(callees('Active')).toEqual(['rocksdb::DB::Get@db.h:1']);
  });
  it('lets an include after an `#undef` define the macro again, and an `#undef` after it hide it', () => {
    expect(callees('Redefined')).toEqual(['rocksdb::DB::Get@db.h:1']);
    expect(callees('Hidden')).toEqual([]);
  });
});
