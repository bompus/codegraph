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
// on stdout: `{ repos: { "<owner/repo>": { tasks: [{ id, prompt, maxFiles? }] } } }`,
// deduplicated on query and file limit, with no paths. Review every prompt before committing it: a
// query on a public repository can still quote a private name.
import { execFileSync } from 'node:child_process';
import { createReadStream, readdirSync, statSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

/** Explore calls in one transcript line: [{ query, projectPath?, maxFiles? }]. */
export function callsInLine(entry) {
  const calls = [];
  const blocks = entry?.message?.content;
  if (Array.isArray(blocks)) {
    for (const b of blocks) {
      if (b?.type === 'tool_use' && /codegraph_explore$/.test(b.name ?? '') && typeof b.input?.query === 'string') {
        calls.push({ query: b.input.query, projectPath: b.input.projectPath, maxFiles: b.input.maxFiles });
      }
    }
  }
  const p = entry?.payload;
  if (p?.type === 'function_call' && /codegraph_explore$/.test(p.name ?? '')) {
    try {
      const args = JSON.parse(p.arguments);
      if (typeof args.query === 'string') calls.push({ query: args.query, projectPath: args.projectPath, maxFiles: args.maxFiles });
    } catch { /* not JSON arguments */ }
  }
  if (p?.type === 'custom_tool_call' && typeof p.input === 'string') {
    for (const m of p.input.matchAll(/codegraph_explore\(\s*\{/g)) {
      const fields = literalFields(p.input, m.index + m[0].length - 1);
      if (typeof fields.query === 'string') calls.push({ query: fields.query, projectPath: fields.projectPath, maxFiles: fields.maxFiles });
    }
  }
  return calls.map((c) => (typeof c.maxFiles === 'number' ? c : { query: c.query, projectPath: c.projectPath }));
}

const ESCAPES = { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f', v: '\v', 0: '\0' };

/**
 * The top-level string and number fields of the JavaScript object literal that
 * opens at `src[open]` ('{'). Code-mode calls are JavaScript, not JSON: keys may
 * be bare, strings may use any quote and may contain braces. A field whose
 * value is not a plain literal (a variable, an expression, a template string
 * with `${`) is null, so a caller can tell it was present but unreadable.
 */
export function literalFields(src, open) {
  const fields = {};
  let depth = 0, key, expectKey = false, expectValue = false;
  for (let i = open; i < src.length; i++) {
    const ch = src[i];
    if (ch === '"' || ch === "'" || ch === '`') {
      let s = '', literal = true, j = i + 1;
      for (; j < src.length && src[j] !== ch; j++) {
        if (src[j] === '\\') { j++; s += ESCAPES[src[j]] ?? src[j]; }
        else if (ch === '`' && src[j] === '$' && src[j + 1] === '{') { literal = false; s += src[j]; }
        else s += src[j];
      }
      i = j;
      if (depth === 1 && expectKey) { key = s; expectKey = false; }
      else if (depth === 1 && expectValue && key !== undefined) fields[key] = literal ? s : null;
      expectValue = false;
    } else if (ch === '{' || ch === '[' || ch === '(') {
      if (depth === 1 && expectValue && key !== undefined) fields[key] = null;
      depth++;
      if (depth === 1) expectKey = true;
      expectValue = false;
    } else if (ch === '}' || ch === ']' || ch === ')') {
      if (--depth === 0) break;
    } else if (depth === 1 && ch === ',') {
      expectKey = true; expectValue = false; key = undefined;
    } else if (depth === 1 && ch === ':') {
      expectValue = true;
    } else if (depth === 1 && /[A-Za-z_$0-9]/.test(ch)) {
      const word = /^[A-Za-z_$0-9.]+/.exec(src.slice(i))[0];
      i += word.length - 1;
      if (expectKey) { key = word; expectKey = false; }
      else if (expectValue && key !== undefined) fields[key] = /^\d+$/.test(word) ? Number(word) : null;
      expectValue = false;
    }
  }
  return fields;
}

/**
 * The directory a call asked about: its projectPath, resolved against the
 * session's working directory when relative, or that directory when the call
 * named none. Null when it cannot be known (a projectPath that is not a plain
 * string, or a relative one with no session directory): such a call is
 * dropped, never credited to the directory the session started in.
 */
export function callDirectory(call, cwd) {
  if (call.projectPath === undefined) return cwd ?? null;
  if (typeof call.projectPath !== 'string' || call.projectPath === '') return null;
  if (isAbsolute(call.projectPath)) return call.projectPath;
  return cwd ? resolve(cwd, call.projectPath) : null;
}

/** `owner/repo` from a remote on github.com itself (HTTPS, SSH URL or scp form), or null. */
export function repoSlug(url) {
  const text = url.trim();
  const scp = /^git@github\.com:([^/\s]+)\/([^/\s]+?)(?:\.git)?\/?$/.exec(text);
  if (scp) return `${scp[1]}/${scp[2]}`.toLowerCase();
  let parsed;
  try { parsed = new URL(text); } catch { return null; }
  if (parsed.hostname.toLowerCase() !== 'github.com') return null;
  const m = /^\/([^/]+)\/([^/]+?)(?:\.git)?\/?$/.exec(parsed.pathname);
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
    if (argv[i] === '--public') {
      const name = argv[++i];
      pub.add(repoSlug(name) ?? name.toLowerCase().replace(/\.git$/, ''));
    }
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
          const dir = callDirectory(call, cwd);
          const slug = dir ? slugFor(dir) : null;
          if (!slug || !pub.has(slug)) { dropped++; continue; }
          const prompt = call.query.trim().replace(/\s+/g, ' ');
          const limit = call.maxFiles === undefined ? '' : `\0${call.maxFiles}`;
          const key = `${slug}\0${prompt.toLowerCase()}${limit}`;
          if (seen.has(key)) { duplicate++; continue; }
          seen.add(key);
          const id = createHash('sha256').update(prompt + limit).digest('hex').slice(0, 10);
          const task = { id: `q-${id}`, prompt };
          if (call.maxFiles !== undefined) task.maxFiles = call.maxFiles;
          (repos[slug] ??= { tasks: [] }).tasks.push(task);
          kept++;
        }
      }
    }
  }
  console.log(JSON.stringify({ repos }, null, 2));
  console.error(`kept ${kept}, duplicates ${duplicate}, dropped ${dropped} (project unknown or not on a --public repository)`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code));
}
