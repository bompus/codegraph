/**
 * A time-capped pass over the source text of indexed files, for the questions
 * the graph cannot answer: whether a name appears anywhere (an object key, a
 * template binding) and where a quoted phrase sits. Reads only files the index
 * tracks, under the same root containment as every other explore read.
 */
import { closeSync, fstatSync, readSync } from 'fs';
import { rootContainedOpener } from '../utils';

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
  /**
   * True when every file was read, or `onMatch` stopped the scan. The budget
   * is checked before each file, so the last read can end past it; its text
   * is still matched, because the answer is complete and the time is spent.
   */
  complete: boolean;
  /** Files not read: outside the root, unreadable, over `maxFileBytes`, or grown while read. */
  skipped: number;
}

/**
 * Calls `onMatch` for every match of the global `pattern` in `files`, in
 * order, until it returns true or a limit runs out. Sizes are checked on the
 * opened file, and the read stops one byte past that size, so a file that grew
 * since indexing, or grows while it is read, cannot overrun them.
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
  const open = rootContainedOpener(projectRoot);
  let skipped = 0;
  let total = 0;
  for (const file of files) {
    if (Date.now() > deadline) return { complete: false, skipped };
    const fd = open(file.path);
    if (fd === null) { skipped++; continue; }
    let text: string;
    try {
      const size = fstatSync(fd).size;
      if (size > limits.maxFileBytes) { skipped++; continue; }
      if (total + size > limits.maxTotalBytes) return { complete: false, skipped };
      const buf = Buffer.allocUnsafe(size + 1);
      let read = 0;
      while (read < buf.length) {
        const n = readSync(fd, buf, read, buf.length - read, null);
        if (n === 0) break;
        read += n;
      }
      if (read > size) { skipped++; continue; }
      total += read;
      text = buf.toString('utf8', 0, read);
    } catch {
      skipped++;
      continue;
    } finally {
      closeSync(fd);
    }
    pattern.lastIndex = 0;
    for (const m of text.matchAll(pattern)) {
      if (onMatch(file.path, m.index ?? 0, m[0])) return { complete: true, skipped };
    }
  }
  return { complete: true, skipped };
}
