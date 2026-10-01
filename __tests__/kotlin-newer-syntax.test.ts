/**
 * Kotlin syntax newer than the vendored grammar — `when` guards (2.1), `..<`
 * ranges (1.9), multi-dollar strings (2.1), nullable function-type
 * receivers — is rewritten to an older equivalent of the same length before
 * the parse; otherwise error recovery dropped the class around it (Exposed's
 * dialect classes came out as loose functions).
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { rewriteNewerKotlinSyntax } from '../src/extraction/languages/kotlin';

const SOURCE = `package app

class Dialect(private val mode: Int) {
    fun sql(value: Any?): String = when (value) {
        is String if mode == 2 -> "text"
        is Int if (
            mode > 0
        ) -> "int"
        is Long -> if (mode == 1) "long" else "big"
        else -> "other"
    }

    fun ranges(n: Int) {
        for (i in 0..<n) println(i)
    }

    fun literal() = $$"\${after()}"
    fun interpolated() = $$"$\${after()}"

    fun path(team: String) = $$"?(@.user.team == $team)"

    fun adjust(body: Op<Boolean>?.() -> Op<Boolean>): Dialect = this

    fun after(): Int = 1
}

class Op<T>
`;

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-newer-'));
  fs.mkdirSync(path.join(root, 'src'), { recursive: true });
  fs.writeFileSync(path.join(root, 'src/Dialect.kt'), SOURCE);
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('newer Kotlin syntax', () => {
  it('preserves lookalike syntax in comments and string contents', () => {
    const text = '// 0..<n and Op<Boolean>?.() -> Op<Boolean>\n/* outer /* 1..<n */ 2..<n */\nval literal = "0..<n Op<Boolean>?.() -> Op<Boolean>"\n';
    expect(rewriteNewerKotlinSyntax(text)).toBe(text);
  });
  it('normalizes tab-separated guards without relying on another guard', () => {
    expect(rewriteNewerKotlinSyntax('is String\tif mode == 2 -> "text"')).toBe('is String,   mode == 2 -> "text"');
  });
  it('rewrites the guard after a comment and preserves its contents', () => {
    const text = '        is String /* note if ignored */ if mode == 2 -> "text"';
    expect(rewriteNewerKotlinSyntax(text)).toBe('        is String /* note if ignored */,   mode == 2 -> "text"');
  });
  it('keeps multi-dollar interpolation semantics', () => {
    const edges = cg.getOutgoingEdgesFrom(cg.getNodesInFile('src/Dialect.kt').map(n => n.id)).filter(e => e.kind === 'calls');
    const callers = edges.filter(e => cg.getNode(e.target)?.name === 'after').map(e => cg.getNode(e.source)?.name);
    expect(callers).not.toContain('literal');
    expect(callers).toContain('interpolated');
  });
  it('is rewritten with every offset intact', () => {
    const out = rewriteNewerKotlinSyntax(SOURCE);
    expect(out.length).toBe(SOURCE.length);
    expect(out).toContain('is String,   mode == 2 ->');
    expect(out).toContain('is Long -> if (mode == 1)');
    expect(out).toContain('0.. n');
    expect(out).toContain('  "?(@.user.team ==  team)"');
    expect(out).toContain('Op<Boolean>.( ) -> Op<Boolean>');
  });

  it('keeps the class around it and every method', () => {
    const methods = cg.getNodesInFile('src/Dialect.kt')
      .filter((n) => n.kind === 'method')
      .map((n) => n.qualifiedName)
      .sort();
    expect(methods).toEqual([
      'app::Dialect::adjust',
      'app::Dialect::after',
      'app::Dialect::path',
      'app::Dialect::literal',
      'app::Dialect::interpolated',
      'app::Dialect::ranges',
      'app::Dialect::sql',
    ].sort());
  });
});
