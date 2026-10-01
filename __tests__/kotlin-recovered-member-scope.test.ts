import { it, expect } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CodeGraph } from '../src';

it('keeps recovered member extensions in their dispatch scope and follows visible wildcard factory returns', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-recovered-scope-'));
  let cg: CodeGraph | undefined;
  const files: Record<string, string> = {
    'Entity.kt': `package dao
class Column<T>
class Op<T>
class Entity {
    fun adjust(body: Op<Boolean>?.() -> Op<Boolean>): Entity = this
    fun Column<String>.getValue(key: String): String = key
    fun Column<String>.inside(): String = getValue("x")
    fun empty(): Boolean = false
}
`,
    'Options.kt': `package app
val ExternalOptions.option: String
    get() = getValue("x")
`,
    'Jdbc.kt': `package jdbc
class Table
class Query {
    fun adjust(body: (() -> Unit)?.() -> Unit): Query = this
    fun where(body: () -> Unit): Query = this
    fun empty(): Boolean = false
}
fun Table.selectAll(): Query = Query()
`,
    'R2dbc.kt': `package r2dbc
class Table
class Query {
    fun where(body: () -> Unit): Query = this
    fun empty(): Boolean = false
}
fun Table.selectAll(): Query = Query()
`,
    'Unrelated.kt': `package other
class Rollback { fun selectAll(): List<String> = listOf() }
`,
    'UseJdbc.kt': `package app
import jdbc.*
fun runJdbc() { withTables { cities -> cities.selectAll().where { }.empty() } }
`,
    'UseR2dbc.kt': `package app
import r2dbc.*
fun runR2dbc() { withTables { cities -> cities.selectAll().where { }.empty() } }
`,
  };
  try {
    for (const [file, source] of Object.entries(files)) fs.writeFileSync(path.join(root, file), source);
    cg = await CodeGraph.init(root, { index: true });
    const calls = (file: string) => cg!.getOutgoingEdgesFrom(cg!.getNodesInFile(file).map(n => n.id)).filter(e => e.kind === 'calls');
    expect(calls('Options.kt').map(e => cg!.getNode(e.target)?.name)).not.toContain('getValue');
    expect(calls('Entity.kt').map(e => cg!.getNode(e.target)?.name)).toContain('getValue');
    for (const [file, expected] of [['UseJdbc.kt', 'Jdbc.kt'], ['UseR2dbc.kt', 'R2dbc.kt']]) {
      const edges = calls(file!).filter(e => cg!.getNode(e.target)?.name === 'empty');
      expect(edges.map(e => cg!.getNode(e.target)?.filePath)).toEqual([expected]);
      expect(Number(edges[0]!.metadata?.confidence)).toBeLessThanOrEqual(0.7);
    }
  } finally { cg?.close(); fs.rmSync(root, { recursive: true, force: true }); }
});

it('does not borrow an extension return when an inherited member shadows it', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-inherited-factory-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(root, 'Api.kt'), `package jdbc
open class Parent { fun selectAll(): OtherQuery = OtherQuery() }
class Table : Parent()
class Query { fun where(body: () -> Unit): Query = this; fun empty() = false }
class OtherQuery { fun where(body: () -> Unit): OtherQuery = this; fun empty() = false }
fun Table.selectAll(): Query = Query()
`);
    fs.writeFileSync(path.join(root, 'Use.kt'), `package app
import jdbc.Table
import jdbc.selectAll
fun run() { withTables { cities -> cities.selectAll().where { }.empty() } }
`);
    cg = await CodeGraph.init(root, { index: true });
    const edges = cg.getOutgoingEdgesFrom(cg.getNodesInFile('Use.kt').map(n => n.id)).filter(e => e.kind === 'calls');
    expect(edges.map(e => cg!.getNode(e.target)?.qualifiedName)).not.toContain('jdbc::Query::empty');
  } finally { cg?.close(); fs.rmSync(root, { recursive: true, force: true }); }
});

