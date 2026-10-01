import { it, expect } from 'vitest';
import { parseWithinBudget } from '../src/extraction/parse-budget';

it('cancels request parsing and reuses the parser for a different document', () => {
  expect(parseWithinBudget('const oldValue = 1;\n'.repeat(20_000), 'typescript', 1)).toBeNull();
  const tree = parseWithinBudget('const freshValue = 2;', 'typescript');
  expect(tree).not.toBeNull();
  expect(tree!.rootNode.text).toBe('const freshValue = 2;');
  expect(tree!.rootNode.descendantsOfType('identifier').map((node) => node.text)).toContain('freshValue');
  tree!.delete();
});
