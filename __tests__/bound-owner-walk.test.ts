import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});
async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-bound-owner-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
  return graph;
}
function calls(caller: string) {
  const from = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')].find(n => n.name === caller)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(e => e.kind === 'calls')
    .map(e => `${e.line}:${graph!.getNode(e.target)!.qualifiedName}`).sort();
}

describe('bound receiver owners and supertype walks', () => {
  it('looks a Python method up along the C3 order, not the first base chain', async () => {
    await project({ 'models.py': `class Base:
    def save(self): pass
class Tracked(Base): pass
class Audited(Base):
    def save(self): pass
class Record(Tracked, Audited): pass
class Left(Base):
    def save(self): pass
class Pair(Left, Audited): pass
def run():
    rec = Record()
    rec.save()
    pair = Pair()
    pair.save()
` });
    expect(calls('run')).toEqual(expect.arrayContaining(['12:Audited::save', '14:Left::save']));
    expect(calls('run')).not.toContain('12:Base::save');
  });

  it('resolves a call on an enum-typed Java field', async () => {
    await project({ 'src/K.java': `enum K { A; void mymethod() {} }
class J { private K k = K.A; void user() { k.mymethod(); } }
` });
    expect(calls('user')).toEqual(['2:K::mymethod']);
  });

  it('lets a Java local hide a same-named field', async () => {
    await project({ 'src/K.java': `class K { void mymethod() {} }
class L { void mymethod() {} }
class J {
  private K k = new K();
  static L make() { return new L(); }
  void user() {
    var k = make();
    k.mymethod();
  }
  void field() { k.mymethod(); }
  void explicit() {
    var k = make();
    this.k.mymethod();
  }
}
` });
    expect(calls('user')).not.toContain('8:K::mymethod');
    expect(calls('field')).toEqual(['10:K::mymethod']);
    expect(calls('explicit')).toContain('13:K::mymethod');
  });

  it('walks the supertypes only of a Java type the call site can see', async () => {
    await project({
      'src/app/K.java': `package app;
import java.util.Map;
class J3 {
  private Map.Entry<String, Integer> k;
  void user3() { k.getKey(); }
}
`,
      'src/app/I.java': `package app;
import lib.Entry;
class J4 {
  private Entry e;
  void user4() { e.getKey(); }
}
`,
      'src/other/Entry.java': `package other;
class BaseEntry { String getKey() { return ""; } }
public class Entry extends BaseEntry {}
`,
      'src/lib/Entry.java': `package lib;
class LibBase { String getKey() { return ""; } }
public class Entry extends LibBase {}
`,
    });
    expect(calls('user3')).toEqual([]);
    expect(calls('user4')).toEqual(['5:lib::LibBase::getKey']);
  });
});
