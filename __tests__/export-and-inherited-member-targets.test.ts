/**
 * Three resolution paths that picked a declaration the program never reaches:
 *   - `setTimeout(this.create)` on a class that both implements an interface
 *     declaring `create` and inherits a `create` body links to the body;
 *   - a default export links to the exported declaration, not an earlier
 *     same-named function nested inside another one;
 *   - a re-export chain that comes back into a file for a different name
 *     still reaches that name.
 * Each case has a control that differs only in the trigger and must keep its
 * existing edge.
 */

import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';

let tempDir: string;
let cg: CodeGraph | null = null;

async function targets(
  files: Record<string, string>,
  fromName: string,
  edgeKind: 'calls' | 'references'
): Promise<string[]> {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-export-inherited-'));
  for (const [rel, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(tempDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tempDir, rel), source);
  }
  cg = await CodeGraph.init(tempDir, { index: true });
  cg.resolveReferences();
  const from = [...cg.getNodesByKind('function'), ...cg.getNodesByKind('method')].find((n) => n.name === fromName)!;
  expect(from).toBeDefined();
  return cg
    .getOutgoingEdges(from.id)
    .filter((e) => e.kind === edgeKind)
    .map((e) => cg!.getNode(e.target))
    .filter((n): n is NonNullable<typeof n> => !!n)
    .map((n) => `${n.kind}:${n.qualifiedName}@${n.filePath}:${n.startLine}`);
}

afterEach(() => {
  cg?.close();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

const profiles = (implementsClause: string) => ({
  'profiles.ts': [
    'export interface IProfiles { create(name: string): void; }',
    'export class BaseProfiles { create(name: string) { return name; } }',
    'export class ReadonlyProfiles extends BaseProfiles {}',
    `export class MainProfiles extends ReadonlyProfiles${implementsClause} {`,
    '  wire() { setTimeout(this.create); }',
    '}',
  ].join('\n'),
});

describe('this.member callback on a class with an interface and an inherited body', () => {
  it('links to the inherited implementation, not the interface declaration', async () => {
    expect(await targets(profiles(' implements IProfiles'), 'wire', 'references')).toEqual([
      'method:BaseProfiles::create@profiles.ts:2',
    ]);
  });

  it('control: without the interface the same body is linked', async () => {
    expect(await targets(profiles(''), 'wire', 'references')).toEqual(['method:BaseProfiles::create@profiles.ts:2']);
  });

  it('still links to the interface declaration when no class declares the member', async () => {
    const files = {
      'profiles.ts': [
        'export interface IProfiles { create(name: string): void; }',
        'export class MainProfiles implements IProfiles {',
        '  wire() { setTimeout(this.create); }',
        '}',
      ].join('\n'),
    };
    expect(await targets(files, 'wire', 'references')).toEqual(['method:IProfiles::create@profiles.ts:1']);
  });
});

const barrelUse = {
  'use.js': "import { Card } from './cards/index.js';\nexport function draw() { return Card(); }\n",
};

describe('default export with a same-named nested function', () => {
  const files = (nestedName: string) => ({
    ...barrelUse,
    'cards/index.js': "export { default as Card } from './card.js';\n",
    'cards/card.js': `function outer() { function ${nestedName}() { return 0; } }\nexport default function card() { return 1; }\n`,
  });

  it('links to the exported declaration', async () => {
    expect(await targets(files('card'), 'draw', 'calls')).toEqual(['function:card@cards/card.js:2']);
  });

  it('control: a differently named nested function changes nothing', async () => {
    expect(await targets(files('cardx'), 'draw', 'calls')).toEqual(['function:card@cards/card.js:2']);
  });
});

describe('re-export chain that re-enters a file for another name', () => {
  it('reaches the name in the re-entered file', async () => {
    const files = {
      ...barrelUse,
      'cards/index.js': "export { default as Card } from './card.js';\nexport function real() { return 1; }\n",
      'cards/card.js': "export { real as default } from './index.js';\n",
    };
    expect(await targets(files, 'draw', 'calls')).toEqual(['function:real@cards/index.js:2']);
  });

  it('control: the same chain through a third file', async () => {
    const files = {
      ...barrelUse,
      'cards/index.js': "export { default as Card } from './card.js';\n",
      'cards/card.js': "export { real as default } from './real.js';\n",
      'cards/real.js': 'export function real() { return 1; }\n',
    };
    expect(await targets(files, 'draw', 'calls')).toEqual(['function:real@cards/real.js:1']);
  });
});
