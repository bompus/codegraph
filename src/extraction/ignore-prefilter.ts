/**
 * A no-match fast path for an `ignore` matcher.
 *
 * `ignore` tests a path against every rule, one regex at a time, even though
 * almost every path in a scan matches none of them. With the defaults plus a
 * large `.gitignore` (170 rules on n8n) that is most of the scan filter's cost.
 * One regex joining every rule answers "does anything match?" in a single test;
 * only a path that some rule matches goes through the stock rule walk, which
 * still decides negations and order. A path no rule matches gets the stock
 * result for that case (`{ ignored: false, unignored: false }`), so the answer
 * is unchanged.
 *
 * This reads `ignore`'s rule list, which is not public API. When that shape is
 * absent (a different major), or the rules can't share one regex, the matcher
 * is left untouched.
 */
import type { Ignore } from 'ignore';

interface Rule { regex: RegExp }
interface RuleManager {
  _rules: Rule[];
  test(path: string, checkUnignored: boolean, mode: string): { ignored: boolean; unignored: boolean };
}

const MODE_IGNORE = 'regex';
const NO_MATCH = Object.freeze({ ignored: false, unignored: false });

export function withNoMatchFastPath(ig: Ignore): Ignore {
  const manager = (ig as unknown as { _rules?: RuleManager })._rules;
  if (!manager || !Array.isArray(manager._rules) || typeof manager.test !== 'function') return ig;
  const stock = manager.test.bind(manager);
  // Rules are added after construction (`add`), so rebuild when the count moves.
  let builtFor = -1;
  let any: RegExp | null = null;
  manager.test = (path, checkUnignored, mode) => {
    if (mode !== MODE_IGNORE) return stock(path, checkUnignored, mode);
    if (builtFor !== manager._rules.length) {
      builtFor = manager._rules.length;
      any = joinRules(manager._rules);
    }
    if (any && !any.test(path)) return { ...NO_MATCH };
    return stock(path, checkUnignored, mode);
  };
  return ig;
}

function joinRules(rules: Rule[]): RegExp | null {
  if (rules.length === 0) return null;
  const regexes = rules.map((r) => r.regex);
  if (!regexes.every((r) => r instanceof RegExp)) return null;
  const flags = regexes[0]!.flags;
  // Joined alternatives share one flag set, and a backreference would point
  // at the wrong group once numbered in the joined source.
  if (regexes.some((r) => r.flags !== flags || /\\[1-9]|\\k</.test(r.source))) return null;
  return new RegExp(regexes.map((r) => `(?:${r.source})`).join('|'), flags);
}
