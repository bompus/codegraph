import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';


// Native graph regressions from upstream #2332/#2334.
// TypeScript spies do not observe the Rust resolver; these make no cost claim.
describe('JS resolution graph (#2334)', () => {
  let tmpDir: string | undefined;
  let cg: CodeGraph | undefined;

  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (tmpDir) fs.rmSync(tmpDir, { recursive: true, force: true, maxRetries: 5 });
    tmpDir = undefined;
  });

  async function index(files: Record<string, string>): Promise<CodeGraph> {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-js-work-'));
    for (const [file, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(tmpDir, file)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, file), content);
    }
    cg = CodeGraph.initSync(tmpDir);
    await cg.indexAll();
    return cg;
  }


  const callers = (graph: CodeGraph, file: string, name: string) => {
    const target = graph.getNodesByName(name).find((n) => n.filePath === file && n.kind === 'function')!;
    return [...new Set(graph.getIncomingEdges(target.id).filter((e) => e.kind === 'calls')
      .map((e) => graph.getNode(e.source)?.name))];
  };

  it('resolves a destructured factory call inside division expressions', async () => {
    const CALLS = 40;
    const app = `import { useAuth } from './auth';\n` +
      `const { login } = useAuth();\n` +
      Array.from({ length: CALLS }, (_, i) => `function step${i}() { return ${i}; }\n`).join('') +
      `export function run() {\n` +
      Array.from({ length: CALLS }, (_, i) => `  step${i}();\n`).join('') +
      // A division the blanking reads as a regex literal around the call.
      `  return (${CALLS}) / login() / 2;\n}\n`;
    const graph = await index({
      'auth.js': 'export function useAuth() {\n  function login() {\n    return 1;\n  }\n  return { login };\n}\n',
      'app.js': app,
    });
    // The call through the destructured name reaches the function the hook returns.
    expect(callers(graph, 'auth.js', 'login')).toEqual(['run']);
    expect(callers(graph, 'app.js', 'step7')).toEqual(['run']);
  });

  it("resolves repeated bare calls in one function", async () => {
    const LINES = 40;
    const host = `export function host() {\n` +
      Array.from({ length: LINES }, (_, i) => `  helper(${i});\n`).join('') + `}\n`;
    const graph = await index({ 'lib.js': `export function helper(n) {\n  return n;\n}\n${host}` });
    // Each call resolved, so each asked whether `host` binds `helper` itself.
    expect(callers(graph, 'lib.js', 'helper')).toEqual(['host']);
  });
});
