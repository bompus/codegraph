import type { Language, Node } from '../types';
import { parseSourceTree, type TreeNode as SyntaxNode } from '../extraction/parse-tree';
import { seedLiteralsInQuery } from '../extraction/literal-capture';
import { extractSearchTerms, isTestPath } from '../search/query-utils';
import { QUOTED_SPAN } from './source-scan';

export interface RequestedSourceRange {
  start: number;
  end: number;
  name: string;
  score: number;
  nodeId?: string;
}

/** Source evidence inside an already selected file; never adds graph nodes or edges. */
export async function requestedSourceRanges(
  filePath: string, source: string, language: Language, query: string, nodes: readonly Node[] = [],
  callLines: readonly number[] = [],
): Promise<RequestedSourceRange[]> {
  const test = isTestPath(filePath) && ['javascript', 'typescript', 'jsx', 'tsx'].includes(language);
  const vue = language === 'vue';

  // A component's name identifies its file, not every line containing "table"
  // or "board". Match the remaining question against its template and styles.
  const basename = filePath.split('/').pop()!.replace(/\.[^.]+$/, '');
  const escapedBasename = basename.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const question = query.replace(
    new RegExp(`${QUOTED_SPAN.source}|\\b${escapedBasename}\\b`, 'gi'),
    match => /^["'`]/.test(match) ? match : '',
  );
  const terms = extractSearchTerms(question).filter(term => !test
    || !['test', 'tests', 'testing', 'spec', 'specs', 'verify', 'verifies', 'verifi'].includes(term));
  const literals = seedLiteralsInQuery(question);
  const identifiers = [...new Set((question.match(/[A-Za-z_$][\w$]*/g) ?? [])
    .filter(word => /[a-z][A-Z]|_/.test(word)).map(word => word.toLowerCase()))];
  if (terms.length === 0 && literals.length === 0 && !test) return [];
  const scoreCeiling = terms.length + (identifiers.length + literals.length) * (terms.length + 1) + 1;
  const score = (text: string): number => {
    const words = new Set(extractSearchTerms(text, { stems: false }));
    return terms.filter(t => words.has(t)).length
      + identifiers.filter(t => words.has(t)).length * (terms.length + 1)
      + literals.filter(l => text.includes(l)).length * (terms.length + 1);
  };
  const ranges: RequestedSourceRange[] = [];
  const declarations: RequestedSourceRange[] = [];
  const sourceLines = source.split('\n');
  if (!test) {
    for (const node of nodes) {
      if (!['function', 'method', 'constant', 'variable', 'property'].includes(node.kind)
          || node.startLine < 1 || node.endLine < node.startLine
          || node.endLine - node.startLine + 1 > sourceLines.length / 2) continue;
      const hit = score(sourceLines.slice(node.startLine - 1, node.endLine).join('\n'));
      if (hit > 0) declarations.push({
        start: node.startLine, end: node.endLine, name: node.name, score: hit, nodeId: node.id,
      });
    }
  }
  if (vue) {
    // Script definitions already have indexed ranges. Only supplement the
    // unmodelled template/style text, with bounded windows around actual hits.
    let inScript = false;
    const lines = source.split('\n');
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]!;
      if (/<script(?:\s|>)/i.test(line)) inScript = true;
      if (!inScript) {
        const hit = score(line);
        if (hit > 0) ranges.push({
          start: Math.max(1, i - 3), end: Math.min(lines.length, i + 5), name: 'query match', score: hit,
        });
      }
      if (/<\/script\s*>/i.test(line)) inScript = false;
    }
    // Small style blocks are a useful unit: the selector alone does not show
    // its widths. Large blocks still use the matching-line windows above.
    for (const match of source.matchAll(/<style(?:\s[^>]*)?>[\s\S]*?<\/style\s*>/gi)) {
      const start = source.slice(0, match.index).split('\n').length;
      const end = start + match[0].split('\n').length - 1;
      const hit = score(match[0]);
      if (hit > 0 && end - start < 200) ranges.push({ start, end, name: 'style', score: hit });
    }
  } else if (test) {
    const fallbackTests: RequestedSourceRange[] = [];
    const tree = await parseSourceTree(source, language);
    if (!tree) return [];
    try {
      const visit = (node: SyntaxNode): void => {
        if (node.type === 'call_expression') {
          const callee = node.childForFieldName('function');
          const parameterized = callee?.type === 'call_expression';
          let target = parameterized ? callee.childForFieldName('function') : callee;
          const members: string[] = [];
          while (target?.type === 'member_expression') {
            members.push(target.childForFieldName('property')?.text ?? '');
            target = target.childForFieldName('object');
          }
          const each = parameterized && members.shift() === 'each';
          const testCall = (!parameterized || each) && target?.type === 'identifier'
            && /^(?:it|test)$/.test(target.text)
            && members.every(member => /^(?:only|skip|concurrent|serial|failing)$/.test(member));
          const args = node.childForFieldName('arguments')?.namedChildren ?? [];
          if (testCall && args.some(a => ['arrow_function', 'function_expression'].includes(a.type))) {
            const callHit = callLines.some(line => line >= node.startPosition.row + 1 && line <= node.endPosition.row + 1);
            const hit = score(node.text) + score(args[0]?.text ?? '') + (callHit ? scoreCeiling : 0);
            if (hit > 0) {
              ranges.push({
                start: node.startPosition.row + 1, end: node.endPosition.row + 1, name: 'test', score: hit + 1,
              });
              // A long test may not fit whole. Its matching setup/assertion
              // statements remain complete units, including multiline arrays.
              const testStatements: SyntaxNode[] = [];
              const statements = (child: SyntaxNode): void => {
                if (['expression_statement', 'lexical_declaration', 'variable_declaration'].includes(child.type)) {
                  testStatements.push(child);
                  return;
                }
                for (const nested of child.namedChildren) statements(nested);
              };
              for (const arg of args) {
                if (['arrow_function', 'function_expression'].includes(arg.type)) statements(arg);
              }
              const containsCall = (statement: SyntaxNode): boolean => callLines.some(line =>
                line >= statement.startPosition.row + 1 && line <= statement.endPosition.row + 1);
              for (let i = 0; i < testStatements.length; i++) {
                const statement = testStatements[i]!;
                // Keep the caller statement and the next complete statement,
                // which can assert its result even when the import is aliased.
                const callScore = containsCall(statement) ? scoreCeiling
                  : i > 0 && containsCall(testStatements[i - 1]!) ? scoreCeiling / 2 : 0;
                const match = score(statement.text) + callScore;
                if (match > 0) ranges.push({
                  start: statement.startPosition.row + 1, end: statement.endPosition.row + 1,
                  name: 'test statement', score: hit + match / scoreCeiling,
                });
              }
            } else fallbackTests.push({
              start: node.startPosition.row + 1, end: node.endPosition.row + 1, name: 'test', score: 1,
            });
            return;
          }
        }
        for (const child of node.namedChildren) visit(child);
      };
      visit(tree.rootNode);
      // A file-only request has no callback terms. Keep its tests ahead of
      // helper boilerplate, unless the question names a helper definition.
      const queryNames = new Set((question.match(/[A-Za-z_$][\w$]*/g) ?? []).map(name => name.toLowerCase()));
      if (ranges.length === 0 && !nodes.some(n => ['function', 'method'].includes(n.kind) && queryNames.has(n.name.toLowerCase()))) {
        ranges.push(...fallbackTests);
      }
    } finally {
      tree.delete();
    }
  }
  // Adjacent template hits describe one region; they must not consume every
  // candidate slot and exclude a later cell or style block for the same query.
  if (vue) {
    // An exact identifier/literal on a template line must not lose to a
    // larger declaration accumulating incidental prose matches across its body.
    const declarationScore = Math.max(0, ...declarations.map(r => r.score));
    for (const r of ranges) if (r.score > terms.length) r.score += declarationScore;
    const merged: RequestedSourceRange[] = [];
    for (const r of ranges.sort((a, b) => a.start - b.start)) {
      const last = merged[merged.length - 1];
      if (last && r.start <= last.end + 1) {
        last.end = Math.max(last.end, r.end);
        last.score = Math.max(last.score, r.score);
      } else merged.push({ ...r });
    }
    return [...declarations, ...merged].sort((a, b) => b.score - a.score || a.start - b.start).slice(0, 12);
  }
  return [...declarations, ...ranges].sort((a, b) => b.score - a.score || a.start - b.start).slice(0, 12);
}
