import { expect, it } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CodeGraph } from '../src';

// An extension function's qualified name is `Foo::shout` whatever its package, while the class
// it extends is `pkg::Foo`: the call on a typed receiver has to find it by the receiver's type.
it('links a call on a typed receiver to an extension function declared in a package', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-ext-pkg-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Foo.kt'), `package demo

class Foo
class Bar
class Baz { fun shout(): String = "member" }
`);
    fs.writeFileSync(path.join(dir, 'Ext.kt'), `package demo

fun Foo.shout(): String = "x"
infix fun Foo.join(other: Foo): Foo = this
fun Baz.shout(loud: Boolean): String = "extension"
fun Bar.whisper(): String = "y"
`);
    fs.writeFileSync(path.join(dir, 'Use.kt'), `package demo

fun byParameter(value: Foo): String = value.shout()
fun byConstructor(): String {
    val f = Foo()
    return f.shout()
}
fun byInfix(a: Foo, b: Foo): Foo = a join b
fun memberWins(value: Baz): String = value.shout()
fun otherType(value: Bar): String = value.shout()
fun unrelatedExtension(value: Foo): String = value.whisper()
`);
    cg = await CodeGraph.init(dir, { index: true });
    const targets = (name: string) => cg!.getOutgoingEdges(cg!.getNodesInFile('Use.kt').find((n) => n.name === name)!.id)
      .filter((e) => e.kind === 'calls').map((e) => cg!.getNode(e.target)!.qualifiedName + '@' + cg!.getNode(e.target)!.filePath);
    expect(targets('byParameter')).toEqual(['Foo::shout@Ext.kt']);
    expect(targets('byConstructor')).toEqual(['Foo::shout@Ext.kt']);
    expect(targets('byInfix')).toEqual(['Foo::join@Ext.kt']);
    expect(targets('memberWins')).toEqual(['demo::Baz::shout@Foo.kt']);
    expect(targets('otherType')).toEqual([]);
    expect(targets('unrelatedExtension')).toEqual([]);
  } finally {
    cg?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
