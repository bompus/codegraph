/**
 * A Kotlin DSL lambda's receiver is typed by the function it is passed to,
 * which can mean reading a local bound to a call. That lookup asked for the
 * same file's block frames while they were still being built, so the scan
 * re-entered itself until the stack was gone and `codegraph index` died with
 * SIGSEGV — javalin's TestCompression.kt (a property `buildSampleJson(500)`
 * read through `sampleJson10k.map { … }`) and JetBrains Exposed both crashed.
 *
 * The chain, kept minimal here: a property initialised by a call to a function
 * of the same class, read as a qualified call with a trailing lambda from a
 * member of that class. The frame scan now stands an empty set in while it
 * runs, so typing the receiver during the scan terminates.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let dir = '';
let cg: CodeGraph;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-frame-cycle-'));
  fs.writeFileSync(
    path.join(dir, 'Screen.kt'),
    `package p

class Screen {
    private fun buildItems(): String = "x"

    private val items = buildItems()

    fun render() {
        items.map { it }
    }
}
`
  );
  cg = await CodeGraph.init(dir, { index: true });
});

afterAll(() => {
  cg?.close();
  fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
});

describe('a Kotlin frame scan that types a property from its call', () => {
  it('indexes the file instead of recursing until the process dies', () => {
    const nodes = cg.getNodesInFile('Screen.kt');
    const named = nodes.map((n) => `${n.kind} ${n.name}`).sort();
    expect(named).toContain('class Screen');
    expect(named).toContain('method buildItems');
    expect(named).toContain('method render');
    expect(nodes.some((n) => n.name === 'items')).toBe(true);
  });

  it('keeps the call the property is initialised with', () => {
    const build = cg.getNodesInFile('Screen.kt').find((n) => n.name === 'buildItems')!;
    const callers = cg.getIncomingEdges(build.id).filter((e) => e.kind === 'calls');
    expect(callers.length).toBeGreaterThanOrEqual(1);
  });
});
