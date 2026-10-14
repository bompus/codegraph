/**
 * Edge cases of the C++ resolver found reviewing the upstream reconcile (#497):
 *
 * - `Ptr p(0)` for `using Ptr = Widget*;` declares a pointer; it constructs no `Widget`.
 * - A class nested in a function-local class is as local to the function as that class.
 * - A class an included `.cc` file declares is the includer's.
 * - A `using Base<T>::foo;` in a nested class is not the outer class's.
 * - A header's unnamed namespace is the includer's, not every file's.
 * - Of two headers declaring one class, an object creation takes the one it includes.
 * - `NULL` is an integer zero; a string literal converts to `const void*` ahead of a class.
 * - Annotation blanking leaves `const int FOO_BAR = 1;` and `n * FOO_BAR::VALUE` alone.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { blankCppDeclaratorAttributeMacros, blankCppPointerAnnotationMacros } from '../src/extraction/languages/c-cpp';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-edge-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

const lines = (...text: string[]): string => [...text, ''].join('\n');

/** What a function's edges of `kind` reach at `line`, as `target:its first line @confidence`. */
function reached(cg: CodeGraph, qualifiedName: string, kind: 'calls' | 'instantiates', line?: number): string[] {
  const from = [...cg.getNodesByKind('method'), ...cg.getNodesByKind('function')].find((n) => n.qualifiedName === qualifiedName)!;
  return cg.getOutgoingEdgesFrom([from.id], [kind])
    .filter((e) => line === undefined || e.line === line)
    .map((e) => {
      const t = cg.getNode(e.target)!;
      return `${t.qualifiedName}:${t.startLine} @${(e.metadata as { confidence?: number } | undefined)?.confidence}`;
    })
    .sort();
}

describe('C++ object creation', () => {
  it('constructs no class for a pointer alias of it', async () => {
    const cg = await indexed({
      'widget.h': lines(
        'class Widget {',
        ' public:',
        '  Widget(int n) {}',
        '};',
        'using Ptr = Widget*;',
        'void Run() {',
        '  Ptr p(0);',
        '  Widget w(1);',
        '}',
      ),
    });
    try {
      expect(reached(cg, 'Run', 'instantiates', 7)).toEqual([]);
      expect(reached(cg, 'Run', 'instantiates', 8)).toEqual(['Widget:1 @0.9']);
    } finally {
      cg.close();
    }
  });

  it('constructs no class for a chain of pointer aliases (a global-qualified one too), and the class for an alias with a comment', async () => {
    const cg = await indexed({
      'widget.h': lines(
        'class Widget {',
        ' public:',
        '  Widget(int n) {}',
        '};',
        'using Ptr = Widget*;',
        'using Ptr2 = Ptr;',
        'using Global = ::Ptr;',
        'using Plain = Widget /* an object * not a pointer */;',
        'void Run() {',
        '  Ptr2 q(0);',
        '  Plain w(1);',
        '  Global g(0);',
        '}',
      ),
    });
    try {
      expect(reached(cg, 'Run', 'instantiates', 10)).toEqual([]);
      expect(reached(cg, 'Run', 'instantiates', 11)).toEqual(['Widget:1 @0.9']);
      expect(reached(cg, 'Run', 'instantiates', 12)).toEqual([]);
    } finally {
      cg.close();
    }
  });

  it("does not see a class nested in a function's local class from another function", async () => {
    const cg = await indexed({
      'a_test.cc': lines(
        'namespace ns {',
        'struct Local { struct Inner { Inner(int n) {} }; };',
        'TEST_F(A, B) {',
        '  struct Local { struct Inner { Inner(int n) {} }; };',
        '  Local::Inner own(1);',
        '}',
        'TEST_F(A, C) {',
        '  Local::Inner other(1);',
        '}',
        '}  // namespace ns',
      ),
    });
    try {
      const made = cg.getNodesByKind('function').filter((n) => n.name === 'TEST_F').sort((a, b) => a.startLine - b.startLine)
        .map((fn) => cg.getOutgoingEdgesFrom([fn.id], ['instantiates']).map((e) => `${fn.startLine} -> ${cg.getNode(e.target)?.startLine}`).sort());
      // (Each `Inner` is recorded by the name before its first `::`, `Local`.)
      expect(made).toEqual([['3 -> 4'], ['7 -> 2']]);
    } finally {
      cg.close();
    }
  });

  it('constructs a class an included source file declares', async () => {
    const cg = await indexed({
      'engine.cc': lines('class Engine {', ' public:', '  Engine(int n) {}', '};'),
      'engine_test.cc': lines('#include "engine.cc"', 'void Run() {', '  Engine e(1);', '}'),
    });
    try {
      expect(reached(cg, 'Run', 'instantiates')).toEqual(['Engine:1 @0.9']);
    } finally {
      cg.close();
    }
  });

  it('takes, of two headers declaring one class, the one it includes', async () => {
    const widget = (n: number) => lines('namespace ns {', 'class Widget {', ' public:', `  Widget(int n) {}  // ${n}`, '};', '}  // namespace ns');
    const cg = await indexed({
      'one/widget.h': widget(1),
      'two/widget.h': widget(2),
      'one/use.cc': lines('#include "two/widget.h"', 'void Run() {', '  ns::Widget w(1);', '}'),
    });
    try {
      const run = [...cg.getNodesByKind('function')].find((n) => n.qualifiedName === 'Run')!;
      const files = cg.getOutgoingEdgesFrom([run.id], ['instantiates']).map((e) => cg.getNode(e.target)?.filePath);
      expect(files).toEqual(['two/widget.h']);
    } finally {
      cg.close();
    }
  });
});

