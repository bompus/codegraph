/**
 * A Kotlin method whose extracted name leaves out its companion object
 * (`app::Validation::installMapper`) is reached through an import of
 * `app.Validation.Companion.installMapper` only when its source sits in a
 * `companion object` body. The resolver reads that from declaration heads; it
 * types DSL lambda blocks too only when some source spells `Companion.(`, the
 * one lambda type that could name `Companion` as a receiver.
 */
import { describe, it, expect, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
const graphs: CodeGraph[] = [];

afterAll(() => {
  for (const cg of graphs) cg.close();
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

const sources: Record<string, string> = {
  'src/main/kotlin/app/Validation.kt': `package app
fun build(block: Builder.() -> Unit) { }
class Builder
class Validation {
    companion object {
        fun installMapper() { }
    }
    fun plain() { }
}
`,
  'src/main/kotlin/consumer/Use.kt': `package consumer
import app.Validation.Companion.installMapper
import app.Validation.Companion.plain
fun configure() {
    installMapper()
    plain()
}
`,
};

async function index(extra: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-companion-'));
  roots.push(root);
  for (const [rel, content] of Object.entries({ ...sources, ...extra })) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  const cg = await CodeGraph.init(root, { index: true });
  graphs.push(cg);
  return cg;
}

function callsFrom(cg: CodeGraph, file: string): string[] {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg.getNode(e.target)!.qualifiedName)
    .sort();
}

describe('Kotlin companion imports', () => {
  it('reach a companion member and not a plain member', async () => {
    const cg = await index({});
    expect(callsFrom(cg, 'src/main/kotlin/consumer/Use.kt')).toEqual(['app::Validation::installMapper']);
  });

  it('resolve the same when a lambda type names Companion as its receiver', async () => {
    const cg = await index({
      'src/main/kotlin/app/Dsl.kt': `package app
class Companion
fun companionScope(block: Companion.() -> Unit) { }
`,
    });
    expect(callsFrom(cg, 'src/main/kotlin/consumer/Use.kt')).toEqual(['app::Validation::installMapper']);
  });
});
