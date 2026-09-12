#!/usr/bin/env node
/**
 * Read-time parse benchmark: kernel parse-tree service vs the wasm parser
 * (Phase 3 of docs/design/kernel-only-extraction-plan.md).
 *
 * For every source file under the given roots (kernel-routed languages
 * only), times the three read-time derivations on both trees and checks that
 * their outputs agree:
 *
 *   tokenize  — syntax-tokens.tokenizeSource (viewer highlighting)
 *   guards    — branch-guards.guardsInSource at up to 20 call sites per file
 *   parse     — the bare parse (parseSourceTree), the floor under both
 *
 * Usage:
 *   node scripts/bench-parse-tree.mjs <dir>... [--lang c,kotlin] [--max-files N]
 *
 * Requires: npm run build (dist/) and a staged kernel. Prints a table of
 * median and total wall per derivation per arm, the speedup, and the number
 * of files whose outputs differed (expected only for erroring trees).
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const dist = (p) => path.join(ROOT, 'dist', p);

const args = process.argv.slice(2);
const roots = [];
let langFilter = null;
let maxFiles = 400;
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--lang') langFilter = new Set(args[++i].split(','));
  else if (args[i] === '--max-files') maxFiles = Number(args[++i]);
  else roots.push(args[i]);
}
if (roots.length === 0) {
  console.error('usage: bench-parse-tree.mjs <dir>... [--lang c,kotlin] [--max-files N]');
  process.exit(2);
}

const EXTS = new Map([
  ['.ts', 'typescript'], ['.tsx', 'tsx'], ['.js', 'javascript'], ['.jsx', 'jsx'],
  ['.java', 'java'], ['.py', 'python'], ['.go', 'go'], ['.c', 'c'], ['.h', 'c'],
  ['.cpp', 'cpp'], ['.cc', 'cpp'], ['.hpp', 'cpp'], ['.rs', 'rust'], ['.cs', 'csharp'],
  ['.rb', 'ruby'], ['.php', 'php'], ['.swift', 'swift'], ['.kt', 'kotlin'], ['.R', 'r'],
  ['.lua', 'lua'], ['.luau', 'luau'], ['.scala', 'scala'], ['.dart', 'dart'],
]);
const SKIP_DIRS = new Set(['node_modules', '.git', 'dist', 'build', 'target', 'vendor', 'deps']);

function collect(dir, out) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (out.length >= maxFiles) return;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) collect(p, out);
    } else {
      const lang = EXTS.get(path.extname(e.name));
      if (!lang) continue;
      if (langFilter && !langFilter.has(lang)) continue;
      const size = fs.statSync(p).size;
      if (size === 0 || size > 256 * 1024) continue;
      out.push({ file: p, lang });
    }
  }
}
const files = [];
for (const r of roots) collect(path.resolve(r), files);
if (files.length === 0) {
  console.error('no matching files');
  process.exit(2);
}

const { initGrammars, loadGrammarsForLanguages } = await import(dist('extraction/grammars.js'));
const { parseSourceTree } = await import(dist('extraction/parse-tree.js'));
const { tokenizeSource } = await import(dist('extraction/syntax-tokens.js'));
const { guardsInSource, supportsBranchGuards } = await import(dist('graph/branch-guards.js'));
const kernel = await import(dist('extraction/kernel/index.js'));

await initGrammars();
await loadGrammarsForLanguages([...new Set(files.map((f) => f.lang))]);
process.env.CODEGRAPH_KERNEL_LANGS = 'all';
delete process.env.CODEGRAPH_KERNEL;
if (!kernel.getKernel()) {
  console.error('no kernel staged — run npm run build:kernel');
  process.exit(2);
}

const hr = () => Number(process.hrtime.bigint()) / 1e6;
function median(xs) {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.floor(s.length / 2)] : 0;
}
const sites = (source) => {
  const out = [];
  const lines = source.split('\n');
  for (let i = 0; i < lines.length && out.length < 20; i++) {
    const m = /[A-Za-z_][\w.]*\(/.exec(lines[i]);
    if (m) out.push({ line: i + 1, column: m.index });
  }
  return out;
};

async function arm(name) {
  if (name === 'wasm') process.env.CODEGRAPH_KERNEL = '0';
  else delete process.env.CODEGRAPH_KERNEL;
  const t = { parse: [], tokenize: [], guards: [] };
  const outputs = new Map();
  for (const { file, lang } of files) {
    const source = fs.readFileSync(file, 'utf8');
    let t0 = hr();
    const tree = await parseSourceTree(source, lang);
    t.parse.push(hr() - t0);
    const rec = { hasError: tree?.rootNode.hasError ?? null, spans: null, guards: null };
    tree?.delete();
    t0 = hr();
    const tok = await tokenizeSource(source, lang);
    t.tokenize.push(hr() - t0);
    rec.spans = JSON.stringify(tok?.spans ?? null);
    if (supportsBranchGuards(lang)) {
      const ss = sites(source);
      t0 = hr();
      const gs = [];
      for (const s of ss) gs.push(await guardsInSource(source, lang, s.line, s.column));
      t.guards.push(hr() - t0);
      rec.guards = JSON.stringify(gs);
    }
    outputs.set(file, rec);
  }
  return { t, outputs };
}

// Warm both arms once so JIT and grammar-load costs stay out of the numbers.
await arm('kernel');
await arm('wasm');
const K = await arm('kernel');
const W = await arm('wasm');

let differ = 0;
let differClean = 0;
for (const [file, k] of K.outputs) {
  const w = W.outputs.get(file);
  if (k.spans !== w.spans || k.guards !== w.guards) {
    differ++;
    if (!w.hasError) differClean++;
  }
}

const row = (label, k, w) => {
  const sk = k.reduce((a, b) => a + b, 0);
  const sw = w.reduce((a, b) => a + b, 0);
  return `| ${label} | ${median(w).toFixed(2)} | ${median(k).toFixed(2)} | ${sw.toFixed(0)} | ${sk.toFixed(0)} | ${(sw / Math.max(sk, 0.001)).toFixed(2)}x |`;
};
console.log(`\nfiles: ${files.length} (${[...new Set(files.map((f) => f.lang))].join(', ')})`);
console.log('| derivation | wasm median ms | kernel median ms | wasm total ms | kernel total ms | speedup |');
console.log('|---|---|---|---|---|---|');
console.log(row('parse', K.t.parse, W.t.parse));
console.log(row('tokenize', K.t.tokenize, W.t.tokenize));
if (K.t.guards.length) console.log(row('guards (≤20 sites/file)', K.t.guards, W.t.guards));
console.log(`\noutputs differ: ${differ} of ${files.length} files (${differClean} of them with a clean wasm tree — must be 0)`);
process.exit(differClean > 0 ? 1 : 0);
