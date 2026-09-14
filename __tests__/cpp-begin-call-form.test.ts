/**
 * C/C++ extractor drops `this` and 2-hop field receivers, so a bare `begin`
 * often means `array->begin()` or ADL `begin(x)`, not the implicit-this
 * member. Exact-name used to pick among every same-named `begin` by
 * proximity / exported; these fixtures pin the call-site-form rule that
 * replaced that guess.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

function project(files: Record<string, string>): void {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-cpp-begin-'));
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(tempDir, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, content);
  }
}

function callTargets(caller: string): string[] {
  const from = cg!.getNodesByName(caller).find((n) => n.kind !== 'file');
  expect(from, `missing caller ${caller}`).toBeDefined();
  return cg!.getOutgoingEdges(from!.id).filter((e) => e.kind === 'calls').map((e) => e.target);
}

function nodeNamed(name: string, file: string) {
  return cg!.getNodesByName(name).find((n) => n.filePath.replace(/\\/g, '/').endsWith(file));
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe('C++ begin call-site form', () => {
  async function indexFixture(): Promise<void> {
    project({
      'json.hpp': `
struct Inner {
  int* begin() { return nullptr; }
};
struct Json {
  Inner* array;
  int* begin() { return nullptr; }
  int* front() { return begin(); }
  void wipe() { this->array->begin(); }
};
struct Ordered {
  void at() { this->begin(); }
};
struct Dictionary {
  int* begin() { return nullptr; }
  bool contains() { return begin() != nullptr; }
};
`,
      'helper.cpp': `int* begin(int* x) { return x; }\n`,
      'call.cpp': `
void algorithms() {
  int expected = 0;
  begin(&expected);
}
`,
    });
    cg = await CodeGraph.init(tempDir, { index: true });
    cg.resolveReferences();
  }

  it('keeps implicit-this begin() on the enclosing type', async () => {
    await indexFixture();
    const begins = cg!.getNodesByName('begin').filter((n) => n.filePath.replace(/\\/g, '/').endsWith('json.hpp'));
    const jsonTypeBegin = begins.find((n) => n.qualifiedName.includes('Json'));
    const dictTypeBegin = begins.find((n) => n.qualifiedName.includes('Dictionary'));
    expect(jsonTypeBegin).toBeDefined();
    expect(dictTypeBegin).toBeDefined();
    expect(callTargets('front')).toContain(jsonTypeBegin!.id);
    expect(callTargets('contains')).toContain(dictTypeBegin!.id);
  });

  it('does not attach 2-hop array->begin() to the enclosing type\'s begin', async () => {
    await indexFixture();
    const jsonTypeBegin = cg!.getNodesByName('begin').find(
      (n) => n.qualifiedName.includes('Json') && n.filePath.replace(/\\/g, '/').endsWith('json.hpp'),
    );
    expect(jsonTypeBegin).toBeDefined();
    expect(callTargets('wipe')).not.toContain(jsonTypeBegin!.id);
  });

  it('does not attach this->begin() to another type\'s begin', async () => {
    await indexFixture();
    const jsonTypeBegin = cg!.getNodesByName('begin').find(
      (n) => n.qualifiedName.includes('Json') && n.filePath.replace(/\\/g, '/').endsWith('json.hpp'),
    );
    const helperBegin = nodeNamed('begin', 'helper.cpp');
    expect(jsonTypeBegin).toBeDefined();
    expect(callTargets('at')).not.toContain(jsonTypeBegin!.id);
    if (helperBegin) expect(callTargets('at')).not.toContain(helperBegin.id);
  });

  it('does not attach ADL begin(x) to a cross-TU helper or a class member', async () => {
    await indexFixture();
    const helperBegin = nodeNamed('begin', 'helper.cpp');
    const jsonTypeBegin = cg!.getNodesByName('begin').find(
      (n) => n.qualifiedName.includes('Json') && n.filePath.replace(/\\/g, '/').endsWith('json.hpp'),
    );
    expect(helperBegin).toBeDefined();
    expect(jsonTypeBegin).toBeDefined();
    const targets = callTargets('algorithms');
    expect(targets).not.toContain(helperBegin!.id);
    expect(targets).not.toContain(jsonTypeBegin!.id);
  });
});
