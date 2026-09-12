import type { EvalResult, EdgeCase, EdgeCaseResult, EdgeEndpoint } from './types.js';

export const PASS_THRESHOLD = 0.5;

export function scoreSearchNodes(
  caseId: string,
  expectedSymbols: string[],
  results: Array<{ node: { name: string }; score: number }>,
  latencyMs: number
): EvalResult {
  const expectedLower = expectedSymbols.map((s) => s.toLowerCase());
  const resultNames = results.map((r) => r.node.name.toLowerCase());

  const found: string[] = [];
  const missed: string[] = [];
  let firstRank = 0;

  for (let i = 0; i < expectedLower.length; i++) {
    const idx = resultNames.indexOf(expectedLower[i]);
    if (idx !== -1) {
      found.push(expectedSymbols[i]);
      if (firstRank === 0) firstRank = idx + 1;
    } else {
      missed.push(expectedSymbols[i]);
    }
  }

  const recall = expectedSymbols.length > 0 ? found.length / expectedSymbols.length : 0;
  const mrr = firstRank > 0 ? 1 / firstRank : 0;

  return {
    caseId,
    pass: recall >= PASS_THRESHOLD,
    recall,
    mrr,
    foundSymbols: found,
    missedSymbols: missed,
    latencyMs,
  };
}

export function scoreFindRelevantContext(
  caseId: string,
  expectedSymbols: string[],
  subgraph: { nodes: Map<string, { name: string }>; edges: unknown[]; roots: string[] },
  latencyMs: number
): EvalResult {
  const expectedLower = new Set(expectedSymbols.map((s) => s.toLowerCase()));
  const nodeNames = new Set<string>();
  for (const node of subgraph.nodes.values()) {
    nodeNames.add(node.name.toLowerCase());
  }

  const found: string[] = [];
  const missed: string[] = [];

  for (const sym of expectedSymbols) {
    if (nodeNames.has(sym.toLowerCase())) {
      found.push(sym);
    } else {
      missed.push(sym);
    }
  }

  const recall = expectedSymbols.length > 0 ? found.length / expectedSymbols.length : 0;
  const nodeCount = subgraph.nodes.size;
  const edgeCount = subgraph.edges.length;
  const edgeDensity = nodeCount > 0 ? edgeCount / nodeCount : 0;

  return {
    caseId,
    pass: recall >= PASS_THRESHOLD,
    recall,
    mrr: 0,
    foundSymbols: found,
    missedSymbols: missed,
    nodeCount,
    edgeCount,
    edgeDensity,
    latencyMs,
  };
}

/**
 * Precision scoring (docs/design/resolution-binding-model-plan.md, Phase 0).
 *
 * The recall scorers above ask "did the expected symbols show up"; nothing in
 * them penalizes a wrong edge, which is exactly what the resolution PR chain
 * (#1713, #1718, #1746, #1844) exists to remove. This scores an {@link EdgeCase}
 * against a graph: for an `absent` case the edge must not exist between any
 * node pair matching the endpoints; for a `present` control it must.
 */
export function scoreEdgeCase(
  edgeCase: EdgeCase,
  graph: {
    getNodesByName(name: string): Array<{ id: string; name: string; filePath: string }>;
    getOutgoingEdgesFrom(ids: readonly string[], kinds?: Array<EdgeCase['kind']>): Array<{ source: string; target: string; metadata?: Record<string, unknown> | null }>;
    getIncomingEdgesTo(ids: readonly string[], kinds?: Array<EdgeCase['kind']>): Array<{ source: string; target: string; metadata?: Record<string, unknown> | null }>;
    getNode(id: string): { id: string; name: string; filePath: string } | null;
  }
): EdgeCaseResult {
  const pick = (ep: EdgeEndpoint) =>
    graph.getNodesByName(ep.name).filter((n) => n.filePath.endsWith(ep.file) || n.filePath.replace(/\\/g, '/').endsWith(ep.file));
  const fromNodes = edgeCase.from ? pick(edgeCase.from) : null;
  const toNodes = pick(edgeCase.to);
  const missingEndpoints: string[] = [];
  if (fromNodes && fromNodes.length === 0) missingEndpoints.push(`from ${edgeCase.from!.file}:${edgeCase.from!.name}`);
  if (toNodes.length === 0) missingEndpoints.push(`to ${edgeCase.to.file}:${edgeCase.to.name}`);

  const found: EdgeCaseResult['found'] = [];
  if (toNodes.length && (fromNodes === null || fromNodes.length)) {
    const toIds = new Set(toNodes.map((n) => n.id));
    const edges = fromNodes
      ? graph.getOutgoingEdgesFrom(fromNodes.map((n) => n.id), [edgeCase.kind]).filter((e) => toIds.has(e.target))
      : graph.getIncomingEdgesTo([...toIds], [edgeCase.kind]);
    for (const e of edges) {
      const meta = (e.metadata ?? {}) as { resolvedBy?: string };
      found.push({ from: e.source, to: e.target, resolvedBy: meta.resolvedBy });
    }
  }

  // A missing FROM endpoint makes an `absent` case vacuous and a `present`
  // case a failure; a missing TO endpoint is fine for `absent` (the wrong
  // target may simply not exist at this commit) and a failure for `present`.
  let pass: boolean;
  if (edgeCase.expect === 'absent') pass = found.length === 0 && (fromNodes === null || fromNodes.length > 0);
  else pass = found.length > 0;
  return { caseId: edgeCase.id, expect: edgeCase.expect, pass, found, missingEndpoints };
}
