/**
 * Finding the few lines of a large JSONL transcript that can hold prose without
 * decoding and parsing the rest. Most of a transcript is tool traffic (about 95%
 * of the entries on a real history); `Buffer.indexOf` searches bytes natively,
 * so locating the marked lines costs a third of splitting and parsing every one.
 */

/** Transcripts below this are parsed whole: they cost little and the scan saves nothing. */
export const SCAN_MIN_BYTES = 256 * 1024;

export interface LineScan {
  /** Byte patterns that mark a line as a candidate, in compact JSON. */
  markers: readonly Buffer[];
  /** A marker that keeps a line only when `keep` says so (given the line's byte range). */
  conditional?: { marker: Buffer; keep: (buf: Buffer, start: number, end: number) => boolean };
  /** The same keys written with spaces: the compact-JSON assumption no longer holds. */
  spaced: readonly Buffer[];
}

export const bytes = (...patterns: string[]): Buffer[] => patterns.map((p) => Buffer.from(p));

/**
 * The marked lines of `buf` in file order, decoded. Null when the scan cannot
 * be trusted and the caller must read every line: spaced JSON, or no marker at
 * all in a file this large.
 */
export function scanMarkedLines(buf: Buffer, scan: LineScan): string[] | null {
  if (scan.spaced.some((m) => buf.includes(m))) return null;
  const starts = new Set<number>();
  const collect = (marker: Buffer, keep?: (start: number, end: number) => boolean): void => {
    let at = buf.indexOf(marker);
    while (at !== -1) {
      const start = buf.lastIndexOf(10, at) + 1;
      let end = buf.indexOf(10, at);
      if (end === -1) end = buf.length;
      if (!keep || keep(start, end)) starts.add(start);
      at = buf.indexOf(marker, end);
    }
  };
  for (const marker of scan.markers) collect(marker);
  if (scan.conditional) collect(scan.conditional.marker, (s, e) => scan.conditional!.keep(buf, s, e));
  if (starts.size === 0) return null;
  return [...starts]
    .sort((a, b) => a - b)
    .map((start) => {
      let end = buf.indexOf(10, start);
      if (end === -1) end = buf.length;
      return buf.toString('utf8', start, end);
    });
}

/** `buf` as lines to parse: the marked ones when it is large enough to scan and the scan holds. */
export function transcriptLines(buf: Buffer, scan: LineScan): string[] {
  return (buf.length >= SCAN_MIN_BYTES ? scanMarkedLines(buf, scan) : null) ?? buf.toString('utf8').split('\n');
}
