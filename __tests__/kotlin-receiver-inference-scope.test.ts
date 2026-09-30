import { expect, it } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CodeGraph } from '../src';

it.each([
  ['shadowed factory', `package api
open class Expression
class Column : Expression()
class External { fun count(): Int = 0 }
fun createColumn(): Column = Column()
fun Expression.count(): Int = 0
`, `package app
import api.createColumn
import api.count
import api.External
fun run(createColumn: () -> External) = createColumn().count()
`, 'Expression::count'],
  ['shadowed apply', `package api
class Other
class Validator {
  fun check(apply: (() -> Unit) -> Other) = apply { }
  fun getOrNull(): String? = null
}
`, `package app
import api.Validator
import api.Other
fun run(value: Validator) = value.check { Other() }.getOrNull()
`, 'api::Validator::getOrNull'],
  ['inherited receiver return', `package api
open class Base { fun self() = this }
class Child : Base() { fun count(): Int = 1 }
fun Base.count(): Int = 0
`, `package app
import api.Child
import api.count
fun run(value: Child) = value.self().count()
`, 'api::Child::count'],
  ['overloaded apply', `package api
class Other
class Validator {
  fun apply(): Validator = this
  fun apply(block: () -> Unit): Other = Other()
  fun check() = apply { }
  fun getOrNull(): String? = null
}
`, `package app
import api.Validator
fun run(value: Validator) = value.check().getOrNull()
`, 'api::Validator::getOrNull'],
  ['labeled this', `package api
class Outer {
  fun getOrNull(): String? = null
  inner class Inner {
    fun parent() = this@Outer
    fun getOrNull(): String? = null
  }
}
`, `package app
import api.Outer
fun run(value: Outer.Inner) = value.parent().getOrNull()
`, 'api::Outer::Inner::getOrNull'],
])('does not invent a receiver type through %s', async (_label, api, use, unexpected) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-receiver-shadow-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), api);
    fs.writeFileSync(path.join(dir, 'Use.kt'), use);
    cg = await CodeGraph.init(dir, { index: true });
    const caller = cg.getNodesInFile('Use.kt').find((n) => n.name === 'run')!;
    const targets = cg.getOutgoingEdges(caller.id).filter((e) => e.kind === 'calls').map((e) => cg!.getNode(e.target)!.qualifiedName);
    expect(targets).not.toContain(unexpected);
    if (_label === 'inherited receiver return') expect(targets).toContain('Base::count');
  } finally {
    cg?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

it('carries an imported return-type hypothesis through fluent calls below the trust line', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-fluent-confidence-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), `package api
class Context
class Validator {
  fun check(value: Boolean) = apply { }
  fun copy(): Validator = this
  fun getOrNull(): String? = null
}
fun <T> Context.query(key: String): Validator = Validator()
`);
    fs.writeFileSync(path.join(dir, 'Use.kt'), `package app
import api.Context
import api.query
fun use(handler: (Context) -> Unit) {}
fun run() { use { it.query<String>("x").check(true).copy().getOrNull() } }
`);
    cg = await CodeGraph.init(dir, { index: true });
    const caller = cg.getNodesInFile('Use.kt').find((n) => n.name === 'run')!;
    const edge = cg.getOutgoingEdges(caller.id).find((e) => e.kind === 'calls' && cg!.getNode(e.target)!.qualifiedName === 'api::Validator::getOrNull');
    expect(edge).toBeDefined();
    expect(edge!.metadata?.confidence).toBeLessThanOrEqual(0.7);
  } finally {
    cg?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
