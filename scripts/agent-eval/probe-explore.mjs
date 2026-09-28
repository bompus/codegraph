#!/usr/bin/env node
// Deterministic explore probe: run codegraph_explore against an existing index
// and report what came back, without an agent run.
//
//   node scripts/agent-eval/probe-explore.mjs <repo> "<query>" [--expect <regex>]...
//     Prints the answer, then stats on stderr: output size and where each
//     --expect pattern first matches. Exits 1 when any pattern is missing.
//
//   node scripts/agent-eval/probe-explore.mjs <repo> --tasks <tasks.json> [--ids md-*,cx-*] --out <dir>
//   node scripts/agent-eval/probe-explore.mjs <repo> "<query>" --out <dir>
//     One JSON line per task on stdout and each answer saved as <out>/<id>.md
//     (a single query uses the id "literal"). Rows carry ms, chars, sourceAt
//     (offset of the Source Code section, -1 when absent), files (the source
//     files the answer renders) and mustMatchAt (first offset of each task's
//     mustMatch pattern, -1 when missing). A final row with id "memory" reports
//     resident memory idle and after the run. tasks.json is `{ "tasks": [...] }`
//     or a bare array of `{ id, prompt, mustMatch? }`.
//
// Options:
//   --build <dir>      codegraph checkout whose dist/ answers (default: this one)
//   --mcp              ask a `codegraph serve --mcp` subprocess instead of
//                      calling the handler in-process; memory is the server's
//   --runtime <path>   executable that runs the server with --mcp (default: the
//                      one running this script, so `bun probe-explore.mjs --mcp`
//                      serves on Bun)
//   --timeout-ms <n>   per-request limit with --mcp (default 30000)
import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SOURCE_HEADER = '**Source Code**';
const FILE_HEADER = /^\*\*[^`\n]*`([^`]+)`\*\*/;

/** The source files an explore answer renders, in order, from its Source Code section on. */
export function renderedFiles(text) {
  const files = [];
  let inSource = false;
  for (const line of text.split('\n')) {
    if (line.startsWith(SOURCE_HEADER)) inSource = true;
    const m = inSource ? FILE_HEADER.exec(line) : null;
    if (m) files.push(m[1]);
  }
  return files;
}

/** One probe row: where the answer's source starts and where each expected pattern first lands. */
export function probeRow(id, text, ms, mustMatch = []) {
  return {
    id,
    ms,
    chars: text.length,
    sourceAt: text.indexOf(SOURCE_HEADER),
    files: renderedFiles(text),
    mustMatchAt: mustMatch.map((re) => new RegExp(re).exec(text)?.index ?? -1),
  };
}

/** Tasks whose id matches any comma-separated glob; every task when `ids` is empty. */
export function selectTasks(tasks, ids) {
  if (!ids) return tasks;
  const globs = ids.split(',').map((p) => new RegExp(`^${p.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')}$`));
  return tasks.filter((t) => globs.some((g) => g.test(t.id)));
}

/** Resident memory now and at its peak, in MB. */
function memMB(pid) {
  if (process.platform === 'linux') {
    const status = readFileSync(`/proc/${pid}/status`, 'utf8');
    const kb = (name) => Math.round(Number(new RegExp(`^${name}:\\s+(\\d+) kB$`, 'm').exec(status)?.[1] ?? 0) / 1024);
    return { rssMB: kb('VmRSS'), peakMB: kb('VmHWM') };
  }
  if (pid === process.pid) {
    return { rssMB: Math.round(process.memoryUsage().rss / 1048576), peakMB: -1 };
  }
  return { rssMB: -1, peakMB: -1 };
}

function parseArgs(argv) {
  const opts = { expect: [], positional: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--mcp') opts.mcp = true;
    else if (a === '--expect') opts.expect.push(argv[++i]);
    else if (['--tasks', '--ids', '--out', '--build', '--runtime', '--timeout-ms'].includes(a)) opts[a.slice(2)] = argv[++i];
    else opts.positional.push(a);
  }
  return opts;
}

/** In-process explore through the built ToolHandler. */
async function inProcess(build, repo) {
  const load = async (rel) => import(pathToFileURL(join(build, rel)).href);
  const idx = await load('dist/index.js');
  const tools = await load('dist/mcp/tools.js');
  // esModuleInterop: dynamic import of CJS yields { default: module.exports, ...named }
  const CodeGraph = idx.default?.default ?? idx.default ?? idx.CodeGraph;
  const ToolHandler = tools.ToolHandler ?? tools.default?.ToolHandler;
  if (typeof CodeGraph?.openSync !== 'function' || typeof ToolHandler !== 'function') {
    throw new Error(`could not resolve CodeGraph.openSync or ToolHandler under ${build}/dist`);
  }
  const cg = CodeGraph.openSync(repo);
  const handler = new ToolHandler(cg);
  return {
    pid: process.pid,
    async explore(query) {
      const res = await handler.execute('codegraph_explore', { query });
      if (res.isError) throw new Error(`codegraph_explore: ${JSON.stringify(res.content)}`);
      return (res.content ?? []).map((c) => c.text ?? '').join('\n');
    },
    async close() {
      try { cg.close?.(); } catch {}
    },
  };
}

/** Explore through a `codegraph serve --mcp` subprocess, the path an agent takes. */
async function overMcp(build, repo, runtime, timeoutMs) {
  // Direct mode: without it the first call detaches a daemon that outlives the
  // probe and keeps serving the build it was spawned from.
  const proc = spawn(runtime, [join(build, 'dist', 'bin', 'codegraph.js'), 'serve', '--mcp'], {
    cwd: repo,
    env: { ...process.env, CODEGRAPH_NO_DAEMON: '1' },
    stdio: ['pipe', 'pipe', 'inherit'],
    windowsHide: true,
  });
  const exited = new Promise((res) => proc.on('exit', (code, signal) => res({ code, signal })));
  const pending = new Map();
  let buf = '';
  proc.stdout.setEncoding('utf8');
  proc.stdout.on('data', (chunk) => {
    buf += chunk;
    let nl;
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      try {
        const msg = JSON.parse(line);
        pending.get(msg.id)?.(msg);
      } catch { /* non-JSON stdout noise */ }
    }
  });
  let nextId = 1;
  async function rpc(method, params) {
    const id = nextId++;
    let timer;
    try {
      const reply = new Promise((res) => pending.set(id, res));
      const timeout = new Promise((_, rej) => { timer = setTimeout(() => rej(new Error(`${method}: timed out after ${timeoutMs}ms`)), timeoutMs); });
      const died = exited.then(({ code, signal }) => { throw new Error(`${method}: server exited with code ${code}, signal ${signal ?? 'none'}`); });
      proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
      const response = await Promise.race([reply, died, timeout]);
      if (response.error) throw new Error(`${method}: ${JSON.stringify(response.error)}`);
      if (response.result?.isError) throw new Error(`${method}: ${JSON.stringify(response.result.content)}`);
      return response;
    } finally {
      clearTimeout(timer);
      pending.delete(id);
    }
  }
  const close = async () => {
    if (proc.exitCode === null && proc.signalCode === null) proc.kill();
    await exited;
  };
  try {
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'probe-explore', version: '0' } });
  } catch (error) {
    await close();
    throw error;
  }
  proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized', params: {} }) + '\n');
  return {
    pid: proc.pid,
    async explore(query) {
      const res = await rpc('tools/call', { name: 'codegraph_explore', arguments: { query, projectPath: repo } });
      return (res.result?.content ?? []).map((c) => c.text ?? '').join('\n');
    },
    close,
  };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const [repoArg, query] = opts.positional;
  if (!repoArg || (!query && !opts.tasks)) {
    console.error('usage: probe-explore.mjs <repo> "<query>" | --tasks <tasks.json> [--ids globs] [--out dir] [--expect regex]... [--build dir] [--mcp [--runtime path] [--timeout-ms n]]');
    return 2;
  }
  const repo = resolve(repoArg);
  const build = resolve(opts.build ?? fileURLToPath(new URL('../..', import.meta.url)));
  const rows = Boolean(opts.out || opts.tasks);
  let tasks = [{ id: 'literal', prompt: query, mustMatch: opts.expect }];
  if (opts.tasks) {
    const parsed = JSON.parse(readFileSync(opts.tasks, 'utf8'));
    tasks = selectTasks(Array.isArray(parsed) ? parsed : parsed.tasks, opts.ids);
  }
  if (opts.out) mkdirSync(opts.out, { recursive: true });

  const session = opts.mcp
    ? await overMcp(build, repo, opts.runtime ?? process.execPath, Number(opts['timeout-ms'] ?? 30000))
    : await inProcess(build, repo);
  let missing = 0;
  try {
    // Idle: the index is loaded and nothing has been asked yet.
    if (opts.mcp && rows) await new Promise((res) => setTimeout(res, 2000));
    const idle = memMB(session.pid);
    for (const task of tasks) {
      const t0 = Date.now();
      const text = await session.explore(task.prompt);
      const row = probeRow(task.id, text, Date.now() - t0, task.mustMatch);
      missing += row.mustMatchAt.filter((at) => at < 0).length;
      if (opts.out) writeFileSync(join(opts.out, `${task.id}.md`), text);
      if (rows) {
        console.log(JSON.stringify(row));
        continue;
      }
      console.log(text);
      console.error('\n--- PROBE STATS ---');
      console.error('output chars:', row.chars);
      console.error('source section at:', row.sourceAt);
      task.mustMatch.forEach((re, i) => console.error(`expect ${re}:`, row.mustMatchAt[i] < 0 ? 'MISSING' : `at ${row.mustMatchAt[i]}`));
    }
    if (rows) {
      const after = memMB(session.pid);
      const runtime = opts.mcp ? (opts.runtime ?? process.execPath) : process.execPath;
      console.log(JSON.stringify({ id: 'memory', runtime, idleMB: idle.rssMB, afterMB: after.rssMB, peakMB: after.peakMB }));
    }
  } finally {
    await session.close();
  }
  return missing > 0 && !rows ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then(
    (code) => process.exit(code),
    (error) => {
      console.error(`[probe-explore] ${error instanceof Error ? error.message : String(error)}`);
      process.exit(1);
    },
  );
}
