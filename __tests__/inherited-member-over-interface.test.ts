/**
 * A method call on a class that inherits the method through its superclass
 * chain and also implements an interface declaring it binds to the inherited
 * class method: that body is the one that runs. A breadth-first supertype
 * walk reached the owner's interface one level before the deeper superclass
 * and answered with the bodiless declaration.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let dir: string;
let cg: CodeGraph;

const files: Record<string, string> = {
  'src/profiles.ts':
    'export interface IProfiles { create(name: string): void; }\n' +
    'export class BaseProfiles { create(name: string) { return name; } }\n' +
    'export class ReadonlyProfiles extends BaseProfiles {}\n' +
    'export class MainProfiles extends ReadonlyProfiles implements IProfiles {}\n',
  'src/use.ts':
    "import { MainProfiles } from './profiles';\n" +
    'export function run(profiles: MainProfiles) { profiles.create(\'x\'); }\n',
};

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-inherited-'));
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  }
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
});

afterAll(() => {
  cg.destroy();
  fs.rmSync(dir, { recursive: true, force: true });
});

describe('inherited member over interface declaration', () => {
  it('binds to the superclass method, not the interface declaration', () => {
    const run = cg.getNodesByName('run').find((n) => n.filePath.endsWith('use.ts'))!;
    const callees = cg.getCallees(run.id)
      .filter(({ edge }) => edge.kind === 'calls')
      .map(({ node: n }) => n.qualifiedName);
    expect(callees).toEqual(['BaseProfiles::create']);
  });
});
