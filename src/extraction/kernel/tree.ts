/**
 * Native parse trees for read-time consumers (Phase 3 of
 * docs/design/kernel-only-extraction-plan.md). TS mirror of
 * codegraph-kernel/src/tree.rs — the row layout there is the contract.
 *
 * `parseNativeTree` asks the kernel for the whole CST in one crossing and
 * wraps it in {@link NativeNode}, a facade with the subset of the
 * web-tree-sitter `Node` surface that `syntax-tokens.ts`,
 * `graph/branch-guards.ts` and `mcp/explore-source-ranges.ts` use. Positions
 * come back in UTF-16 units, so `source.slice(startIndex, endIndex)` is exact
 * and `startPosition.column` matches what the wasm parser reported.
 *
 * The facade is deliberately structural: consumers type their nodes as
 * `TreeNode` (src/extraction/parse-tree.ts), which both this class and the
 * wasm `Node` satisfy, so the same walkers run on either tree.
 */

import { getKernel } from './loader';
import type { Language } from '../../types';

export const TREE_ABI_VERSION = 1;
export const TREE_ROW_SIZE = 60;
const NONE = 0xffffffff;
const FLAG_NAMED = 1;
const FLAG_HAS_ERROR = 2;
const FLAG_MISSING = 4;

const ROW = {
  kind: 0, // u16
  flags: 2, // u8
  field: 4, // u16
  parent: 8,
  startByte: 12,
  endByte: 16,
  startRow: 20,
  startCol: 24,
  endRow: 28,
  endCol: 32,
  startIndex: 36,
  endIndex: 40,
  childOff: 44,
  childCount: 48,
  namedChildCount: 52,
  indexInParent: 56,
} as const;

export interface TreePoint {
  row: number;
  column: number;
}

interface TreeData {
  source: string;
  nodes: Buffer;
  children: Buffer;
  kinds: string[];
  fields: string[];
  cache: Map<number, NativeNode>;
}

export class NativeNode {
  constructor(
    private readonly data: TreeData,
    /** Preorder index — also the node's identity. */
    readonly id: number
  ) {}

  private u32(off: number): number {
    return this.data.nodes.readUInt32LE(this.id * TREE_ROW_SIZE + off);
  }
  private u16(off: number): number {
    return this.data.nodes.readUInt16LE(this.id * TREE_ROW_SIZE + off);
  }
  private u8(off: number): number {
    return this.data.nodes.readUInt8(this.id * TREE_ROW_SIZE + off);
  }
  private at(index: number): NativeNode {
    let n = this.data.cache.get(index);
    if (!n) {
      n = new NativeNode(this.data, index);
      this.data.cache.set(index, n);
    }
    return n;
  }

