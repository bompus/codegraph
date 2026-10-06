/** Fold embedded scripts into one SFC file/component hierarchy, preserving nested owners. */

import type { Binding, Edge, ExtractionError, ExtractionResult, Language, Node, UnresolvedReference } from '../types';
import * as path from 'path';

/** The SFC's own file node — the whole file, as the tree-sitter extractor makes one. */
export function sfcFileNode(filePath: string, source: string, language: Language): Node {
  return {
    id: `file:${filePath}`,
    kind: 'file',
    name: path.basename(filePath),
    qualifiedName: filePath,
    filePath,
    language,
    startLine: 1,
    endLine: source.split('\n').length,
    startColumn: 0,
    endColumn: 0,
    isExported: false,
    updatedAt: Date.now(),
  };
}

export interface ScriptFold {
  filePath: string;
  componentNodeId: string;
  /** Lines before the block in the SFC file: the block's line 1 is `lineOffset + 1`. */
  lineOffset: number;
  language: Language;
  /** The block runs once per component instance — Vue `<script setup>`, a Svelte instance script, Astro frontmatter. */
  perInstance: boolean;
}

export interface ScriptSink {
  nodes: Node[];
  edges: Edge[];
  unresolvedReferences: UnresolvedReference[];
  errors: ExtractionError[];
  bindings: Binding[];
}

/** What a per-instance block does at its top level, as opposed to what it imports. */
const INSTANCE_REF_KINDS: ReadonlySet<string> = new Set(['calls', 'references', 'instantiates', 'function_ref']);

export function foldScriptResult(result: ExtractionResult, fold: ScriptFold, sink: ScriptSink): void {
  const blockFile = `file:${fold.filePath}`;
  // Nested symbols retain their script parent; top-level symbols belong to the component.
  const parented = new Set<string>();
  for (const edge of result.edges) {
    if (edge.kind === 'contains' && edge.source !== blockFile) parented.add(edge.target);
  }

  // A per-instance block's top-level `const data = useFetch(…)` is component
  // state: its initializer runs on every instance, as the component's doing.
  const instanceValues = new Set<string>();
  if (fold.perInstance) {
    for (const node of result.nodes) {
      if ((node.kind === 'constant' || node.kind === 'variable') && !parented.has(node.id)) instanceValues.add(node.id);
    }
  }
  const runsAsComponent = (id: string) => id === blockFile || instanceValues.has(id);

  for (const node of result.nodes) {
    if (node.kind === 'file') continue;
    node.startLine += fold.lineOffset;
    node.endLine += fold.lineOffset;
    node.language = fold.language;
    sink.nodes.push(node);
    if (!parented.has(node.id)) sink.edges.push({ source: fold.componentNodeId, target: node.id, kind: 'contains' });
  }

  for (const edge of result.edges) {
    if (edge.kind === 'contains' && edge.source === blockFile) continue;
    if (edge.line) edge.line += fold.lineOffset;
    // What a top-level value DOES is the component's; what it HOLDS stays its
    // own — `const api = { load() {…} }` keeps `api::load` (#2300).
    if (fold.perInstance && runsAsComponent(edge.source) && edge.kind !== 'imports' && edge.kind !== 'contains') {
      edge.source = fold.componentNodeId;
    }
    sink.edges.push(edge);
  }

  for (const ref of result.unresolvedReferences) {
    ref.line += fold.lineOffset;
    ref.filePath = fold.filePath;
    ref.language = fold.language;
    if (fold.perInstance && runsAsComponent(ref.fromNodeId) && INSTANCE_REF_KINDS.has(ref.referenceKind)) {
      ref.fromNodeId = fold.componentNodeId;
    }
    sink.unresolvedReferences.push(ref);
  }

  for (const binding of result.bindings ?? []) {
    sink.bindings.push({
      ...binding,
      filePath: fold.filePath,
      scopeStart: binding.scopeStart + fold.lineOffset,
      scopeEnd: binding.scopeEnd + fold.lineOffset,
      line: binding.line + fold.lineOffset,
    });
  }

  for (const error of result.errors) {
    if (error.line) error.line += fold.lineOffset;
    sink.errors.push(error);
  }
}
