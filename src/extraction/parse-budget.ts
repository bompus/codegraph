/** Cooperative deadline for request-time native parsing. */
import type { Language } from '../types';
import { parseSourceTreeSync, type ParsedTree } from './parse-tree';

export const REQUEST_PARSE_BUDGET_MS = 3000;

/** Native parser progress checks stop parsing; external scanners need their own guards. */
export function parseWithinBudget(source: string, language: Language, budgetMs = REQUEST_PARSE_BUDGET_MS): ParsedTree | null {
  return parseSourceTreeSync(source, language, budgetMs);
}
