/**
 * A kernel over the live db either loads the whole node table or answers its
 * node lookups by query. The load costs per node and the queries per ref, so
 * the choice depends on both: a small project's full index must load the table
 * (querying cost gin about 60 ms of a 1 s index), while a one-file sync on a
 * large project must still query (the load is about 150 ms on ktor).
 */
import { describe, it, expect } from 'vitest';
import { ReferenceResolver } from '../src/resolution/index';

function queriesFor(nodeCount: number): ReferenceResolver {
  const resolver = Object.create(ReferenceResolver.prototype) as ReferenceResolver;
  (resolver as unknown as { queries: { getNodeCount(): number } }).queries = { getNodeCount: () => nodeCount };
  return resolver;
}

const shouldQuery = (nodeCount: number, refs: number): boolean =>
  (queriesFor(nodeCount) as unknown as { shouldQueryLookups(n: number): boolean }).shouldQueryLookups(refs);

describe('kernel node lookups: table load or query', () => {
  it('loads the table for a small project full index', () => {
    expect(shouldQuery(3_089, 13_643)).toBe(false); // gin
    expect(shouldQuery(5_614, 22_684)).toBe(false); // Alamofire
  });

  it('queries for a one-file sync on a small project', () => {
    expect(shouldQuery(3_089, 60)).toBe(true);
  });

  it('queries below the crossover on a large project and loads above it', () => {
    expect(shouldQuery(49_708, 4_900)).toBe(true); // ktor, 4.9k refs: query 302 ms vs table 375 ms
    expect(shouldQuery(49_708, 36_015)).toBe(false); // above 15,000 refs
  });
});
