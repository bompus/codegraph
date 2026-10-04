/**
 * A time-capped pass over the source text of indexed files, for the questions
 * the graph cannot answer: whether a name appears anywhere (an object key, a
 * template binding) and where a quoted phrase sits. Reads only files the index
 * tracks. The native reader verifies the opened descriptor before reading content.
 */
import { getKernel } from '../extraction/kernel/loader';

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
  /**
   * The scan stops, incomplete, before a file that would take it past this
   * many bytes. Every byte read counts, including a skipped file's. A read
   * may use one extra byte to detect growth, then the scan stops.
   */
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
  const Reader = getKernel()?.ContainedSourceReader;
  if (!Reader) return { complete: false, skipped: 0 };
  let reader: InstanceType<NonNullable<typeof Reader>>;
  try { reader = new Reader(projectRoot); }
  catch { return { complete: false, skipped: files.length }; }
  let skipped = 0;
  let total = 0;
  for (const file of files) {
    if (total > limits.maxTotalBytes) return { complete: false, skipped };
    if (Date.now() > deadline) return { complete: false, skipped };
    let text: string;
    try {
      const result = reader.read(file.path,
        Math.min(limits.maxFileBytes, MAX_SCAN_TOTAL_BYTES),
        Math.max(0, Math.min(limits.maxTotalBytes - total, MAX_SCAN_TOTAL_BYTES)));
      total += result.bytesRead;
      if (result.limitReached) return { complete: false, skipped };
      if (result.content == null) { skipped++; continue; }
      text = result.content.toString('utf8');
    } catch {
      skipped++;
      continue;
    }
    pattern.lastIndex = 0;
    for (const m of text.matchAll(pattern)) {
      if (onMatch(file.path, m.index ?? 0, m[0])) return { complete: true, skipped };
    }
  }
  return { complete: true, skipped };
}
