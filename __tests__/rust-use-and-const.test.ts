/**
 * Rust `use` bindings and `const` initializers.
 *
 * - A function brought in by `use` and passed as a callback
 *   (`register(handler)`) gets its function-ref edge, like a local one.
 * - `use a::b as c;` records its import like any other `use`.
 * - `use crate::…` adds no import of a module named `crate`.
 * - `const MAX: u32 = OTHER;` declares MAX only; OTHER is a read.
 */
import { describe, it, expect, afterEach, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { CodeGraph } from '../src';
import { extractFromSource } from '../src/extraction';
import { initGrammars, loadAllGrammars } from '../src/extraction/grammars';

beforeAll(async () => {
  await initGrammars();
  await loadAllGrammars();
});

describe('Rust use bindings and const initializers', () => {
  let cg: CodeGraph | undefined;
  let dir: string;

  afterEach(() => {
    cg?.destroy();
    cg = undefined;
    fs.rmSync(dir, { recursive: true, force: true });
  });

  const index = async (files: Record<string, string>): Promise<CodeGraph> => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-rust-use-'));
    for (const [name, content] of Object.entries(files)) fs.writeFileSync(path.join(dir, name), content);
    cg = CodeGraph.initSync(dir);
    await cg.indexAll();
    return cg;
  };

  it('links imported callbacks and aliased imports, and imports no `crate` module', async () => {
    const g = await index({
      'handlers.rs': 'pub fn handler() {}\npub fn other() {}\npub fn grouped() {}\n',
      'lib.rs': [
        'mod handlers;',
        'use crate::handlers::handler;',
        'use crate::handlers::other as aliased;',
        'use crate::handlers::{grouped};',
        'fn register(f: fn()) {}',
        'fn wire() {',
        '    register(handler);',
        '    register(grouped);',
        '}',
      ].join('\n'),
    });
    const fn = (name: string) => g.getNodesByKind('function').find((n) => n.name === name)!;
    const refsFrom = (name: string) => g.getOutgoingEdges(fn(name).id)
      .filter((e) => e.kind === 'references' && e.metadata?.fnRef === true)
      .map((e) => g.getNode(e.target)!.name);
    expect(refsFrom('wire').sort()).toEqual(['grouped', 'handler']);

    const file = g.getNodesByKind('file').find((n) => n.name === 'lib.rs')!;
    const imported = g.getOutgoingEdges(file.id).filter((e) => e.kind === 'imports')
      .map((e) => g.getNode(e.target)!.name);
    expect(imported).toContain('other');
    const extracted = extractFromSource('lib.rs', fs.readFileSync(path.join(dir, 'lib.rs'), 'utf8'));
    expect(extracted.unresolvedReferences.filter((r) => r.referenceName === 'crate')).toEqual([]);
  });

  it('declares only the const name; the initializer is a read', async () => {
    const g = await index({ 'lib.rs': 'const OTHER: u32 = 1;\nconst MAX: u32 = OTHER;\nfn read() -> u32 { OTHER }\n' });
    const others = g.getNodesByName('OTHER').filter((n) => n.filePath === 'lib.rs');
    expect(others.map((n) => n.startLine)).toEqual([1]);
  });
});
