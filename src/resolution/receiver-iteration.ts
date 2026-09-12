/** Receiver evidence introduced by a scoped lambda or iteration construct. */
import { parseSourceTreeSync, TreeNode } from '../extraction/parse-tree';
import { Node } from '../types';
import { ResolutionContext, UnresolvedRef } from './types';

export function inferIterationReceiver(
  receiver: string, ref: UnresolvedRef, context: ResolutionContext,
  infer: (name: string, site: UnresolvedRef) => string | null,
  resolveType: (type: string, site: UnresolvedRef) => Node | undefined,
  resolveCall: (name: string, site: UnresolvedRef) => Node | undefined,
): { type: string; site: UnresolvedRef } | null {
  if (ref.language !== 'kotlin' && ref.language !== 'go') return null;
  const source = context.readFile(ref.filePath);
  if (!source) return null;
  const tree = parseSourceTreeSync(source, ref.language);
  if (!tree) return null;
  try {
    let node = tree.rootNode.descendantForPosition({ row: ref.line - 1, column: ref.column });
    for (; node; node = node.parent) {
      if (ref.language === 'kotlin' && node.type === 'lambda_literal') {
        const parameters = node.namedChildren.find(n => n.type === 'lambda_parameters');
        const names = parameters?.descendantsOfType('simple_identifier').map(n => n.text) ?? ['it'];
        // The nearest lambda owns implicit it, even when its type is unknown.
        if (!names.includes(receiver)) continue;
        let call: TreeNode | null = node.parent;
        while (call && call.type !== 'call_expression') call = call.parent;
        const navigation = call?.namedChildren[0];
        if (navigation?.type !== 'navigation_expression') return null;
        const root = navigation.namedChildren[0];
        const method = navigation.namedChildren[1]?.text.replace(/^[?.]+/, '');
        if (root?.type !== 'simple_identifier' || !['let', 'also'].includes(method ?? '')) return null;
        const site = { ...ref, line: navigation.startPosition.row + 1, column: navigation.startPosition.column };
        const type = infer(root.text, site);
        return type ? { type, site } : null;
      }
      if (ref.language === 'go' && node.type === 'for_statement') {
        const range = node.namedChildren.find(n => n.type === 'range_clause');
        const names = range?.childForFieldName('left')?.namedChildren;
        const collection = range?.childForFieldName('right');
        if (!names || !collection) continue;
        const index = names.findIndex(n => n.text === receiver);
        if (index < 0) continue;
        if (index !== 1) return null;
        if (collection.type === 'selector_expression') {
          const base = collection.childForFieldName('operand');
          const field = collection.childForFieldName('field');
          if (base?.type !== 'identifier' || !field) return null;
          const ownerType = infer(base.text, ref);
          if (!ownerType) return null;
          // A field's slice element type is resolved in its owner's file.
          const owner = resolveType(ownerType, ref);
          if (!owner) return null;
          const lines = context.getFileLines?.(owner.filePath)?.slice(owner.startLine - 1, owner.endLine).join('\n');
          const type = lines?.match(new RegExp(`\\b${field.text}\\s+\\[\\d*\\]\\s*\\*?([\\w.]+)`))?.[1];
          return type ? { type, site: { ...ref, filePath: owner.filePath, line: owner.startLine } } : null;
        }
        if (collection.type !== 'identifier') return null;
        const bindings = context.getBindings?.(ref.filePath) ?? [];
        const binding = bindings.filter(b => b.name === collection.text && b.scopeStart <= ref.line && b.scopeEnd >= ref.line)
          .sort((a, b) => (a.scopeEnd - a.scopeStart) - (b.scopeEnd - b.scopeStart))[0];
        if (!binding) return null;
        const declaration = source.split(/\r?\n/)[binding.line - 1] ?? '';
        const escaped = collection.text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const type = declaration.match(new RegExp(`\\b${escaped}\\s*(?::=|=)?\\s*(?:\\[\\d*\\]|map\\[[^\\]]+\\])\\s*\\*?([\\w.]+)`));
        // The first slice/array range binding is the numeric index. Map keys
        // need their own declared type; don't substitute the value's type.
        if (type?.[1]) return { type: type[1], site: ref };
        const factory = declaration.match(new RegExp(`\\b${escaped}(?:\\s*,\\s*\\w+)*\\s*:=\\s*([\\w.]+)\\s*\\(`))?.[1];
        if (!factory) return null;
        const callee = resolveCall(factory, { ...ref, line: binding.line });
        const element = callee?.signature?.match(/\)\s*\(?\s*\[\]\s*\*?([\w.]+)/)?.[1];
        return element && callee ? { type: element, site: { ...ref, filePath: callee.filePath, line: callee.startLine } } : null;
      }
    }
    return null;
  } finally { tree.delete(); }
}
