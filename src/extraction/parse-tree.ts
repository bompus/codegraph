/**
 * One way to get a parse tree at read time (Phase 3 of
 * docs/design/kernel-only-extraction-plan.md).
 *
 * The viewer's highlighter, the branch-guard walker and explore's
 * requested-source ranges all parse a file on request and walk the tree.
 * They used to call `getParser(language).parse(source)` — the wasm parser,
 * always. `parseSourceTree` asks the kernel first (one crossing, the whole
 * tree as flat buffers — see kernel/tree.ts) and falls back to the wasm
 * parser only when the kernel cannot serve the language. The fallback goes
 * away with the wasm path (Phase 5).
 *
 * {@link TreeNode} is the structural surface the three consumers use. Both
 * the kernel facade (`NativeNode`) and web-tree-sitter's `Node` satisfy it,
 * so the walkers are written once and run on either tree.
 */

import type { Language } from '../types';
import { getParser, loadGrammarsForLanguages } from './grammars';
import { parseNativeTree } from './kernel/tree';
import { kernelSupports } from './kernel/loader';

export interface TreePoint {
  row: number;
  column: number;
}

/** The node surface shared by the wasm `Node` and the kernel facade. */
export interface TreeNode {
  readonly id: number;
  readonly type: string;
  readonly isNamed: boolean;
  readonly hasError: boolean;
  readonly text: string;
  readonly startIndex: number;
  readonly endIndex: number;
  readonly startPosition: TreePoint;
  readonly endPosition: TreePoint;
  readonly parent: TreeNode | null;
  readonly childCount: number;
  readonly namedChildCount: number;
  readonly children: TreeNode[];
  readonly namedChildren: TreeNode[];
  readonly firstChild: TreeNode | null;
  readonly lastChild: TreeNode | null;
  readonly nextSibling: TreeNode | null;
  readonly previousSibling: TreeNode | null;
  readonly nextNamedSibling: TreeNode | null;
  readonly previousNamedSibling: TreeNode | null;
  child(index: number): TreeNode | null;
  namedChild(index: number): TreeNode | null;
  childForFieldName(name: string): TreeNode | null;
  fieldNameForChild(index: number): string | null;
  childrenForFieldName(name: string): TreeNode[];
  descendantForPosition(start: TreePoint, end?: TreePoint): TreeNode;
  descendantForIndex(start: number, end?: number): TreeNode;
}

export interface ParsedTree {
  readonly rootNode: TreeNode;
  /** Release native memory (wasm trees); a no-op for kernel trees. */
  delete(): void;
}

/** Kernel first, wasm second. Null when neither can parse the language. */
export async function parseSourceTree(source: string, language: Language): Promise<ParsedTree | null> {
  const native = tryNative(source, language);
  if (native) return native;
  try {
    await loadGrammarsForLanguages([language]);
  } catch {
    return null;
  }
  return parseWasm(source, language);
}

/**
 * Synchronous twin for callers that cannot await. The kernel path is always
 * synchronous; the wasm path only serves a grammar that is ALREADY loaded.
 */
export function parseSourceTreeSync(source: string, language: Language): ParsedTree | null {
  return tryNative(source, language) ?? parseWasm(source, language);
}

function tryNative(source: string, language: Language): ParsedTree | null {
  if (process.env.CODEGRAPH_KERNEL === '0') return null;
  if (!kernelSupports(language)) return null;
  return parseNativeTree(source, language) as ParsedTree | null;
}

function parseWasm(source: string, language: Language): ParsedTree | null {
  try {
    const parser = getParser(language);
    if (!parser) return null;
    const tree = parser.parse(source);
    if (!tree?.rootNode) return null;
    return tree as unknown as ParsedTree;
  } catch {
    return null;
  }
}
