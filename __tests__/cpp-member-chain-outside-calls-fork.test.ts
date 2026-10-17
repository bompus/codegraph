/**
 * Fork-side case beside `cpp-member-chain-outside-calls.test.ts`: a C++ `->` call through a raw
 * pointer to a library type, `out->clear()` on a `std::string* out`, calls the library type's own
 * method, not a project method that only shares its name.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import type { Node } from '../src/types';

const lines = (...l: string[]): string => [...l, ''].join('\n');

describe('a C++ call through a raw pointer to a type from outside the project', () => {
  let root: string;
  let cg: CodeGraph;

  beforeAll(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-raw-pointer-'));
    const write = (file: string, text: string): void => {
      fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      fs.writeFileSync(path.join(root, file), text);
    };
    write('src/buffer.h', lines(
      'class Buffer {',
      ' public:',
      '  void clear() {}',
      '};',
    ));
    write('src/use.cc', lines(
      '#include <string>',
      '#include "src/buffer.h"',
      'void RawPointer(std::string* out) { out->clear(); }',
      'void ProjectPointer(Buffer* b) { b->clear(); }',
    ));
    cg = await CodeGraph.init(root, { index: true });
  });

  afterAll(() => {
    cg?.close();
    fs.rmSync(root, { recursive: true, force: true });
  });

  const callees = (name: string): string[] => {
    const fn = cg.getNodesByKind('function').find((n: Node) => n.name === name);
    if (!fn) throw new Error(`no function ${name}`);
    return cg.getCallees(fn.id).filter((r) => r.edge.kind === 'calls').map((r) => r.node.qualifiedName);
  };

  it('does not link `out->clear()` on a `std::string*` to a project `clear`', () => {
    expect(callees('RawPointer')).not.toContain('Buffer::clear');
  });

  it('still links `b->clear()` on a pointer to the project class', () => {
    expect(callees('ProjectPointer')).toEqual(['Buffer::clear']);
  });
});
