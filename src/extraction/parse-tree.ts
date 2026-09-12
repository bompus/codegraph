/**
 * One way to get a parse tree (docs/design/kernel-only-extraction-plan.md).
 *
 * Every consumer that needs a tree — the generic extractor, the SFC
 * extractors' blocks, the viewer's highlighter, the branch-guard walker,
 * explore's requested-source ranges — comes through here. The kernel parses
 * (one crossing, the whole CST as flat buffers, see kernel/tree.ts) and the
 * `NativeNode` facade exposes the node surface the walkers were written
 * against. There is no other parser: a host without a kernel binary cannot
 * parse, and `requireKernel` says so with the paths it looked in.
 *
 * {@link TreeNode} is the structural surface the walkers use; `NativeNode`
 * satisfies it. It is kept as an interface (rather than the class) so the
 * walkers stay decoupled from the buffer layout.
 */

import type { Language } from '../types';
import { parseNativeTree } from './kernel/tree';
import { requireKernel } from './kernel/loader';

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
  descendantForPosition(start: TreePoint, end?: TreePoint): TreeNode | null;
  descendantForIndex(start: number, end?: number): TreeNode | null;
  fieldNameForNamedChild(index: number): string | null;
  equals(other: TreeNode | null | undefined): boolean;
  descendantsOfType(types: string | string[], start?: TreePoint, end?: TreePoint): TreeNode[];
}

export interface ParsedTree {
  readonly rootNode: TreeNode;
  /** Kept for the walkers' `try/finally` shape; kernel trees are plain Buffers, so a no-op. */
  delete(): void;
}

/** Parse with the kernel. Null only when the binary carries no grammar for `language`. */
export async function parseSourceTree(source: string, language: Language): Promise<ParsedTree | null> {
  return parseSourceTreeSync(source, language);
}

/** Synchronous form; the kernel path is always synchronous. */
export function parseSourceTreeSync(source: string, language: Language): ParsedTree | null {
  requireKernel();
  return parseNativeTree(source, language) as ParsedTree | null;
}
