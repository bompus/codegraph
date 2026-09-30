/**
 * What a Kotlin or Java lambda's parameter is typed as, when the resolver
 * types it from the call the lambda is given to.
 *
 * - `w.let { it.paint() }` types `it` as `w`'s type only when `let` is the
 *   stdlib one. An extension `fun Widget.let(…)`, in this file or another,
 *   passes its own value (like a member `let` already did).
 * - A lambda given to the stdlib `run`, `apply` or `with` binds no `it`, so an
 *   enclosing lambda's `it` shows through. A function the project declares
 *   under one of those names is the callee instead, and its lambda binds `it`.
 * - Java `(Foo f) -> f.bar()` declares `f`'s type; it wins over a field `f`.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];

async function project(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-jvm-lambda-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

afterAll(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

/** `caller -> target` for every call edge whose target is named `name`. */
function callsTo(cg: CodeGraph, name: string): string[] {
  return cg
    .getNodesByKind('method')
    .filter((n) => n.name === name)
    .flatMap((t) => cg.getIncomingEdges(t.id).filter((e) => e.kind === 'calls').map((e) => `${cg.getNode(e.source)!.name} -> ${t.qualifiedName}`))
    .sort();
}

const WIDGETS = `package app

class Widget {
    fun paint() {}
}

class Gadget {
    fun paint() {}
}
`;

describe('Kotlin scope functions', () => {
  let cg: CodeGraph;
  beforeAll(async () => {
    cg = await project({
      'src/app/Widgets.kt': WIDGETS,
      // Extensions named like the stdlib scope functions, in another file.
      'src/app/Ext.kt': `package app

fun Widget.let(block: (Gadget) -> Unit) { block(Gadget()) }
fun Widget.run(block: (Widget) -> Unit) { block(this) }
fun with(block: (Widget) -> Unit) { block(Widget()) }
`,
      'src/app/Use.kt': `package app

fun extensionLet(w: Widget) {
    w.let { it.paint() }
}

fun extensionRun(w: Widget, g: Gadget) {
    g.also {
        w.run { it.paint() }
    }
}

fun topLevelWith(g: Gadget) {
    g.also {
        with { it.paint() }
    }
}
`,
      // Controls: the stdlib reading, on a type with no extension of its own.
      'src/app/Std.kt': `package app

fun stdAlso(g: Gadget) {
    g.also { it.paint() }
}

fun stdApply(w: Widget, g: Gadget) {
    g.also {
        w.apply { it.paint() }
    }
}

fun stdWithArgs(w: Widget, g: Gadget) {
    g.also {
        kotlin.with(w) { it.paint() }
    }
}
`,
    });
  });
  afterAll(() => cg.close());

  it('types `it` only where the stdlib function is the callee', () => {
    expect(callsTo(cg, 'paint')).toEqual([
      'stdAlso -> app::Gadget::paint',
      'stdApply -> app::Gadget::paint',
      'stdWithArgs -> app::Gadget::paint',
    ]);
  });
});

describe('a typed Java lambda parameter', () => {
  it('types its receiver, over a same-named field; an untyped one stays unresolved', async () => {
    const cg = await project({
      'src/app/Foo.java': `package app;
public class Foo { public void bar() {} }
`,
      'src/app/Baz.java': `package app;
public class Baz { public void bar() {} }
`,
      'src/app/Use.java': `package app;
import java.util.function.Consumer;
public class Use {
    Baz f = new Baz();
    void typed() {
        Consumer<Foo> c = (Foo f) -> f.bar();
    }
    void untyped() {
        Consumer<Foo> c = f -> f.bar();
    }
    void field() {
        f.bar();
    }
}
`,
    });
    try {
      expect(callsTo(cg, 'bar')).toEqual(['field -> app::Baz::bar', 'typed -> app::Foo::bar']);
    } finally {
      cg.close();
    }
  });
});
