/**
 * A method inherited through `class Child extends Base` comes from the `Base`
 * the declaration names, never from a same-named `Base` in another package or
 * namespace.
 *
 * The conformance fallback walked supertypes by their bare names: from the
 * first supertype on, `Base::m` matched a method on any `Base` in the project,
 * and the supertypes of every same-named `Base` were pooled. A call on a
 * `Child` receiver then linked to an unrelated package's `Base.m()`, or, when
 * the real `Base` lacked `m` and a grandparent had it, skipped the grandparent
 * for the decoy. The walk now follows the `extends`/`implements` edges' target
 * declarations for the languages listed below.
 *
 * Each case runs with the decoy package sorting before and after the real
 * one, since ties between same-named candidates fell to index order.
 * Java and Kotlin reach the declaration through their bound-type path when a
 * parameter or local holds the receiver, and are kept as guards there; a
 * factory call's return type reaches the fallback. Scala and Swift keep a separate
 * heuristic that links a method name only one type declares, so the
 * "decoy has the only method" case is asserted for C#, Java and Kotlin alone.
 */
import { describe, it, expect, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterAll(() => {
  for (const r of roots.splice(0)) fs.rmSync(r, { recursive: true, force: true });
});

type Scenario = 'direct' | 'grand' | 'decoyOnly';
interface Layout {
  real: string;
  decoy: string | null;
  scenario: Scenario;
}

const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

interface Lang {
  name: string;
  /** Files by relative path; `caller` names the one holding `c.m()`. */
  build(l: Layout): Record<string, string>;
  caller(l: Layout): string;
  method: string;
  /** Scenarios the language is held to. */
  scenarios: Scenario[];
}

const hasRoot = (l: Layout) => l.scenario === 'grand';
const ownsMethod = (l: Layout) => l.scenario === 'direct';
// 'decoyOnly': the real chain has no `m` at all; only the decoy declares one.

const languages: Lang[] = [
  {
    name: 'java',
    method: 'm',
    scenarios: ['direct', 'grand', 'decoyOnly'],
    caller: () => 'main/Use.java',
    build(l) {
      const f: Record<string, string> = {};
      if (hasRoot(l)) f[`${l.real}/Root.java`] = `package ${l.real};\npublic class Root { public void m() {} }\n`;
      f[`${l.real}/Base.java`] = `package ${l.real};\npublic class Base${hasRoot(l) ? ' extends Root' : ''} { ${ownsMethod(l) ? 'public void m() {}' : ''} }\n`;
      f[`${l.real}/Child.java`] = `package ${l.real};\npublic class Child extends Base { }\n`;
      if (l.decoy) f[`${l.decoy}/Base.java`] = `package ${l.decoy};\npublic class Base { public void m() {} }\n`;
      f['main/Use.java'] = `package main;\nimport ${l.real}.Child;\npublic class Use { void f(Child c) { c.m(); } }\n`;
      return f;
    },
  },
  {
    name: 'kotlin',
    method: 'm',
    scenarios: ['direct', 'grand', 'decoyOnly'],
    caller: () => 'main/Use.kt',
    build(l) {
      const f: Record<string, string> = {};
      if (hasRoot(l)) f[`${l.real}/Root.kt`] = `package ${l.real}\nopen class Root { fun m() {} }\n`;
      f[`${l.real}/Base.kt`] = `package ${l.real}\nopen class Base${hasRoot(l) ? '() : Root()' : ''} { ${ownsMethod(l) ? 'fun m() {}' : ''} }\n`;
      f[`${l.real}/Child.kt`] = `package ${l.real}\nclass Child : Base()\n`;
      if (l.decoy) f[`${l.decoy}/Base.kt`] = `package ${l.decoy}\nopen class Base { fun m() {} }\n`;
      f['main/Use.kt'] = `package main\nimport ${l.real}.Child\nclass Use { fun f(c: Child) { c.m() } }\n`;
      return f;
    },
  },
  {
    // A factory's return type names the receiver, which no binding holds.
    name: 'java via a factory call',
    method: 'm',
    scenarios: ['direct', 'grand', 'decoyOnly'],
    caller: () => 'main/Use.java',
    build(l) {
      const f = languages[0].build(l);
      f['main/Use.java'] = `package main;\nimport ${l.real}.Child;\npublic class Use { static Child make() { return null; } void f() { Use.make().m(); } }\n`;
      return f;
    },
  },
  {
    name: 'kotlin via a factory call',
    method: 'm',
    scenarios: ['direct', 'grand', 'decoyOnly'],
    caller: () => 'main/Use.kt',
    build(l) {
      const f = languages[1].build(l);
      f['main/Use.kt'] = `package main\nimport ${l.real}.Child\nobject Use { fun make(): Child = Child()\n  fun f() { Use.make().m() } }\n`;
      return f;
    },
  },
  {
    name: 'csharp',
    method: 'M',
    scenarios: ['direct', 'grand', 'decoyOnly'],
    caller: () => 'main/Use.cs',
    build(l) {
      const f: Record<string, string> = {};
      const ns = cap(l.real);
      if (hasRoot(l)) f[`${l.real}/Root.cs`] = `namespace ${ns} { public class Root { public void M() {} } }\n`;
      f[`${l.real}/Base.cs`] = `namespace ${ns} { public class Base${hasRoot(l) ? ' : Root' : ''} { ${ownsMethod(l) ? 'public void M() {}' : ''} } }\n`;
      f[`${l.real}/Child.cs`] = `namespace ${ns} { public class Child : Base { } }\n`;
      if (l.decoy) f[`${l.decoy}/Base.cs`] = `namespace ${cap(l.decoy)} { public class Base { public void M() {} } }\n`;
      f['main/Use.cs'] = `using ${ns};\nnamespace Main { public class Use { void F(Child c) { c.M(); } } }\n`;
      return f;
    },
  },
  {
    name: 'scala',
    method: 'm',
    scenarios: ['direct', 'grand'],
    caller: () => 'main/Use.scala',
    build(l) {
      const f: Record<string, string> = {};
      if (hasRoot(l)) f[`${l.real}/Root.scala`] = `package ${l.real}\nclass Root { def m(): Unit = {} }\n`;
      f[`${l.real}/Base.scala`] = `package ${l.real}\nclass Base${hasRoot(l) ? ' extends Root' : ''} { ${ownsMethod(l) ? 'def m(): Unit = {}' : ''} }\n`;
      f[`${l.real}/Child.scala`] = `package ${l.real}\nclass Child extends Base\n`;
      if (l.decoy) f[`${l.decoy}/Base.scala`] = `package ${l.decoy}\nclass Base { def m(): Unit = {} }\n`;
      f['main/Use.scala'] = `package main\nimport ${l.real}.Child\nclass Use { def f(c: Child): Unit = { c.m() } }\n`;
      return f;
    },
  },
  {
    name: 'swift',
    method: 'm',
    scenarios: ['direct', 'grand'],
    caller: () => 'main/Use.swift',
    build(l) {
      const f: Record<string, string> = {};
      if (hasRoot(l)) f[`${l.real}/Root.swift`] = 'open class Root { public func m() {} }\n';
      f[`${l.real}/Base.swift`] = `open class Base${hasRoot(l) ? ': Root' : ''} { ${ownsMethod(l) ? 'public func m() {}' : ''} }\n`;
      f[`${l.real}/Child.swift`] = 'open class Child: Base { }\n';
      if (l.decoy) f[`${l.decoy}/Base.swift`] = 'open class Base { public func m() {} }\n';
      f['main/Use.swift'] = 'func f(c: Child) { c.m() }\n';
      return f;
    },
  },
];

/** The file that must hold the callee, or none when the call has no project target. */
function expectedFile(l: Layout, lang: Lang): string | null {
  if (l.scenario === 'decoyOnly') return null;
  const ext = Object.keys(lang.build(l))[0].split('.').pop();
  return `${l.real}/${l.scenario === 'grand' ? 'Root' : 'Base'}.${ext}`;
}

async function callTargets(lang: Lang, l: Layout): Promise<string[]> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `cg-supertype-${lang.name}-`));
  roots.push(root);
  for (const [rel, content] of Object.entries(lang.build(l))) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  const cg = await CodeGraph.init(root, { index: true });
  try {
    const out: string[] = [];
    for (const n of cg.getNodesInFile(lang.caller(l))) {
      for (const e of cg.getOutgoingEdges(n.id)) {
        if (e.kind !== 'calls') continue;
        const t = cg.getNode(e.target)!;
        if (t.name === lang.method) out.push(t.filePath);
      }
    }
    return out.sort();
  } finally {
    cg.close();
  }
}

// The decoy package sorts before the real one in one layout and after it in the other.
const orders = [
  { real: 'zreal', decoy: 'adecoy' },
  { real: 'areal', decoy: 'zdecoy' },
];

describe('inherited method calls follow the declared supertype', () => {
  for (const lang of languages) {
    for (const scenario of lang.scenarios) {
      for (const order of orders) {
        const where = scenario === 'direct' ? 'on the parent' : scenario === 'grand' ? 'past a parent without it' : 'declared only by the other package';
        it(`${lang.name}: the method ${where} ignores a same-named type (${order.real} vs ${order.decoy})`, async () => {
          const layout: Layout = { ...order, scenario };
          const expected = expectedFile(layout, lang);
          expect(await callTargets(lang, layout)).toEqual(expected ? [expected] : []);
        });
        if (scenario !== 'decoyOnly' && order === orders[0]) {
          it(`${lang.name}: control without a same-named type keeps the edge (${scenario})`, async () => {
            const layout: Layout = { real: order.real, decoy: null, scenario };
            expect(await callTargets(lang, layout)).toEqual([expectedFile(layout, lang)]);
          });
        }
      }
    }
  }
});
