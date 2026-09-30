/**
 * Two Kotlin fixes that travel together:
 * - a primary constructor on the line after its class name with a supertype
 *   list (`expect open class ByteString` / `internal constructor(…) :
 *   Comparable<ByteString> {`, Kotlin Multiplatform's shape) dropped the class
 *   and scattered its members as loose functions; the constructor's modifiers
 *   and keyword are blanked (offsets kept) so the class parses;
 * - with members back on their classes, a standard-library name on an
 *   untyped chain link (`builder.apply { … }`, `"x".toPath().toString()`)
 *   needs a receiver named after the owner — okhttp's `.apply { }` went to
 *   `ConnectionSpec.apply`.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { joinKotlinSplitConstructors } from '../src/extraction/languages/kotlin';

let root = '';
let cg: CodeGraph;

const BYTE_STRING = `package okio

expect open class ByteString
// Trusted internal constructor doesn't clone data.
internal constructor(data: ByteArray) : Comparable<ByteString> {
  fun hex(): String
  fun toByteArray(): ByteArray
}
`;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-split-'));
  const files: Record<string, string> = {
    'src/main/kotlin/app/Validator.kt': `package app
class Context
class Validator {
  fun getOrNull(): String? = null
  fun getOrDefault(value: String): String = value
}
fun <T> Context.queryParamAsClass(key: String): Validator = Validator()
class Other {
  fun queryParamAsClass(key: String): String = key
}
`,
    'src/main/kotlin/consumer/Validation.kt': `package consumer
import app.Context
import app.Other
import app.queryParamAsClass
fun positive(ctx: Context) = ctx.queryParamAsClass<String>("x").getOrNull()
fun negative(ctx: Other) = ctx.queryParamAsClass("x").getOrDefault("default")
`,
    'src/commonMain/kotlin/okio/ByteString.kt': BYTE_STRING,
    'src/main/kotlin/app/ConnectionSpec.kt': `package app

class ConnectionSpec {
  fun apply(sslSocket: Any, isFallback: Boolean) {}
}
`,
    'src/main/kotlin/app/Builder.kt': `package app

class Builder {
  var name = ""
  fun build(): String = StringBuilder().apply { append(name) }.toString()
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

describe('Kotlin split primary constructors and standard chain calls', () => {
  it('keeps the class and its members', () => {
    const nodes = cg.getNodesInFile('src/commonMain/kotlin/okio/ByteString.kt');
    const hex = nodes.find((n) => n.name === 'hex')!;
    expect(hex.qualifiedName).toBe('okio::ByteString::hex');
    const out = joinKotlinSplitConstructors(BYTE_STRING);
    expect(out.length).toBe(BYTE_STRING.length);
    expect(out.split('\n')[4]).toMatch(/^\s+\(data: ByteArray\) : Comparable<ByteString> \{$/);
  });

  it('never sends a scope function to a project type', () => {
    const build = cg.getNodesInFile('src/main/kotlin/app/Builder.kt').find((n) => n.name === 'build')!;
    const targets = cg.getOutgoingEdges(build.id).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
    expect(targets.some((t) => t.endsWith('ConnectionSpec::apply'))).toBe(false);
  });
});

it('preserves each newline when blanking annotated constructor modifiers', () => {
  const source = 'expect open class ByteString\r\n@Inject\r\ninternal constructor(data: ByteArray) : Comparable<ByteString> {\r\n  fun hex(): String\r\n}\r\n';
  const out = joinKotlinSplitConstructors(source);
  expect(out.length).toBe(source.length);
  expect([...out.matchAll(/\r|\n/g)].map((m) => m.index)).toEqual([...source.matchAll(/\r|\n/g)].map((m) => m.index));
});

it('keeps imported extension return-type chains and rejects a different receiver', () => {
  const nodes = cg.getNodesInFile('src/main/kotlin/consumer/Validation.kt');
  const targets = (name: string) => cg.getOutgoingEdges(nodes.find((n) => n.name === name)!.id).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
  expect(targets('positive')).toContain('app::Validator::getOrNull');
  expect(targets('negative')).not.toContain('app::Validator::getOrDefault');
});
