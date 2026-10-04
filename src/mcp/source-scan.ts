/**
 * A time-capped pass over the source text of indexed files, for the questions
 * the graph cannot answer: whether a name appears anywhere (an object key, a
 * template binding) and where a quoted phrase sits. Reads only files the index
 * tracks, under the same root containment as every other explore read.
 */
import { readFileSync } from 'fs';
import { rootContainmentCheck } from '../utils';

/** Larger files are generated or data; a phrase or name there is not an answer. */
const MAX_SCAN_BYTES = 2 * 1024 * 1024;

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

/**
 * Calls `onMatch` for every match of the global `pattern` in `files`, in
 * order, until it returns true or `budgetMs` runs out. Returns false only when
 * the budget ran out first.
 */
export function scanIndexedSource(
  projectRoot: string,
  files: readonly ScanFile[],
  pattern: RegExp,
  budgetMs: number,
  onMatch: (filePath: string, offset: number, text: string) => boolean,
): boolean {
  if (!pattern.global) throw new Error('scanIndexedSource needs a global pattern');
  const deadline = Date.now() + budgetMs;
  const contained = rootContainmentCheck(projectRoot);
  for (const file of files) {
    if (Date.now() > deadline) return false;
    if (file.size > MAX_SCAN_BYTES) continue;
    const absolute = contained(file.path);
    if (!absolute) continue;
    let text: string;
    try {
      text = readFileSync(absolute, 'utf8');
    } catch {
      continue;
    }
    pattern.lastIndex = 0;
    for (const m of text.matchAll(pattern)) {
      if (onMatch(file.path, m.index ?? 0, m[0])) return true;
    }
  }
  return true;
}
