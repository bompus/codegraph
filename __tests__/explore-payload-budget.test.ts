/**
 * Payload switches for codegraph_explore (colbymchenry/codegraph#1701):
 *   - `maxChars` / CODEGRAPH_EXPLORE_MAX_CHARS lowers the tier's response cap;
 *   - CODEGRAPH_EXPLORE_SYMBOLS_ONLY=1 renders source only for files that define
 *     a named symbol and ships every other ranked file as a pointer.
 */
import { describe, it, expect, beforeAll, afterAll, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { exploreMaxChars, getExploreOutputBudget, ToolHandler } from '../src/mcp/tools';
import CodeGraph from '../src/index';

const tier = getExploreOutputBudget(300); // the <500 tier: maxOutputChars 18000

describe('exploreMaxChars', () => {
  afterEach(() => {
    delete process.env.CODEGRAPH_EXPLORE_MAX_CHARS;
  });

  it('leaves the tier alone when nothing asks for a cap', () => {
    expect(exploreMaxChars(tier)).toBe(tier);
    expect(exploreMaxChars(tier, undefined)).toBe(tier);
    expect(exploreMaxChars(tier, 'nine thousand')).toBe(tier);
  });

  it('lowers maxOutputChars to the requested cap and keeps every other field', () => {
    const capped = exploreMaxChars(tier, 9000);
    expect(capped.maxOutputChars).toBe(9000);
    expect({ ...capped, maxOutputChars: tier.maxOutputChars }).toEqual(tier);
  });

  it('never raises a tier: an oversize request is a no-op', () => {
    expect(exploreMaxChars(tier, 30000)).toBe(tier);
    expect(exploreMaxChars(tier, tier.maxOutputChars)).toBe(tier);
  });

  it('clamps a tiny request to the 2000-char floor', () => {
    expect(exploreMaxChars(tier, 500).maxOutputChars).toBe(2000);
  });

  it('reads CODEGRAPH_EXPLORE_MAX_CHARS as the default and lets the call argument win', () => {
    process.env.CODEGRAPH_EXPLORE_MAX_CHARS = '9000';
    expect(exploreMaxChars(tier).maxOutputChars).toBe(9000);
    expect(exploreMaxChars(tier, 6000).maxOutputChars).toBe(6000);
    process.env.CODEGRAPH_EXPLORE_MAX_CHARS = 'not-a-number';
    expect(exploreMaxChars(tier)).toBe(tier);
  });
});

describe('codegraph_explore payload switches end to end', () => {
  let testDir: string;
  let cg: CodeGraph;
  let handler: ToolHandler;

  beforeAll(async () => {
    testDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-explore-payload-'));
    const srcDir = path.join(testDir, 'src');
    fs.mkdirSync(srcDir);
    const fatLines: string[] = ['export class Session {'];
    for (let i = 0; i < 30; i++) {
      fatLines.push(`  method${i}(arg: string): string {`);
      fatLines.push(`    return this.helper${i}(arg) + "${i}";`);
      fatLines.push(`  }`);
      fatLines.push(`  private helper${i}(arg: string): string {`);
      fatLines.push(`    return arg.repeat(${i + 1});`);
      fatLines.push(`  }`);
    }
    fatLines.push('}');
    fs.writeFileSync(path.join(srcDir, 'session.ts'), fatLines.join('\n'));
    for (let i = 0; i < 5; i++) {
      fs.writeFileSync(
        path.join(srcDir, `support${i}.ts`),
        `import { Session } from './session';\nexport function callSession${i}(s: Session) { return s.method${i}('hi'); }\n`
      );
    }
    cg = CodeGraph.initSync(testDir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
    handler = new ToolHandler(cg);
  });

  afterAll(() => {
    if (cg) cg.destroy();
    if (testDir && fs.existsSync(testDir)) fs.rmSync(testDir, { recursive: true, force: true });
  });

  afterEach(() => {
    delete process.env.CODEGRAPH_EXPLORE_MAX_CHARS;
    delete process.env.CODEGRAPH_EXPLORE_SYMBOLS_ONLY;
  });

  const explore = async (args: Record<string, unknown>) => {
    const result = await handler.execute('codegraph_explore', args);
    return (result.content?.[0]?.text ?? '') as string;
  };

  it('a maxChars argument shrinks the answer below the tier default', async () => {
    const full = await explore({ query: 'Session method helper' });
    const capped = await explore({ query: 'Session method helper', maxChars: 4000 });
    // hardCeiling is 1.5 × the cap; the epilogue may add a little on top.
    expect(capped.length).toBeLessThan(6000 + 500);
    expect(capped.length).toBeLessThan(full.length);
  });

  it('CODEGRAPH_EXPLORE_MAX_CHARS caps the answer the same way', async () => {
    const full = await explore({ query: 'Session method helper' });
    process.env.CODEGRAPH_EXPLORE_MAX_CHARS = '4000';
    const capped = await explore({ query: 'Session method helper' });
    expect(capped.length).toBeLessThan(6000 + 500);
    expect(capped.length).toBeLessThan(full.length);
  });

  it('symbols-only renders source for the named file and ships the rest as pointers', async () => {
    const full = await explore({ query: 'callSession2' });
    // Control: the caller's class file renders beside the named function.
    expect(full).toContain('**`src/support2.ts`**');
    expect(full).toContain('**`src/session.ts`**');

    process.env.CODEGRAPH_EXPLORE_SYMBOLS_ONLY = '1';
    const only = await explore({ query: 'callSession2' });
    expect(only).toContain('**`src/support2.ts`**');
    expect(only).toContain('callSession2(s: Session)');
    expect(only).not.toContain('**`src/session.ts`**');
    // The withheld file is still nameable for a follow-up call.
    expect(only).toContain('src/session.ts');
    expect(only.length).toBeLessThan(full.length);
  });

  it('symbols-only is inert when the query names no symbol', async () => {
    const full = await explore({ query: 'how do the support helpers use the session' });
    process.env.CODEGRAPH_EXPLORE_SYMBOLS_ONLY = '1';
    const only = await explore({ query: 'how do the support helpers use the session' });
    expect(only).toBe(full);
  });
});
