#!/usr/bin/env node
// Collect the codegraph_explore queries real agent sessions asked, for replay
// with replay-queries.mjs.
//
//   node scripts/agent-eval/harvest-queries.mjs --public <owner/repo> [--public ...] [--since 2026-09-01] <dir|file.jsonl>...
//
// Reads Claude Code transcripts (assistant tool_use blocks) and Codex rollouts
// (MCP tool calls and code-mode `tools.mcp__codegraph__codegraph_explore({...})`
// calls). Each call's project is its projectPath, else the session's working
// directory, mapped to a repository through `git remote get-url origin`. Only
// queries on a repository named by --public are kept; everything else is
// dropped and counted, so a private project never reaches the output. Output
// on stdout: `{ repos: { "<owner/repo>": { tasks: [{ id, prompt }] } } }`,
// deduplicated, with no paths. Review every prompt before committing it: a
// query on a public repository can still quote a private name.
import { execFileSync } from 'node:child_process';
import { createReadStream, readdirSync, statSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

/** Explore calls in one transcript line: [{ query, projectPath? }]. */
export function callsInLine(entry) {
  const calls = [];
  const blocks = entry?.message?.content;
  if (Array.isArray(blocks)) {
    for (const b of blocks) {
      if (b?.type === 'tool_use' && /codegraph_explore$/.test(b.name ?? '') && typeof b.input?.query === 'string') {
        calls.push({ query: b.input.query, projectPath: b.input.projectPath });
      }
    }
  }
  const p = entry?.payload;
  if (p?.type === 'function_call' && /codegraph_explore$/.test(p.name ?? '')) {
    try {
      const args = JSON.parse(p.arguments);
      if (typeof args.query === 'string') calls.push({ query: args.query, projectPath: args.projectPath });
    } catch { /* not JSON arguments */ }
  }
  if (p?.type === 'custom_tool_call' && typeof p.input === 'string') {
    for (const m of p.input.matchAll(/codegraph_explore\(\{([^}]*)\}\)/g)) {
      const field = (name) => {
        const f = new RegExp(`\\b${name}\\s*:\\s*("(?:[^"\\\\]|\\\\.)*")`).exec(m[1]);
        try { return f ? JSON.parse(f[1]) : undefined; } catch { return undefined; }
      };
      const query = field('query');
      if (typeof query === 'string') calls.push({ query, projectPath: field('projectPath') });
    }
  }
  return calls;
}

/** `owner/repo` from a GitHub remote URL, or null. */
export function repoSlug(url) {
  const m = /github\.com[:/]([^/\s]+)\/([^/\s]+?)(?:\.git)?\/?$/.exec(url.trim());
  return m ? `${m[1]}/${m[2]}`.toLowerCase() : null;
}

function* jsonlFiles(path) {
  const st = statSync(path, { throwIfNoEntry: false });
  if (!st) return;
  if (st.isFile()) { if (path.endsWith('.jsonl')) yield path; return; }
  for (const name of readdirSync(path)) yield* jsonlFiles(join(path, name));
}

async function main(argv) {
  const pub = new Set();
  const inputs = [];
  let since = '';
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--public') pub.add(argv[++i].toLowerCase());
    else if (argv[i] === '--since') since = argv[++i];
    else inputs.push(argv[i]);
  }
  if (!pub.size || !inputs.length) {
    console.error('usage: harvest-queries.mjs --public <owner/repo> [--public ...] [--since YYYY-MM-DD] <dir|file.jsonl>...');
    return 2;
  }
  const slugOf = new Map();
  const slugFor = (dir) => {
    if (!slugOf.has(dir)) {
      let slug = null;
      try {
        slug = repoSlug(execFileSync('git', ['-C', dir, 'remote', 'get-url', 'origin'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }));
      } catch { /* gone or not a repository */ }
      slugOf.set(dir, slug);
    }
    return slugOf.get(dir);
  };
  const repos = {};
  const seen = new Set();
  let kept = 0, dropped = 0, duplicate = 0;
  for (const input of inputs) {
    for (const file of jsonlFiles(input)) {
      let cwd;
      // Line by line: a long transcript can exceed the largest string a runtime allows.
      for await (const line of createInterface({ input: createReadStream(file, 'utf8'), crlfDelay: Infinity })) {
        if (!line.includes('codegraph_explore') && !line.includes('"cwd"')) continue;
        let entry;
        try { entry = JSON.parse(line); } catch { continue; }
        cwd = entry.cwd ?? entry.payload?.cwd ?? cwd;
        if (since && (entry.timestamp ?? '') < since) continue;
        for (const call of callsInLine(entry)) {
          const dir = call.projectPath ?? cwd;
          const slug = dir ? slugFor(dir) : null;
          if (!slug || !pub.has(slug)) { dropped++; continue; }
          const prompt = call.query.trim().replace(/\s+/g, ' ');
          const key = `${slug}\0${prompt.toLowerCase()}`;
          if (seen.has(key)) { duplicate++; continue; }
          seen.add(key);
          const id = createHash('sha256').update(prompt).digest('hex').slice(0, 10);
          (repos[slug] ??= { tasks: [] }).tasks.push({ id: `q-${id}`, prompt });
          kept++;
        }
      }
    }
  }
  console.log(JSON.stringify({ repos }, null, 2));
  console.error(`kept ${kept}, duplicates ${duplicate}, dropped ${dropped} (not on a --public repository)`);
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`) main(process.argv.slice(2)).then((code) => process.exit(code));
