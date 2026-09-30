import { expect, it } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { CodeGraph } from '../src';

it('binds imported SQL-style extensions through declared property initializer returns', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-column-'));
  let cg: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'Api.kt'), `package api
open class Expression
open class ExpressionWithColumnType : Expression()
class Column : ExpressionWithColumnType()
class LiteralOp<T> : ExpressionWithColumnType()
fun stringLiteral(value: String): LiteralOp<String> = LiteralOp()
fun helper() { fun stringLiteral(value: String): String = value }
fun Expression.trim(): Expression = this
open class Table { fun varchar(name: String): Column = Column() }
fun Table.json(name: String): Column = Column()
fun ExpressionWithColumnType.count(): Int = 0
fun ExpressionWithColumnType.contains(value: String): Boolean = false
fun Expression.substring(start: Int, end: Int): Expression = this
class AbstractQuery { fun count(): Int = 0 }
class External { fun count(): Int = 0 }
`);
    fs.writeFileSync(path.join(dir, 'Tables.kt'), `package app
import api.Table
import api.json
object Users : Table() { val name = varchar("name") }
object Teams : Table() { val project = json("project") }
`);
    fs.writeFileSync(path.join(dir, 'Use.kt'), `package app
import api.count
import api.contains
import api.substring
import api.External
import api.stringLiteral
import api.trim
fun trimmed() = stringLiteral("  x  ").trim()
fun countNames() = Users.name.count()
fun hasProject() = Teams.project.contains("x")
fun shortened() = Users.name.substring(1, 2)
fun unrelated(value: External) = value.count()
`);
    cg = await CodeGraph.init(dir, { index: true });
    const targets = (name: string) => cg!.getOutgoingEdges(cg!.getNodesInFile('Use.kt').find((n) => n.name === name)!.id)
      .filter((e) => e.kind === 'calls').map((e) => cg!.getNode(e.target)!.qualifiedName);
    expect(targets('trimmed')).toContain('Expression::trim');
    expect(targets('countNames')).toEqual(['ExpressionWithColumnType::count']);
    expect(targets('hasProject')).toEqual(['ExpressionWithColumnType::contains']);
    expect(targets('shortened')).toEqual(['Expression::substring']);
    expect(targets('unrelated')).toEqual(['api::External::count']);
  } finally {
    cg?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
