/**
 * Path-vocabulary seeds + importer spine (the warp-drive CLI bug): a bare
 * query token that names a DIRECTORY segment ("command" → `commands/`) is
 * location evidence the name/FTS channels can't express — the dispatch hub
 * (`cmd.config.ts`'s `COMMANDS` registry) shares no token with the query, and
 * the entrypoint (`main.ts`) shares none either, so the flow stayed invisible
 * until the agent spelled literal paths.
 *
 * The fixture mirrors that shape: an entrypoint that calls `main`, a `main`
 * that reads a `COMMANDS` registry in `cmd.config.ts`, and lazy command
 * modules under `commands/` reached only through dynamic `import('./x')`
 * specifiers. The registry's hub symbol is a `constant` — kind-weighted at
 * 0.35 it lands under the score floor, and spine-injected files carry zero
 * RWR mass by construction — so each selection stage had to be taught not to
 * drop deliberately-injected evidence.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';

let tempDir: string;
let cg: CodeGraph | null = null;

function project(files: Record<string, string>): void {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-pathvocab-'));
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(tempDir, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, content);
  }
}

async function explore(query: string): Promise<string> {
  const res = await new ToolHandler(cg!).execute('codegraph_explore', { query });
  return res.content?.[0]?.text ?? '';
}

const hasSection = (response: string, file: string): boolean =>
  response.includes('**`' + file + '`');

afterEach(() => {
  cg?.destroy();
  cg = null;
  fs.rmSync(tempDir, { recursive: true, force: true });
});

const CLI_FIXTURE: Record<string, string> = {
  'src/cli.ts': "import { main } from './main';\nmain();\n",
  'src/main.ts': [
    "import { COMMANDS } from './cmd.config';",
    'export async function main(argv: string[]) {',
    '  const cmd = COMMANDS[argv[0]];',
    '  const mod = await cmd.load();',
    '  return mod.run();',
    '}',
  ].join('\n'),
  'src/cmd.config.ts': [
    'export const COMMANDS = {',
    "  about: { load: () => import('./commands/about.ts').then((v) => v.about) },",
    "  generate: { load: () => import('./commands/generate.ts').then((v) => v.generate) },",
    '};',
  ].join('\n'),
  'src/commands/about.ts': 'export function about() { return "about"; }\n',
  'src/commands/generate.ts': 'export function generate() { return "gen"; }\n',
  // A lexical decoy: shares the query's words but none of the wiring.
  'src/docs/command-notes.ts': 'export function commandDispatchNotes() { return 1; }\n',
};

describe('path-vocabulary seeds + importer spine', () => {
  it('a bare directory token surfaces the lazy module, its registry, and the entrypoint', async () => {
    project(CLI_FIXTURE);
    cg = CodeGraph.initSync(tempDir);
    await cg.indexAll();

    const out = await explore('how does the CLI dispatch a command like about');
    expect(hasSection(out, 'src/commands/about.ts')).toBe(true);
    expect(hasSection(out, 'src/cmd.config.ts')).toBe(true);
    expect(hasSection(out, 'src/main.ts')).toBe(true);
  }, 60_000);

  it('renders the registry constant — not just its file shell', async () => {
    project(CLI_FIXTURE);
    cg = CodeGraph.initSync(tempDir);
    await cg.indexAll();

    const out = await explore('how does the CLI dispatch a command like about');
    expect(out).toContain('COMMANDS');
  }, 60_000);

  it('a query with no path vocabulary does not invent the spine', async () => {
    project(CLI_FIXTURE);
    cg = CodeGraph.initSync(tempDir);
    await cg.indexAll();

    const out = await explore('main');
    expect(hasSection(out, 'src/main.ts')).toBe(true);
  }, 60_000);
});
