#!/usr/bin/env node
/**
 * Rebuild CHANGELOG.md after an upstream merge.
 *
 * `.gitattributes` merges CHANGELOG.md with git's union driver, so an upstream
 * merge never stops on it. Union works while both sides only add entries under
 * `## [Unreleased]`. Once upstream cuts a release (renames its `[Unreleased]`
 * to `[x.y.z]`), union files fork entries into upstream's new release section,
 * doubles some and drops others.
 *
 * This script rebuilds the file from the merge's parents instead of reading
 * the union result:
 *
 *   - the fork parent's header, through `## [Unreleased]`;
 *   - the fork parent's `[Unreleased]` entries, minus every entry that appears
 *     word for word in an upstream release, and minus every entry the fork
 *     carried unchanged from upstream's `[Unreleased]` at the merge base that
 *     upstream no longer lists there (released under new wording, or removed);
 *   - upstream's own `[Unreleased]` entries the fork lacks, under their `###`
 *     heading;
 *   - upstream's file, verbatim, from its newest release heading to the end.
 *
 * A remaining fork entry that reads like an entry upstream released since the
 * fork parent's newest release, but not word for word, stays in place and is
 * reported for a person to decide.
 *
 * Usage, from the repository root:
 *   node scripts/changelog-reconcile.mjs [--fork <ref>] [--upstream <ref>] [--base <ref>] [--check] [--strict]
 *
 * During a merge the refs default to HEAD and MERGE_HEAD; on a merge commit to
 * HEAD^1 and HEAD^2. --base defaults to their merge base. The script writes
 * CHANGELOG.md; --check only reports whether the file differs from the rebuild
 * (exit 1 when it does). --strict also exits 1 when an entry needs review or
 * the fork parent's release sections differ from upstream's.
 */
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const UNRELEASED = '## [Unreleased]';

/** Split a changelog into its header (through `## [Unreleased]`), the Unreleased body and the release tail. */
export function splitChangelog(text) {
  const lines = text.split('\n');
  const u = lines.indexOf(UNRELEASED);
  if (u < 0) throw new Error(`no "${UNRELEASED}" heading`);
  let r = lines.findIndex((l, i) => i > u && l.startsWith('## ['));
  if (r < 0) r = lines.length;
  return { header: lines.slice(0, u + 1), body: lines.slice(u + 1, r), tail: lines.slice(r) };
}

