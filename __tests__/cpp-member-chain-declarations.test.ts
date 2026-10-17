/**
 * A C++ call on a member reached through another member (`compact->compaction->level()`,
 * `b->rep.Clear()`, `shared.mu.Lock()`, `this->mu_->Lock()`) links to the method of the member's
 * class wherever that class or the object's type is declared in one of the ways leveldb writes them:
 *
 * - a struct defined out of line as a nested class: `struct DBImpl::CompactionState { … };`,
 *   declared `struct CompactionState;` inside `DBImpl`;
 * - a global struct whose member's type comes in through `using leveldb::WriteBatch;`;
 * - a local built with constructor-call syntax, `SharedState shared(n);`, which tree-sitter reads
 *   as a function declaration; it declares an object when its "parameters" are variables in scope;
 * - a class written with an attribute macro, `class SCOPED_LOCKABLE MutexLock`,
 *   `struct LEVELDB_EXPORT Options`, which extraction blanks before parsing.
 *
 * Each class here has a namesake method on `Decoy`, so a link by the bare method name would pick a
 * class the receiver isn't.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import type { Node } from '../src/types';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function indexed(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-member-chain-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

function callable(cg: CodeGraph, qualifiedName: string): Node {
  const node = [...cg.getNodesByKind('method'), ...cg.getNodesByKind('function')].find((n) => n.qualifiedName === qualifiedName);
  if (!node) throw new Error(`no function or method ${qualifiedName}`);
  return node;
}

/** The qualified names of a function's or method's `calls` callees, sorted. */
function calls(cg: CodeGraph, caller: string): string[] {
  return cg
    .getCallees(callable(cg, caller).id)
    .filter((r) => r.edge.kind === 'calls')
    .map((r) => r.node.qualifiedName)
    .sort();
}

const lines = (...text: string[]): string => [...text, ''].join('\n');

const TYPES = lines(
  'namespace ns {',
  'namespace port {',
  'class Mutex {',
  ' public:',
  '  void Lock() {}',
  '  void Unlock() {}',
  '};',
  'class CondVar {',
  ' public:',
  '  void Wait() {}',
  '};',
  '}  // namespace port',
  'class Compaction {',
  ' public:',
  '  int level() const { return 0; }',
  '};',
  'class Block {',
  ' public:',
  '  int NewIterator() { return 0; }',
  '};',
  'class WriteBatch {',
  ' public:',
  '  void Clear() {}',
  '};',
  'class Env {',
  ' public:',
  '  virtual void CreateDir() {}',
  '};',
  'class Decoy {',
  ' public:',
  '  void Lock() {}',
  '  void Unlock() {}',
  '  void Wait() {}',
  '  int level() const { return 1; }',
  '  int NewIterator() { return 1; }',
  '  void Clear() {}',
  '  void CreateDir() {}',
  '};',
  '}  // namespace ns',
);

describe('C++ member chains through how the member is declared', () => {
  it('reads a member of a struct defined out of line as a nested class', async () => {
    const cg = await indexed({
      'src/types.h': TYPES,
      'src/table.h': lines(
        '#include "src/types.h"',
        'namespace ns {',
        'class Table {',
        ' public:',
        '  int NewIterator() { return 2; }',
        '  void Run();',
        '  struct Rep;',
        ' private:',
        '  Rep* rep_;',
        '};',
        '}  // namespace ns',
      ),
      'src/table.cc': lines(
        '#include "src/table.h"',
        'namespace ns {',
        'struct Table::Rep {',
        '  Block* index_block;',
        '  Compaction* const compaction;',
        '  port::CondVar cv;',
        '};',
        'void Table::Run() {',
        '  rep_->index_block->NewIterator();',
        '  rep_->compaction->level();',
        '}',
        'void T_viaParameter(Table::Rep* r) { r->compaction->level(); r->cv.Wait(); }',
        '}  // namespace ns',
      ),
    });
    try {
      // The member's class, not the method of the class the struct is nested in.
      expect(calls(cg, 'ns::Table::Run')).toEqual(['ns::Block::NewIterator', 'ns::Compaction::level']);
      expect(calls(cg, 'ns::T_viaParameter')).toEqual(['ns::Compaction::level', 'ns::port::CondVar::Wait']);
    } finally {
      cg.close();
    }
  });

  it('reads a member of a global struct whose type a using-declaration brings in', async () => {
    const cg = await indexed({
      'src/types.h': TYPES,
      'src/c.cc': lines(
        '#include "src/types.h"',
        'using ns::Env;',
        'using ns::WriteBatch;',
        'struct wb_t { WriteBatch rep; };',
        'struct env_t { Env* rep; };',
        'struct unknown_t { Unknown rep; };',
        'void T_value(wb_t* b) { b->rep.Clear(); }',
        'void T_pointer(env_t* e) { e->rep->CreateDir(); }',
        'void T_unknown(unknown_t* u) { u->rep.Clear(); }',
      ),
    });
    try {
      expect(calls(cg, 'T_value')).toEqual(['ns::WriteBatch::Clear']);
      expect(calls(cg, 'T_pointer')).toEqual(['ns::Env::CreateDir']);
      // A member whose type names no visible class is called nothing.
      expect(calls(cg, 'T_unknown')).toEqual([]);
    } finally {
      cg.close();
    }
  });

  it('reads a local constructed with call syntax when its arguments are variables', async () => {
    const cg = await indexed({
      'src/types.h': TYPES,
      'src/bench.cc': lines(
        '#include "src/types.h"',
        'namespace ns {',
        'struct Config {};',
        'struct Shared {',
        '  explicit Shared(int n) {}',
        '  port::Mutex mu;',
        '};',
        'void T_parameter(int n) { Shared shared(n); shared.mu.Lock(); }',
        'void T_local() { int n = 2; Shared shared(n); shared.mu.Unlock(); }',
        // `Config` is a type: this declares a function `shared`, not a `Shared`.
        'void T_functionDeclaration() { Shared shared(Config); shared.mu.Lock(); }',
        '}  // namespace ns',
      ),
    });
    try {
      expect(calls(cg, 'ns::T_parameter')).toEqual(['ns::port::Mutex::Lock']);
      expect(calls(cg, 'ns::T_local')).toEqual(['ns::port::Mutex::Unlock']);
      expect(calls(cg, 'ns::T_functionDeclaration')).not.toContain('ns::port::Mutex::Lock');
    } finally {
      cg.close();
    }
  });

  it('reads a member of a class written with an attribute macro', async () => {
    const cg = await indexed({
      'src/types.h': TYPES,
      'src/lock.h': lines(
        '#include "src/types.h"',
        '#define SCOPED_LOCKABLE',
        '#define LIB_EXPORT',
        'namespace ns {',
        'class SCOPED_LOCKABLE MutexLock {',
        ' public:',
        '  explicit MutexLock(port::Mutex* mu) : mu_(mu) { this->mu_->Lock(); }',
        '  ~MutexLock() { this->mu_->Unlock(); }',
        ' private:',
        '  port::Mutex* const mu_;',
        '};',
        'struct LIB_EXPORT Options {',
        '  Env* env;',
        '};',
        'void T_options(const Options& o) { o.env->CreateDir(); }',
        '}  // namespace ns',
      ),
    });
    try {
      expect(calls(cg, 'ns::MutexLock::MutexLock')).toEqual(['ns::port::Mutex::Lock']);
      expect(calls(cg, 'ns::MutexLock::~MutexLock')).toEqual(['ns::port::Mutex::Unlock']);
      expect(calls(cg, 'ns::T_options')).toEqual(['ns::Env::CreateDir']);
    } finally {
      cg.close();
    }
  });
});