describe('C++ calls', () => {
  it("does not take a nested class's using-declaration for the outer class's", async () => {
    const cg = await indexed({
      'outer.h': lines(
        'template <typename T>',
        'class Base {',
        ' public:',
        '  void foo() {}',
        '};',
        'void foo() {}',
        'template <typename T>',
        'class Outer : public Base<T> {',
        ' public:',
        '  struct Nested : Base<T> {',
        '    using Base<T>::foo;',
        '  };',
        '  void run() {',
        '    foo();',
        '  }',
        '};',
      ),
    });
    try {
      // (The free function, at the confidence of a name a dependent base might also give.)
      expect(reached(cg, 'Outer::run', 'calls', 14).map((r) => r.split(' ')[0])).toEqual(['foo:6']);
    } finally {
      cg.close();
    }
  });

  it("reads a class's using-declaration when its first line also opens a namespace", async () => {
    const cg = await indexed({
      'outer.h': lines(
        'template <typename T>',
        'class Base {',
        ' public:',
        '  void foo() {}',
        '};',
        'void foo() {}',
        'namespace ns { template <typename T> class Outer : public Base<T> {',
        ' public:',
        '  using Base<T>::foo;',
        '  void run() {',
        '    foo();',
        '  }',
        '}; }',
      ),
    });
    try {
      expect(reached(cg, 'ns::Outer::run', 'calls', 11).map((r) => r.split(' ')[0])).toEqual(['Base::foo:4']);
    } finally {
      cg.close();
    }
  });

  it('takes NULL for an integer zero, not for a nullptr', async () => {
    const cg = await indexed({
      'reset.h': lines(
        '#include <cstddef>',
        'class Holder {',
        ' public:',
        '  void Reset(std::nullptr_t) {}',
        '  void Reset(int n) {}',
        '  void Run() { Reset(NULL); }',
        '};',
      ),
    });
    try {
      expect(reached(cg, 'Holder::Run', 'calls', 6)).toEqual(['Holder::Reset:5 @0.9']);
    } finally {
      cg.close();
    }
  });

  it("takes a string literal for a `const void*` ahead of a std::string it converts to", async () => {
    const cg = await indexed({
      'take.h': lines(
        'void Take(const std::string& text) {}',
        'void Take(const void* data) {}',
        'void Run() { Take("x"); }',
      ),
    });
    try {
      expect(reached(cg, 'Run', 'calls', 3).map((r) => r.split(' ')[0])).toEqual(['Take:2']);
    } finally {
      cg.close();
    }
  });

  it("is not what a header's unnamed namespace declares, named by a file that does not include it", async () => {
    const cg = await indexed({
      'conformance/matchers.h': lines('namespace conformance {', 'namespace {', 'int RawPayload(int response) { return response; }', '}  // namespace', '}  // namespace conformance'),
      'conformance/matchers_test.cc': lines('namespace conformance {', 'int Describe() {', '  return RawPayload(1);', '}', '}  // namespace conformance'),
      'conformance/uses_matchers.cc': lines('#include "matchers.h"', 'namespace conformance {', 'int Check() {', '  return RawPayload(1);', '}', '}  // namespace conformance'),
    });
    try {
      const filesOf = (name: string) => {
        const from = cg.getNodesByKind('function').find((n) => n.qualifiedName === name)!;
        return cg.getOutgoingEdgesFrom([from.id], ['calls']).map((e) => cg.getNode(e.target)?.filePath);
      };
      expect(filesOf('conformance::Describe')).toEqual([]);
      expect(filesOf('conformance::Check')).toEqual(['conformance/matchers.h']);
    } finally {
      cg.close();
    }
  });
});

describe('C++ annotation blanking', () => {
  it('leaves a constant named in capitals alone', () => {
    expect(blankCppDeclaratorAttributeMacros('const int FOO_BAR = 1;')).toBe('const int FOO_BAR = 1;');
    expect(blankCppDeclaratorAttributeMacros('static constexpr unsigned MAX_LEN = 4;')).toBe('static constexpr unsigned MAX_LEN = 4;');
  });

  it('blanks a macro after a name that follows a const-qualified type', () => {
    expect(blankCppDeclaratorAttributeMacros('int const value_ GUARDED_BY(mu_);')).toBe('int const value_ ' + ' '.repeat('GUARDED_BY(mu_)'.length) + ';');
    expect(blankCppDeclaratorAttributeMacros('Foo const volatile value_ GUARDED_BY(mu_);')).toBe('Foo const volatile value_ ' + ' '.repeat('GUARDED_BY(mu_)'.length) + ';');
  });

  it('leaves a qualified name after a product alone', () => {
    expect(blankCppPointerAnnotationMacros('consume(n * FOO_BAR::VALUE);')).toBe('consume(n * FOO_BAR::VALUE);');
  });
});