it('keeps visible collection, private String and inherited generic member extensions', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-visible-extension-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(root, 'Api.kt'), `package api
class Op<T>
class Column<T>
fun List<Op<Boolean>>.compoundAnd(): Op<Boolean> = Op()
open class Table {
    fun <T> Column<T>.defaultExpression(value: T): Column<T> = this
    fun datetime(name: String): Column<String> = Column()
}
`);
    fs.writeFileSync(path.join(root, 'Use.kt'), `package app
import api.Column
import api.Table
import api.Op
import api.compoundAnd
class BaseTable : Table() {
    fun configure() { datetime("stamp").defaultExpression("now") }
}
class Constraints {
    private fun String.asIndexStatement(): String = this
    fun createIndex(name: String): String = name
    fun configure() { createIndex("name").asIndexStatement() }
}
fun conjunction(values: List<Op<Boolean>>) { values.map { it }.compoundAnd() }
fun anonymousTable() {
    val datetime = "stamp"
    val table = object : Table() {
        val created = datetime(datetime).defaultExpression(datetime)
    }
}
`);
    cg = await CodeGraph.init(root, { index: true });
    const targets = cg.getOutgoingEdgesFrom(cg.getNodesInFile('Use.kt').map(n => n.id)).filter(e => e.kind === 'calls').map(e => cg!.getNode(e.target)?.name);
    expect(targets).toContain('defaultExpression');
    expect(targets).toContain('asIndexStatement');
    expect(targets).toContain('compoundAnd');
    const created = cg.getNodesInFile('Use.kt').find(n => n.name === 'anonymousTable')!;
    expect(cg.getOutgoingEdgesFrom(cg.getNodesInFile('Use.kt').map(n => n.id)).filter(e => e.kind === 'calls' && e.line! >= created.startLine && e.line! <= created.endLine).map(e => cg!.getNode(e.target)?.name)).toContain('defaultExpression');
  } finally { cg?.close(); fs.rmSync(root, { recursive: true, force: true }); }
});

it('retains unique ordinary member targets on unknown property chains below the trust line', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-unique-chain-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(root, 'Api.kt'), `package api
class ResponseBody { fun string(): String = "body" }
class RoutesConfig { fun apiBuilder(block: () -> Unit) {} }
`);
    fs.writeFileSync(path.join(root, 'Use.kt'), `package app
fun run(response: ExternalResponse, config: ExternalConfig, client: ExternalClient) {
    response.body.string()
    config.routes.apiBuilder { }
    client.get("/").body.string()
}
`);
    cg = await CodeGraph.init(root, { index: true });
    const caller = cg.getNodesInFile('Use.kt').find(n => n.name === 'run')!;
    const edges = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls');
    expect(edges.filter(e => cg!.getNode(e.target)?.name === 'string')).toHaveLength(2);
    for (const name of ['string', 'apiBuilder']) {
      const edge = edges.find(e => cg!.getNode(e.target)?.name === name);
      expect(edge).toBeDefined();
      expect(Number(edge!.metadata?.confidence)).toBeLessThanOrEqual(0.7);
    }
  } finally { cg?.close(); fs.rmSync(root, { recursive: true, force: true }); }
});


it('follows cast, smart-cast and filtered collection element evidence without borrowing another lambda', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-element-types-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), `package api
class Expression
interface Alias<T> { val delegate: Expression; fun aliasOnlyExpression(): Expression = delegate }
class ExpressionAlias(val delegate: Expression) : Alias<String>
fun Expression.alias(name: String) = ExpressionAlias(this)
class Other { fun aliasOnlyExpression() {} }
fun sameLine(value: Any) { when (value) { is Alias<*> -> { val value = Other(); value.aliasOnlyExpression() }; else -> Unit } }
fun casts(value: Any) { (value as? Alias<*>)?.aliasOnlyExpression() }
fun narrowed(value: Any) { when (value) { is Alias<*> -> value.delegate.alias("x").aliasOnlyExpression(); else -> Unit } }
fun filtered(values: List<Any>) {
    val aliases = values.filterIsInstance<Alias<String>>()
    aliases.find { true }?.let { it.delegate.alias("x").aliasOnlyExpression() }
    aliases.find { true }?.aliasOnlyExpression()
}
fun alternatives(value: Any) { when (value) { is Alias<*>, is Other -> value.aliasOnlyExpression(); else -> value.aliasOnlyExpression() } }
fun nested(values: List<Any>) {
    val aliases = values.filterIsInstance<Alias<String>>()
    aliases.find { true }?.let { unknown { it.aliasOnlyExpression() } }
}
class Custom { fun <T> filterIsInstance(): List<Other> = listOf() }
fun custom(values: Custom) { val aliases = values.filterIsInstance<Alias<String>>(); aliases.find { true }?.aliasOnlyExpression() }
`);
    cg = await CodeGraph.init(dir, { index: true });
    const calls = (name: string) => {
      const caller = cg!.getNodesInFile('Api.kt').find(n => n.name === name)!;
      return cg!.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls' && cg!.getNode(e.target)?.qualifiedName.includes('Alias::aliasOnlyExpression'));
    };
    expect(calls('casts')).toHaveLength(1);
    expect(calls('narrowed')).toHaveLength(1);
    // The custom filter declaration applies to Custom only, never the List receiver.
    expect(calls('filtered')).toHaveLength(2);
    for (const name of ['alternatives', 'nested', 'custom', 'sameLine']) expect(calls(name)).toHaveLength(0);
  } finally { cg?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});

