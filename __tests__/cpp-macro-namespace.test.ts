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
  it('declines a visible overload set without argument-type evidence', () => {
    const ids = cg.getNodesInFile('overloads.cc').map(n => n.id);
    expect(cg.getOutgoingEdgesFrom(ids).filter(e => e.kind === 'calls')).toEqual([]);
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
