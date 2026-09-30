/**
 * A Kotlin infix call — `Users.id eq id1`, koin's `single { … } bind I::class`
 * — is a call of the infix function; nothing recorded it before, so a
 * project's infix DSL had no callers (Exposed's `eq` alone is called over a
 * thousand times). A standard-library infix name on an expression receiver
 * (`alias(libs.x) apply false` in a build script) is not a project method's.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-infix-calls-'));
  const files: Record<string, string> = {
    'src/main/kotlin/app/Ops.kt': `package app

class Column {
    infix fun eq(other: Int): Boolean = true
}

class Definition {
    infix fun bind(type: Any): Definition = this
}

fun single(block: () -> Any): Definition = Definition()

class Ops(private val id: Column) {
    fun same(id1: Int): Boolean = id eq id1

    fun wire() {
        single { Ops(Column()) } bind Ops::class
    }
}

class Spec {
    fun apply(block: Spec.() -> Unit): Spec = this
}
`,
    'src/main/kotlin/app/Bitwise.kt': `package app
infix fun Int.and(other: Int): Int = this
class Bits {
    infix fun and(other: Bits): Bits = this
}
fun numeric(value: Int, mask: Int) = value and mask
fun custom(value: Bits, mask: Bits) = value and mask
`,
    'build.gradle.kts': `plugins {
    alias(libs.plugins.kotlin.jvm) apply false
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

/** `Owner::member` of every call edge out of a file. */
function callsFrom(file: string): string[] {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg.getNode(e.target)!.qualifiedName)
    .sort();
}

describe('Kotlin infix calls', () => {
  it('call the infix function', () => {
    const calls = callsFrom('src/main/kotlin/app/Ops.kt');
    expect(calls).toContain('app::Column::eq');
    expect(calls).toContain('app::Definition::bind');
  });

  it('keeps custom infix methods while refusing numeric bitwise extension guesses', () => {
    const nodes = cg.getNodesInFile('src/main/kotlin/app/Bitwise.kt');
    const targets = (name: string) => cg.getOutgoingEdges(nodes.find((n) => n.name === name)!.id)
      .filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
    expect(targets('numeric')).not.toContain('Int::and');
    expect(targets('custom')).toContain('app::Bits::and');
  });

  it('leave a standard infix name on an expression to the library', () => {
    expect(callsFrom('build.gradle.kts')).not.toContain('app::Spec::apply');
  });
});
