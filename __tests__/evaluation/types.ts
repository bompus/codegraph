import type { NodeKind } from '../../src/types.js';

export interface EvalTestCase {
  id: string;
  query: string;
  api: 'searchNodes' | 'findRelevantContext';
  expectedSymbols: string[];
  kinds?: NodeKind[];
  options?: Record<string, unknown>;
}

export interface EvalResult {
  caseId: string;
  pass: boolean;
  recall: number;
  mrr: number;
  foundSymbols: string[];
  missedSymbols: string[];
  nodeCount?: number;
  edgeCount?: number;
  edgeDensity?: number;
  latencyMs: number;
}

export interface EvalReport {
  timestamp: string;
  codebasePath: string;
  codegraphSha: string;
  summary: {
    total: number;
    passed: number;
    failed: number;
    meanRecall: number;
    meanMRR: number;
  };
  results: EvalResult[];
}

// ---------------------------------------------------------------------------
// Precision: known-wrong and known-right edges on pinned real repositories
// (docs/design/resolution-binding-model-plan.md, Phase 0). The recall cases
// above cannot see a false edge; these can.
// ---------------------------------------------------------------------------

export interface EdgeEndpoint {
  /** Suffix of the file path as stored in the graph (matched with endsWith). */
  file: string;
  name: string;
}

export interface EdgeCase {
  id: string;
  /** Corpus key — see PRECISION_CORPORA in edge-cases.ts. */
  corpus: string;
  kind: 'calls' | 'imports' | 'references';
  /** Omitted = any source node (the PR named only the wrong target). */
  from?: EdgeEndpoint;
  to: EdgeEndpoint;
  /** `absent`: a known-wrong edge that must stay gone. `present`: a known-right control. */
  expect: 'absent' | 'present';
  /** Which PR or issue established it. */
  source: string;
  why: string;
}

export interface EdgeCaseResult {
  caseId: string;
  expect: 'absent' | 'present';
  /** Whether the graph matches the expectation. */
  pass: boolean;
  /** Edges found between the endpoints (empty when none). */
  found: Array<{ from: string; to: string; resolvedBy?: string }>;
  /** Endpoints that did not resolve to any node at all — the case cannot be judged. */
  missingEndpoints: string[];
}

export interface PrecisionReport {
  timestamp: string;
  corpus: string;
  repo: string;
  commit: string;
  codegraphSha: string;
  summary: {
    absentCases: number;
    absentHeld: number;
    presentCases: number;
    presentHeld: number;
    unjudgeable: number;
  };
  /** Resolved-edge histogram by resolver, for LOST/GAINED-style comparison across builds. */
  resolvedBy: Record<string, number>;
  edges: number;
  results: EdgeCaseResult[];
}