  get type(): string {
    const id = this.u16(ROW.kind);
    // tree-sitter's ERROR symbol is the builtin 0xFFFF, outside the grammar's
    // kind table; web-tree-sitter spells it `ERROR`, and so do the walkers.
    if (id === 0xffff) return 'ERROR';
    return this.data.kinds[id] ?? '';
  }
  get isNamed(): boolean {
    return (this.u8(ROW.flags) & FLAG_NAMED) !== 0;
  }
  get hasError(): boolean {
    return (this.u8(ROW.flags) & FLAG_HAS_ERROR) !== 0;
  }
  get isMissing(): boolean {
    return (this.u8(ROW.flags) & FLAG_MISSING) !== 0;
  }
  get startIndex(): number {
    return this.u32(ROW.startIndex);
  }
  get endIndex(): number {
    return this.u32(ROW.endIndex);
  }
  get startPosition(): TreePoint {
    return { row: this.u32(ROW.startRow), column: this.u32(ROW.startCol) };
  }
  get endPosition(): TreePoint {
    return { row: this.u32(ROW.endRow), column: this.u32(ROW.endCol) };
  }
  get text(): string {
    return this.data.source.slice(this.startIndex, this.endIndex);
  }
  get parent(): NativeNode | null {
    const p = this.u32(ROW.parent);
    return p === NONE ? null : this.at(p);
  }
  get childCount(): number {
    return this.u32(ROW.childCount);
  }
  get namedChildCount(): number {
    return this.u32(ROW.namedChildCount);
  }
  child(i: number): NativeNode | null {
    if (i < 0 || i >= this.childCount) return null;
    const off = this.u32(ROW.childOff) + i;
    return this.at(this.data.children.readUInt32LE(off * 4));
  }
  get children(): NativeNode[] {
    const n = this.childCount;
    const out: NativeNode[] = new Array(n);
    for (let i = 0; i < n; i++) out[i] = this.child(i)!;
    return out;
  }
  get namedChildren(): NativeNode[] {
    const out: NativeNode[] = [];
    const n = this.childCount;
    for (let i = 0; i < n; i++) {
      const c = this.child(i)!;
      if (c.isNamed) out.push(c);
    }
    return out;
  }
  namedChild(i: number): NativeNode | null {
    if (i < 0) return null;
    let seen = 0;
    const n = this.childCount;
    for (let k = 0; k < n; k++) {
      const c = this.child(k)!;
      if (!c.isNamed) continue;
      if (seen === i) return c;
      seen++;
    }
    return null;
  }
  get firstChild(): NativeNode | null {
    return this.child(0);
  }
  get lastChild(): NativeNode | null {
    return this.child(this.childCount - 1);
  }
  get nextSibling(): NativeNode | null {
    const p = this.parent;
    return p ? p.child(this.u32(ROW.indexInParent) + 1) : null;
  }
  get previousSibling(): NativeNode | null {
    const p = this.parent;
    return p ? p.child(this.u32(ROW.indexInParent) - 1) : null;
  }
  get nextNamedSibling(): NativeNode | null {
    let s = this.nextSibling;
    while (s && !s.isNamed) s = s.nextSibling;
    return s;
  }
  get previousNamedSibling(): NativeNode | null {
    let s = this.previousSibling;
    while (s && !s.isNamed) s = s.previousSibling;
    return s;
  }
  /** The field this node fills in its parent, or null. */
  get fieldName(): string | null {
    const f = this.u16(ROW.field);
    return f === 0 ? null : (this.data.fields[f] ?? null);
  }
  fieldNameForChild(index: number): string | null {
    return this.child(index)?.fieldName ?? null;
  }
  childForFieldName(name: string): NativeNode | null {
    const n = this.childCount;
    for (let i = 0; i < n; i++) {
      const c = this.child(i)!;
      if (c.fieldName === name) return c;
    }
    return null;
  }
  childrenForFieldName(name: string): NativeNode[] {
    const out: NativeNode[] = [];
    const n = this.childCount;
    for (let i = 0; i < n; i++) {
      const c = this.child(i)!;
      if (c.fieldName === name) out.push(c);
    }
    return out;
  }
  /** Smallest node spanning the range (web-tree-sitter semantics). */
  descendantForPosition(start: TreePoint, end: TreePoint = start): NativeNode {
    const before = (a: TreePoint, b: TreePoint) => a.row < b.row || (a.row === b.row && a.column <= b.column);
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    let node: NativeNode = this;
    for (;;) {
      let next: NativeNode | null = null;
      const n = node.childCount;
      for (let i = 0; i < n; i++) {
        const c = node.child(i)!;
        if (before(c.startPosition, start) && before(end, c.endPosition)) {
          next = c;
          break;
        }
      }
      if (!next) return node;
      node = next;
    }
  }
  descendantForIndex(start: number, end: number = start): NativeNode {
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    let node: NativeNode = this;
    for (;;) {
      let next: NativeNode | null = null;
      const n = node.childCount;
      for (let i = 0; i < n; i++) {
        const c = node.child(i)!;
        if (c.startIndex <= start && end <= c.endIndex) {
          next = c;
          break;
        }
      }
      if (!next) return node;
      node = next;
    }
  }
  toString(): string {
    return `(${this.type} ${this.startPosition.row}:${this.startPosition.column}-${this.endPosition.row}:${this.endPosition.column})`;
  }
}

export interface NativeTree {
  rootNode: NativeNode;
  /** Matches the wasm `Tree` surface; native memory is plain Buffers, so a no-op. */
  delete(): void;
}

let lastKernel: ReturnType<typeof getKernel> = null;
/** Kind and field name tables, one fetch per language per process. */
const NAME_TABLES = new Map<string, { kinds: string[]; fields: string[] } | null>();

function namesFor(kernel: NonNullable<ReturnType<typeof getKernel>>, language: string) {
  let entry = NAME_TABLES.get(language);
  if (entry !== undefined) return entry;
  entry = null;
  if (typeof kernel.treeNames === 'function') {
    const t = kernel.treeNames(language);
    if (t) {
      const { names: kinds, next } = splitNames(t.names, t.kindCount, 0);
      const { names: fields } = splitNames(t.names, t.fieldCount, next);
      entry = { kinds, fields };
    }
  }
  NAME_TABLES.set(language, entry);
  return entry;
}

function splitNames(buf: Buffer, count: number, from: number): { names: string[]; next: number } {
  const names: string[] = new Array(count);
  let pos = from;
  for (let i = 0; i < count; i++) {
    const nul = buf.indexOf(0, pos);
    names[i] = buf.toString('utf8', pos, nul);
    pos = nul + 1;
  }
  return { names, next: pos };
}

/**
 * Parse with the kernel, or null when it cannot (no binary, language not
 * compiled in, stack guard tripped). Callers fall back to the wasm parser.
 */
export function parseNativeTree(source: string, language: Language): NativeTree | null {
  const kernel = getKernel();
  if (!kernel || typeof kernel.parseTree !== 'function') return null;
  // A fresh kernel instance (tests reset it) must not reuse another's tables.
  if (kernel !== lastKernel) {
    NAME_TABLES.clear();
    lastKernel = kernel;
  }
  let buffers;
  try {
    buffers = kernel.parseTree(source, language);
  } catch {
    return null;
  }
  const meta = buffers.meta;
  if (meta.readUInt32LE(0) !== TREE_ABI_VERSION) return null;
  const names = namesFor(kernel, language);
  if (!names) return null;
  const data: TreeData = {
    source,
    nodes: buffers.nodes,
    children: buffers.children,
    kinds: names.kinds,
    fields: names.fields,
    cache: new Map(),
  };
  const rootNode = new NativeNode(data, 0);
  data.cache.set(0, rootNode);
  return { rootNode, delete() {} };
}
