/**
 * How far a resolved edge can be trusted, for the surfaces that render one.
 *
 * The resolver records the strategy that bound each reference (`resolvedBy`)
 * and a confidence. Most strategies prove the target: an import binding, a
 * qualified name, a receiver whose declared type was read. A few only match
 * the callee's name — exact-name and fuzzy matches below their unique-hit
 * confidence, and the method-name scoring at the end of the method-call arm.
 * On repowise's graded call edges those name-only hops were right 19 times in
 * 35, against 25 in 27 for every other strategy
 * (docs/benchmarks/precision-replay-2026-09.md). Labelling them lets an agent
 * check the one uncertain hop instead of re-reading the flow around it.
 */

import type { Edge } from '../types';

/** Exact-name / fuzzy hits below this bound matched a name, not a definition. */
const NAME_MATCH_PROVEN_AT = 0.9;
/** Method-call hits below this bound came from method-name scoring, not a typed receiver. */
const METHOD_CALL_PROVEN_AT = 0.8;

/**
 * True when `edge` was bound by the callee's name alone. Synthesized edges are
 * never name guesses — they carry their own mechanism label.
 */
export function isNameGuess(edge: Edge | null | undefined): boolean {
  if (!edge || edge.provenance === 'heuristic') return false;
  const meta = (edge.metadata ?? {}) as Record<string, unknown>;
  const confidence = meta.confidence;
  if (typeof confidence !== 'number') return false;
  switch (meta.resolvedBy) {
    case 'exact-match':
    case 'fuzzy':
      return confidence < NAME_MATCH_PROVEN_AT;
    case 'instance-method':
      return confidence < METHOD_CALL_PROVEN_AT;
    default:
      return false;
  }
}