it('binds a generic factory result to its declared key argument and rejects conflicting overloads', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-generic-key-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Key.kt'), `package keys
class Key<T>
`);
    fs.writeFileSync(path.join(dir, 'Executor.kt'), `package executor
import keys.Key
class Executor {
    companion object { val ExecutorKey = Key<Executor>() }
    fun submitAsyncTask() {}
}
class Other
`);
    fs.writeFileSync(path.join(dir, 'Context.kt'), `package app
import keys.Key
import executor.Executor.Companion.ExecutorKey
import executor.Other
class T { fun submitAsyncTask() {} }
interface Context {
    fun <T> appData(key: Key<T>): T
    fun async() = appData(ExecutorKey).submitAsyncTask()
    fun shadowed() { val ExecutorKey = Key<Other>(); appData(ExecutorKey).submitAsyncTask() }
}
interface Conflicting {
    fun <T> appData(key: Key<T>): T
    fun appData(key: Key<Other>): Other
    fun async() = appData(ExecutorKey).submitAsyncTask()
}
`);
    cg = await CodeGraph.init(dir, { index: true });
    const calls = (qualified: string) => {
      const caller = cg!.getNodesInFile('Context.kt').find(n => n.qualifiedName.endsWith(qualified))!;
      return cg!.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls' && cg!.getNode(e.target)?.name === 'submitAsyncTask');
    };
    expect(calls('Context::async').map(e => [cg!.getNode(e.target)?.qualifiedName, cg!.getNode(e.target)?.filePath])).toEqual([['executor::Executor::submitAsyncTask', 'Executor.kt']]);
    expect(calls('Context::shadowed')).toHaveLength(0);
    expect(calls('Conflicting::async')).toHaveLength(0);
  } finally { cg?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});


it('does not carry stdlib element or lambda types through a visible custom find or let', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-custom-scope-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), `package api
class Alias { fun actual() {} }
class Other { fun actual() {} }
fun make(): Alias = Alias()
`);
    fs.writeFileSync(path.join(dir, 'Use.kt'), `package app
import api.*
fun Alias.let(block: (Other) -> Unit) = block(Other())
fun <T> List<T>.find(predicate: (T) -> Boolean): Other = Other()
fun customLet() { make().let { it.actual() } }
fun customFind(values: List<Any>) { val aliases = values.filterIsInstance<Alias>(); aliases.find { true }?.actual() }
`);
    cg = await CodeGraph.init(dir, { index: true });
    for (const name of ['customLet', 'customFind']) {
      const caller = cg.getNodesInFile('Use.kt').find(n => n.name === name)!;
      const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg!.getNode(e.target)?.qualifiedName);
      expect(targets).not.toContain('api::Alias::actual');
    }
  } finally { cg?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});


it('does not bind an unsupported generic field parameter to a same-named project class', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-generic-field-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), `package api
class T { fun finish() {} }
class Other { fun finish() {} }
interface Box<T> { val item: T }
fun run(box: Box<Other>) = box.item.finish()
`);
    cg = await CodeGraph.init(dir, { index: true });
    const caller = cg.getNodesInFile('Api.kt').find(n => n.name === 'run')!;
    const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg!.getNode(e.target)?.qualifiedName);
    expect(targets).not.toContain('api::T::finish');
  } finally { cg?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});
