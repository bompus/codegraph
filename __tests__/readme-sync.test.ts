/**
 * The README and the docs site's reference pages list what CodeGraph offers:
 * its CLI commands, the MCP tools an agent sees by default, the file types it
 * indexes and the agents `codegraph install` can wire up. Each list is also
 * defined in code, and the two drift apart whenever one changes without the
 * other. These checks read the code's lists and fail on anything the docs
 * leave out, so the doc update lands in the same change.
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { EXTENSION_MAP } from '../src/extraction/grammars';
import { ALL_TARGETS } from '../src/installer/targets/registry';

const root = path.join(__dirname, '..');
const read = (rel: string) => fs.readFileSync(path.join(root, rel), 'utf-8');
const README = read('README.md');
const SITE_CLI = read('site/src/content/docs/reference/cli.md');
const SITE_MCP = read('site/src/content/docs/reference/mcp-server.md');
const SITE_INTEGRATIONS = read('site/src/content/docs/reference/integrations.md');

/** The body of the README section under `heading`, up to the next `## `. */
function section(doc: string, heading: string): string {
  const start = doc.indexOf(`\n${heading}\n`);
  if (start < 0) throw new Error(`README has no "${heading}" section`);
  const end = doc.indexOf('\n## ', start + heading.length + 2);
  return doc.slice(start, end < 0 ? undefined : end);
}

/** Visible top-level commands registered in the CLI entry point. */
function cliCommands(): string[] {
  const src = read('src/bin/codegraph.ts');
  const names = new Set<string>();
  for (const m of src.matchAll(/\.command\('([\w-]+)[^']*'(\s*,\s*\{[^}]*\})?\)/g)) {
    if (m[2]?.includes('hidden: true')) continue;
    names.add(m[1]!);
  }
  // callers/callees are registered in a loop over their names.
  const loop = src.match(/for \(const direction of \[([^\]]+)\] as const\)/);
  for (const m of loop?.[1]?.matchAll(/'([\w-]+)'/g) ?? []) names.add(m[1]!);
  return [...names].sort();
}

/** Short names in `DEFAULT_MCP_TOOLS`. */
function defaultMcpTools(): string[] {
  const m = read('src/mcp/tools.ts').match(/const DEFAULT_MCP_TOOLS = new Set\(\[([^\]]*)\]\)/);
  if (!m) throw new Error('DEFAULT_MCP_TOOLS not found in src/mcp/tools.ts');
  return [...m[1]!.matchAll(/'([\w-]+)'/g)].map((x) => x[1]!);
}

describe('README and docs site list what the code offers', () => {
  it('finds the lists it checks', () => {
    expect(cliCommands()).toContain('explore');
    expect(defaultMcpTools()).toContain('explore');
  });

  it('every CLI command is in the README CLI reference and the site CLI page', () => {
    const readmeCli = section(README, '## CLI Reference');
    const missing = cliCommands().flatMap((c) => [
      ...(new RegExp(`^codegraph ${c}\\b`, 'm').test(readmeCli) ? [] : [`README: codegraph ${c}`]),
      ...(new RegExp(`^codegraph ${c}\\b`, 'm').test(SITE_CLI) ? [] : [`site cli.md: codegraph ${c}`]),
    ]);
    expect(missing).toEqual([]);
  });

  it('every default MCP tool is named in the README and the site MCP page', () => {
    const missing = defaultMcpTools().flatMap((t) => [
      ...(README.includes(`codegraph_${t}`) ? [] : [`README: codegraph_${t}`]),
      ...(SITE_MCP.includes(`codegraph_${t}`) ? [] : [`site mcp-server.md: codegraph_${t}`]),
    ]);
    expect(missing).toEqual([]);
  });

  it('every indexed language has an extension in the README language table', () => {
    const table = section(README, '## Supported Languages');
    const byLanguage = new Map<string, string[]>();
    for (const [ext, lang] of Object.entries(EXTENSION_MAP)) {
      byLanguage.set(lang, [...(byLanguage.get(lang) ?? []), ext]);
    }
    const missing = [...byLanguage]
      .filter(([, exts]) => !exts.some((e) => table.includes(`\`${e}\``)))
      .map(([lang, exts]) => `${lang} (${exts.join(', ')})`);
    expect(missing).toEqual([]);
  });

  it('every installer target is in the README agent list and the site integrations page', () => {
    const agents = section(README, '## Supported Agents');
    const named = (doc: string, t: (typeof ALL_TARGETS)[number]) =>
      doc.includes(t.displayName) || doc.includes(`\`${t.id}\``) ||
      // Copilot's three targets are listed under one GitHub Copilot entry.
      (t.id.startsWith('copilot-') && doc.includes('GitHub Copilot'));
    const missing = ALL_TARGETS.flatMap((t) => [
      ...(named(agents, t) ? [] : [`README: ${t.id}`]),
      ...(named(SITE_INTEGRATIONS, t) ? [] : [`site integrations.md: ${t.id}`]),
    ]);
    expect(missing).toEqual([]);
  });
});
