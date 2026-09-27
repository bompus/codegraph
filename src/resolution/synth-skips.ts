/**
 * Files a synthesis pass skipped for reasons that depend only on their bytes.
 *
 * Most synthesis passes read every file and drop almost all of them on a
 * content-only test (no `x[k](` for the registry pass, and so on). Recording
 * those skips with the hash of the bytes the pass read lets a later run skip
 * the same files without reading them. A skip applies only while that hash
 * equals the file's current indexed hash, so the result stays exactly what a
 * full run would produce: an edited, reverted or deleted file is scanned
 * again. Only content-only decisions may be recorded; a skip that depends on
 * the graph or on another file must not use this.
 */

import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { CodeGraphPackageVersion } from '../mcp/version';

/**
 * The code that owns the rows. A local or development build keeps the package
 * version while its pass logic changes, so the version alone would let a new
 * pass reuse skips an older one decided. The fingerprint of this directory's
 * compiled modules, where every synthesis pass and framework detector lives,
 * changes with any edit.
 */
function codeFingerprint(): string {
  const hash = crypto.createHash('sha256');
  const walk = (dir: string, rel: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name, 'en'))) {
      if (entry.isDirectory()) walk(path.join(dir, entry.name), `${rel}${entry.name}/`);
      else if (/\.(?:js|ts)$/.test(entry.name)) {
        hash.update(rel + entry.name);
        hash.update(fs.readFileSync(path.join(dir, entry.name)));
      }
    }
  };
  try {
    walk(__dirname, '');
  } catch {
    // Unreadable sources: a random stamp, so no skips are ever reused.
    hash.update(String(Math.random()));
  }
  return hash.digest('hex').slice(0, 16);
}

/** The build that owns the rows: a different build's skip decisions may differ. */
export const SYNTH_SKIPS_VERSION = `${CodeGraphPackageVersion}+${codeFingerprint()}`;

export class SynthSkips {
  private readonly recorded: Array<[string, string, string]> = [];
  private readonly hashes = new Map<string, string>();

  constructor(private readonly known: Map<string, Set<string>>) {}

  /** True when `pass` skipped `file` at its current indexed content. */
  has(pass: string, file: string): boolean {
    return this.known.get(pass)?.has(file) ?? false;
  }

  /** Record that `pass` skipped `file` after reading exactly `content`. */
  record(pass: string, file: string, content: string): void {
    let hash = this.hashes.get(file);
    if (hash === undefined) {
      // Same digest as the files table's content_hash (extraction hashContent).
      hash = crypto.createHash('sha256').update(content).digest('hex');
      this.hashes.set(file, hash);
    }
    this.recorded.push([pass, file, hash]);
  }

  /** Rows recorded during this run, for the writer to persist. */
  take(): Array<[string, string, string]> {
    return this.recorded.splice(0);
  }
}

/** Whether an earlier run already skipped `file` for `pass` at its current content. */
export function skipped(ctx: { synthSkips?: SynthSkips }, pass: string, file: string): boolean {
  return ctx.synthSkips?.has(pass, file) ?? false;
}

/** Record a content-only skip; an unreadable file (null) is never recorded. */
export function recordSkip(ctx: { synthSkips?: SynthSkips }, pass: string, file: string, content: string | null): void {
  if (content !== null) ctx.synthSkips?.record(pass, file, content);
}