const blank = (line) => line.trim() === '';
const headingLevel = (line) => /^(#{3,}) /.exec(line)?.[1].length ?? 0;

/**
 * The `- ` entries of a section body. An entry runs on through indented lines,
 * including ones after a blank line, and through unindented text lines that
 * follow it with no blank line between. Each entry records the `###`/`####`
 * headings it sits under.
 */
export function entries(lines) {
  const out = [];
  let path = [];
  for (let i = 0; i < lines.length; i++) {
    const level = headingLevel(lines[i]);
    if (level) path = [...path.filter((h) => headingLevel(h) < level), lines[i]];
    if (!lines[i].startsWith('- ')) continue;
    let end = i + 1;
    for (let j = i + 1; j < lines.length; j++) {
      const line = lines[j];
      if (blank(line)) continue;
      const lazy = j === end && !line.startsWith('- ') && !line.startsWith('#');
      if (!/^\s/.test(line) && !lazy) break;
      end = j + 1;
    }
    out.push({ start: i, end, path, key: entryKey(lines.slice(i, end)) });
    i = end - 1;
  }
  return out;
}

const entryKey = (lines) => lines.map((l) => l.trimEnd()).join('\n');

/** Entry keys of every release section in a tail, by the release heading they sit under. */
function releasedEntries(tail) {
  const byKey = new Map();
  let release = null;
  let section = [];
  const flush = () => {
    for (const e of entries(section)) if (!byKey.has(e.key)) byKey.set(e.key, release);
    section = [];
  };
  for (const line of tail) {
    if (line.startsWith('## [')) {
      flush();
      release = line;
    } else section.push(line);
  }
  flush();
  return byKey;
}

const version = (heading) => /^## \[([^\]]+)\]/.exec(heading)?.[1] ?? null;

/** Lowercased words of an entry, without markup. */
function words(key) {
  return key
    .toLowerCase()
    .replace(/[^a-z0-9#]+/g, ' ')
    .trim()
    .split(' ')
    .filter(Boolean);
}

/**
 * Whether two entries read like the same change: the same words in the same
 * order, the same first eight words, or at least 60% of their distinct longer
 * words shared.
 */
export function similar(a, b) {
  const wa = words(a);
  const wb = words(b);
  if (wa.join(' ') === wb.join(' ')) return true;
  if (wa.length >= 8 && wa.slice(0, 8).join(' ') === wb.slice(0, 8).join(' ')) return true;
  const sa = new Set(wa.filter((w) => w.length > 3));
  const sb = new Set(wb.filter((w) => w.length > 3));
  if (sa.size === 0 || sb.size === 0) return false;
  let shared = 0;
  for (const w of sa) if (sb.has(w)) shared++;
  return shared / (sa.size + sb.size - shared) >= 0.6;
}

/** Where the section a heading at `i` opens ends: the next heading of its level or higher. */
function sectionEnd(lines, i, limit = lines.length) {
  const level = headingLevel(lines[i]);
  let j = i + 1;
  while (j < limit && !(headingLevel(lines[j]) && headingLevel(lines[j]) <= level)) j++;
  return j;
}

/** Whether the heading at `i` has nothing but blank lines before the next heading of its level or higher. */
function emptyHeadingAt(lines, i) {
  if (!headingLevel(lines[i])) return false;
  for (let j = i + 1; j < sectionEnd(lines, i); j++) if (!blank(lines[j])) return false;
  return true;
}

/**
 * Drop the headings the rebuild emptied, with the blank lines after them.
 * `origin[i]` is the input line `lines[i]` came from; headings in `keep`, by
 * input line, stay.
 */
function dropEmptiedHeadings(lines, origin, keep) {
  for (let i = 0; i < lines.length; ) {
    if (!emptyHeadingAt(lines, i) || keep.has(origin[i])) {
      i++;
      continue;
    }
    let j = i + 1;
    while (j < lines.length && blank(lines[j])) j++;
    lines.splice(i, j - i);
    origin.splice(i, j - i);
    // Removing a sub-heading can empty its parent, which sits above it.
    i = 0;
  }
}

/**
 * Add an entry under its headings: after the last entry directly under the
 * deepest one, or after that heading when it has none. Headings the body lacks
 * are added at the end of the deepest one it has.
 */
function insertEntry(body, path, lines) {
  let lo = 0;
  let hi = body.length;
  let matched = 0;
  for (const heading of path) {
    const at = body.findIndex((l, k) => k >= lo && k < hi && l === heading);
    if (at < 0) break;
    hi = sectionEnd(body, at, hi);
    lo = at + 1;
    matched++;
  }
  if (matched < path.length) {
    let at = hi;
    while (at > lo && blank(body[at - 1])) at--;
    const added = [...path.slice(matched).flatMap((h) => ['', h]), '', ...lines];
    if (at < body.length && !blank(body[at])) added.push('');
    body.splice(at, 0, ...added);
    return;
  }
  let direct = lo;
  while (direct < hi && !headingLevel(body[direct])) direct++;
  const own = entries(body.slice(0, direct)).filter((e) => e.start >= lo);
  if (own.length === 0) {
    body.splice(lo, 0, '', ...lines);
    return;
  }
  const last = own.at(-1);
  const spaced = own.length > 1 && blank(body[last.start - 1]);
  body.splice(last.end, 0, ...(spaced ? ['', ...lines] : lines));
}

/**
 * Rebuild the changelog from the fork parent, upstream and (optionally) their
 * merge base. Returns the text and what it changed.
 */
export function reconcileChangelog({ fork, upstream, base = null }) {
  const f = splitChangelog(fork);
  const u = splitChangelog(upstream);
  const released = releasedEntries(u.tail);

  // Upstream releases newer than the fork parent's newest one.
  const forkNewest = version(f.tail.find((l) => l.startsWith('## [')) ?? '');
  const newReleases = new Set();
  for (const line of u.tail) {
    if (!line.startsWith('## [')) continue;
    if (version(line) === forkNewest) break;
    newReleases.add(line);
  }

  // Entries the fork carried unchanged from upstream's Unreleased at the merge
  // base, counted, so a same-worded entry the fork added a second time stays.
  // The fork may file them under its own headings; when it has more copies
  // than the base, the ones under the base's headings go first.
  const upstreamUnreleased = new Set(entries(u.body).map((e) => e.key));
  const carried = new Map();
  if (base !== null) {
    for (const e of entries(splitChangelog(base).body)) {
      if (!upstreamUnreleased.has(e.key)) carried.set(e.key, [...(carried.get(e.key) ?? []), e.path.join('\n')]);
    }
  }
  const forkEntries = entries(f.body);
  const carriedHere = new Set();
  for (const [key, paths] of carried) {
    const copies = forkEntries.filter((e) => e.key === key);
    copies.sort((x, y) => paths.includes(y.path.join('\n')) - paths.includes(x.path.join('\n')));
    for (const e of copies.slice(0, paths.length)) carriedHere.add(e);
  }

  const dropped = [];
  const drop = new Map();
  for (const e of forkEntries) {
    if (released.has(e.key)) dropped.push({ entry: e.key, reason: `released in ${version(released.get(e.key))}` });
    else if (carriedHere.has(e)) dropped.push({ entry: e.key, reason: "upstream's, gone from its [Unreleased]" });
    else continue;
    drop.set(e.start, e);
  }

  // Copy the body without the dropped entries; a removal never widens the gap around it.
  const body = [];
  const origin = [];
  for (let i = 0; i < f.body.length; ) {
    const e = drop.get(i);
    if (!e) {
      body.push(f.body[i]);
      origin.push(i++);
      continue;
    }
    i = e.end;
    if (body.length > 0 && blank(body.at(-1))) while (i < f.body.length && blank(f.body[i])) i++;
  }
  const emptyBefore = new Set(f.body.map((_, i) => i).filter((i) => emptyHeadingAt(f.body, i)));
  dropEmptiedHeadings(body, origin, emptyBefore);

  // Upstream's own Unreleased entries the fork lacks.
  const added = [];
  const have = new Set(entries(body).map((e) => e.key));
  for (const e of entries(u.body)) {
    if (have.has(e.key)) continue;
    insertEntry(body, e.path, u.body.slice(e.start, e.end));
    have.add(e.key);
    added.push(e.key);
  }
  while (body.length > 0 && blank(body.at(-1))) body.pop();

  // What is left that reads like an entry upstream just released.
  const review = [];
  const fresh = [...released].filter(([, rel]) => newReleases.has(rel));
  for (const e of entries(body)) {
    const match = fresh.find(([key]) => similar(e.key, key));
    if (match) review.push({ entry: e.key, like: match[0], release: version(match[1]) });
  }

  // Released sections the fork parent shows differently from upstream; the rebuild keeps upstream's.
  const start = u.tail.findIndex((l) => version(l) === forkNewest);
  const tailDiffers = start >= 0 && f.tail.join('\n') !== u.tail.slice(start).join('\n');

  const text = [...f.header, ...body, '', ...u.tail].join('\n');
  return { text, dropped, added, review, tailDiffers, newReleases: [...newReleases].map(version) };
}

function git(args) {
  return execFileSync('git', args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

function hasRef(ref) {
  try {
    git(['rev-parse', '-q', '--verify', `${ref}^{commit}`]);
    return true;
  } catch {
    return false;
  }
}

function parseArgs(argv) {
  const opts = { check: false, strict: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--check') opts.check = true;
    else if (a === '--strict') opts.strict = true;
    else if (a === '--fork' || a === '--upstream' || a === '--base') {
      const value = argv[++i];
      if (!value) throw new Error(`${a} needs a ref`);
      opts[a.slice(2)] = value;
    } else throw new Error(`unknown argument: ${a}`);
  }
  if (!opts.fork !== !opts.upstream) throw new Error('pass both --fork and --upstream, or neither');
  if (!opts.fork && !opts.upstream) {
    if (hasRef('MERGE_HEAD')) Object.assign(opts, { fork: 'HEAD', upstream: 'MERGE_HEAD' });
    else if (hasRef('HEAD^2')) Object.assign(opts, { fork: 'HEAD^1', upstream: 'HEAD^2' });
  }
  if (!opts.fork || !opts.upstream) {
    throw new Error('not in a merge and HEAD is not a merge commit: pass --fork <ref> and --upstream <ref>');
  }
  opts.base ??= git(['merge-base', opts.fork, opts.upstream]).trim();
  return opts;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const show = (ref) => git(['show', `${ref}:CHANGELOG.md`]);
  const result = reconcileChangelog({ fork: show(opts.fork), upstream: show(opts.upstream), base: show(opts.base) });
  const path = join(git(['rev-parse', '--show-toplevel']).trim(), 'CHANGELOG.md');
  // A Windows checkout may hold CRLF line endings; git stores the file with LF.
  const current = readFileSync(path, 'utf8').replace(/\r\n/g, '\n');
  const first = (key) => key.split('\n')[0].slice(0, 110);

  console.log(`fork ${opts.fork}, upstream ${opts.upstream}, base ${opts.base.slice(0, 8)}`);
  console.log(`upstream releases since the fork parent's newest: ${result.newReleases.join(', ') || 'none'}`);
  console.log(`dropped ${result.dropped.length}, added from upstream's [Unreleased] ${result.added.length}`);
  for (const d of result.dropped) console.log(`  dropped (${d.reason}): ${first(d.entry)}`);
  for (const a of result.added) console.log(`  added: ${first(a)}`);
  if (result.tailDiffers) {
    console.log("note: the fork parent's release sections differ from upstream's; the rebuild keeps upstream's");
  }
  if (result.review.length > 0) {
    console.log(`review ${result.review.length}: kept, but each reads like an entry upstream released`);
    for (const r of result.review) console.log(`  fork: ${first(r.entry)}\n  ${r.release}: ${first(r.like)}\n`);
  }

  const differs = current !== result.text;
  if (opts.check) console.log(differs ? 'CHANGELOG.md differs from the rebuild' : 'CHANGELOG.md matches the rebuild');
  else if (differs) {
    writeFileSync(path, result.text);
    console.log('wrote CHANGELOG.md');
  } else console.log('CHANGELOG.md already matches the rebuild');

  if ((opts.check && differs) || (opts.strict && (result.review.length > 0 || result.tailDiffers))) process.exitCode = 1;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    main();
  } catch (err) {
    console.error(`changelog-reconcile: ${err.message}`);
    process.exitCode = 2;
  }
}
