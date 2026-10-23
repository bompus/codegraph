/**
 * TypeScript `type` modifiers on imports and re-exports.
 *
 * `import { type Cfg }` recorded the local name `"type Cfg"`, so the name
 * never matched and a reference to `Cfg` fell through to name-matching —
 * which picked whichever `Cfg` came first, often the wrong file.
 * `import type { Cfg }` recorded a bogus default import named `type`;
 * `import type X from` / `import type * as ns from` were not recognised at
 * all; and `export { type Foo } from` / `export type { Foo } from` dropped the
 * re-export, breaking barrel resolution for types.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
describe('a type imported with a `type` modifier resolves to its declaration', () => {
  let dir: string;
  let cg: CodeGraph;

  beforeAll(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-ts-type-mod-'));
    const w = (rel: string, text: string) => {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), text);
    };
    // A same-named decoy that name-matching alone would pick first.
    w('app/cfg-local.ts', 'export interface Cfg { local: number }\n');
    w('lib/cfg.ts', 'export interface Cfg { real: number }\n');
    w('lib/index.ts', "export type { Cfg } from './cfg';\n");
    w('app/inline.ts', "import { type Cfg } from '../lib/cfg';\nexport function inline(c: Cfg) { return c; }\n");
    w('app/barrel.ts', "import type { Cfg } from '../lib';\nexport function barrel(c: Cfg) { return c; }\n");
    // `$` / non-ASCII names, same-named in two files: the import decides.
    const dollar = 'export function $fetch() {}\nexport function user$() {}\nexport function café() {}\n';
    w('fx/a.ts', dollar);
    w('fx/b.ts', dollar);
    w('fx/main.ts', "import { $fetch, user$, café } from './b';\nexport function main() { $fetch(); user$(); café(); }\n");
    cg = await CodeGraph.init(dir, { index: true });
  });

  afterAll(() => {
    cg?.destroy();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  const typeRefs = (fn: string) => {
    const node = cg.getNodesByName(fn).find((n) => n.kind === 'function');
    expect(node).toBeDefined();
    return cg
      .getCallees(node!.id)
      .filter(({ node: t }) => t.name === 'Cfg')
      .map(({ node: t }) => t.filePath);
  };

  it('`$` and non-ASCII named imports bind to the file they are imported from', () => {
    const main = cg.getNodesByName('main').find((n) => n.kind === 'function' && n.filePath === 'fx/main.ts');
    expect(main).toBeDefined();
    const targets = cg.getCallees(main!.id).map(({ node }) => `${node.name}@${node.filePath}`).sort();
    expect(targets).toEqual(['$fetch@fx/b.ts', 'café@fx/b.ts', 'user$@fx/b.ts']);
  });

  it('`import { type Cfg }` → lib/cfg.ts', () => {
    expect(typeRefs('inline')).toEqual(['lib/cfg.ts']);
  });

  it('`import type { Cfg }` through a type-only barrel → lib/cfg.ts', () => {
    expect(typeRefs('barrel')).toEqual(['lib/cfg.ts']);
  });
});
