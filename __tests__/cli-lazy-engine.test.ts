/**
 * A command that only reads an index doesn't load the code that writes one.
 *
 * `src/index.ts` is the library surface and imports the extraction, resolution
 * and sync barrels; the CLI reaches the class module (`src/codegraph.ts`)
 * instead, which loads its stacks on first use. These tests run the built CLI
 * in child processes and read what it actually loaded.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const DIST = path.resolve(__dirname, '../dist');
const BIN = path.join(DIST, 'bin', 'codegraph.js');
/** The modules only a writer needs: the extractor, the resolver and the watcher. */
const ENGINE_FILES = ['extraction/index.js', 'extraction/tree-sitter.js', 'resolution/index.js', 'sync/watcher.js'];

const env = { ...process.env, CODEGRAPH_NO_DAEMON: '1' };

function node(args: string[], cwd: string): string {
  return execFileSync(process.execPath, args, {
    cwd,
    encoding: 'utf-8',
    env,
    stdio: ['ignore', 'pipe', 'ignore'], // drop stderr (SQLite experimental warning)
  });
}

describe('the engine stays unloaded until something needs it', () => {
  let root: string;
  let dir: string;
  let hook: string;
  let loadedList: string;

  /** Run the built CLI, and list the files of dist/ it loaded. */
  function cli(args: string[]): { stdout: string; loaded: string[] } {
    const stdout = node(['--require', hook, BIN, ...args], dir);
    const files = JSON.parse(fs.readFileSync(loadedList, 'utf-8')) as string[];
    return {
      stdout,
      loaded: files.filter((f) => f.startsWith(DIST)).map((f) => path.relative(DIST, f).split(path.sep).join('/')),
    };
  }

  beforeAll(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-lazy-engine-'));
    dir = path.join(root, 'project');
    fs.mkdirSync(path.join(dir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'src/auth.ts'), 'export function parseToken(t: string) { return t.trim(); }\n');
    fs.writeFileSync(
      path.join(dir, 'src/login.ts'),
      "import { parseToken } from './auth';\nexport function login(t: string) { return parseToken(t); }\n",
    );
    const cg = await CodeGraph.init(dir, { index: true });
    cg.destroy();

    loadedList = path.join(root, 'loaded.json');
    hook = path.join(root, 'hook.cjs');
    fs.writeFileSync(
      hook,
      `process.on('exit', () => require('fs').writeFileSync(${JSON.stringify(loadedList)}, JSON.stringify(Object.keys(require.cache))));\n`,
    );
  });

  afterAll(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it.each([
    ['callers', ['callers', 'parseToken', '-p', '.'], 'login'],
    ['query', ['query', 'parseToken', '-p', '.'], 'parseToken'],
  ])('`codegraph %s` answers without it', (_name, args, expected) => {
    const { stdout, loaded } = cli(args);
    expect(stdout).toContain(expected);
    expect(loaded).toContain('codegraph.js');
    expect(loaded.filter((f) => ENGINE_FILES.includes(f))).toEqual([]);
  });

  it('`codegraph status` loads it for what only the engine can answer', () => {
    const { stdout, loaded } = cli(['status', dir]);
    expect(stdout).toContain('Index Statistics');
    expect(loaded).toContain('extraction/index.js');
  });

  it('`codegraph sync` writes through it', () => {
    fs.writeFileSync(path.join(dir, 'src/auth.ts'), 'export function parseToken(t: string) { return t; }\nexport function freshName() {}\n');
    const { loaded } = cli(['sync', dir]);
    expect(loaded).toContain('extraction/index.js');
    expect(cli(['query', 'freshName', '-p', '.']).stdout).toContain('freshName');
  });
});
