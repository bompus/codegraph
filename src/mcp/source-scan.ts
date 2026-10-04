/**
 * A time-capped pass over the source text of indexed files, for the questions
 * the graph cannot answer: whether a name appears anywhere (an object key, a
 * template binding) and where a quoted phrase sits. Reads only files the index
 * tracks, under the same root containment as every other explore read.
 */
import { closeSync, fstatSync, openSync, readFileSync } from 'fs';
import { rootContainmentCheck } from '../utils';

/**
 * Limits on what a warm scan reads inside a 300 ms budget, with room to spare.
 * Cost is about 12 microseconds per file plus 1 ms per MB: 13,609 TypeScript
 * files (39.5 MB) took 175 ms, 2,174 C files (47.4 MB) took 55 ms. A larger
 * project skips the scan, because a scan that runs out of time costs the whole
 * budget and answers nothing.
 */
export const MAX_SCAN_FILES = 16_000;
export const MAX_SCAN_TOTAL_BYTES = 64 * 1024 * 1024;

/** Whether a scan of `files` should fit its budget; see {@link MAX_SCAN_FILES}. */
export function scanFits(files: readonly ScanFile[]): boolean {
  if (files.length > MAX_SCAN_FILES) return false;
  let total = 0;
  for (const f of files) total += f.size;
  return total <= MAX_SCAN_TOTAL_BYTES;
}

export interface ScanFile {
  path: string;
  /** Size recorded at index time. */
  size: number;
}

export interface ScanLimits {
  budgetMs: number;
  /** A file larger than this when opened is skipped. */
  maxFileBytes: number;
  /** The scan stops, incomplete, once it has read this many bytes. */
  maxTotalBytes: number;
}

export interface ScanResult {
  /** False when the budget or the byte total ran out before every file was read. */
  complete: boolean;
  /** Files not read: outside the root, unreadable, or over `maxFileBytes`. */
  skipped: number;
}

/**
 * Calls `onMatch` for every match of the global `pattern` in `files`, in
 * order, until it returns true or a limit runs out. Sizes are checked on the
 * opened file, so a file that grew since indexing cannot overrun them.
 */
export function scanIndexedSource(
  projectRoot: string,
  files: readonly ScanFile[],
  pattern: RegExp,
  limits: ScanLimits,
  onMatch: (filePath: string, offset: number, text: string) => boolean,
): ScanResult {
  if (!pattern.global) throw new Error('scanIndexedSource needs a global pattern');
  const deadline = Date.now() + limits.budgetMs;
  const contained = rootContainmentCheck(projectRoot);
  let skipped = 0;
  let total = 0;
  for (const file of files) {
    if (Date.now() > deadline) return { complete: false, skipped };
    const absolute = contained(file.path);
    if (!absolute) { skipped++; continue; }
    let text: string;
    let fd: number | undefined;
    try {
      fd = openSync(absolute, 'r');
      const size = fstatSync(fd).size;
      if (size > limits.maxFileBytes) { skipped++; continue; }
      total += size;
      if (total > limits.maxTotalBytes) return { complete: false, skipped };
      text = readFileSync(fd, 'utf8');
    } catch {
      skipped++;
      continue;
    } finally {
      if (fd !== undefined) closeSync(fd);
    }
    pattern.lastIndex = 0;
    for (const m of text.matchAll(pattern)) {
      if (onMatch(file.path, m.index ?? 0, m[0])) return { complete: true, skipped };
    }
  }
  return { complete: true, skipped };
}
