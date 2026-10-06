import type { TreeNode as SyntaxNode } from '../parse-tree';
import { getNodeText, getChildByField } from '../tree-sitter-helpers';
import type { LanguageExtractor } from '../tree-sitter-types';

/**
 * A Go function's declared return type, normalized to the bare type a chained
 * `New().Method()` could be called on (the #645/#608 mechanism). Reads the
 * `result` field: a pointer `*Foo` is unwrapped to `Foo`, a multi-return
 * `(*Foo, error)` takes the first result (the idiomatic value-or-error shape),
 * a qualified `pkg.Foo` retains its package, and generics reduce to the base.
 * Unnamed collection results are excluded; built-ins fail the later existence check.
 */
function extractGoReturnType(node: SyntaxNode, source: string): string | undefined {
  let result: SyntaxNode | null | undefined = getChildByField(node, 'result');
  if (result?.type === 'parameter_list') {
    const first = result.namedChildren.find((child: SyntaxNode) => child.type === 'parameter_declaration');
    result = first ? getChildByField(first, 'type') : undefined;
  }
  if (result?.type === 'pointer_type') {
    result = result.namedChildren.find((child: SyntaxNode) =>
      ['type_identifier', 'qualified_type', 'generic_type'].includes(child.type));
  }
  if (!result || !['type_identifier', 'qualified_type', 'generic_type'].includes(result.type)) return undefined;
  const base = getNodeText(result, source).trim().split('[')[0]!.trim();
  return /^[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?$/.test(base) ? base : undefined;
}

export const goExtractor: LanguageExtractor = {
  functionTypes: ['function_declaration'],
  classTypes: [], // Go doesn't have classes
  methodTypes: ['method_declaration'],
  interfaceTypes: [],  // Handled via type_spec → resolveTypeAliasKind
  structTypes: [],     // Handled via type_spec → resolveTypeAliasKind
  enumTypes: [],
  typeAliasTypes: ['type_spec'], // Go type declarations
  importTypes: ['import_declaration'],
  callTypes: ['call_expression'],
  variableTypes: ['var_declaration', 'short_var_declaration', 'const_declaration'],
  methodsAreTopLevel: true,
  nameField: 'name',
  bodyField: 'body',
  paramsField: 'parameters',
  returnField: 'result',
  getReturnType: extractGoReturnType,
  getSignature: (node, source) => {
    const params = getChildByField(node, 'parameters');
    const result = getChildByField(node, 'result');
    if (!params) return undefined;
    let sig = getNodeText(params, source);
    if (result) {
      sig += ' ' + getNodeText(result, source);
    }
    return sig;
  },
  resolveTypeAliasKind: (node, _source) => {
    // Go type_spec: `type Foo struct { ... }` or `type Bar interface { ... }`
    // The inner type is in the 'type' field of the type_spec node
    const typeChild = getChildByField(node, 'type');
    if (!typeChild) return undefined;
    if (typeChild.type === 'struct_type') return 'struct';
    if (typeChild.type === 'interface_type') return 'interface';
    return undefined;
  },
  isExported: (node, source) => {
    // Go: a symbol is exported when its identifier starts with an uppercase letter.
    // Look at the `name` field directly (works for function_declaration,
    // method_declaration, type_spec, and var_spec / const_spec via extractor flow).
    const nameNode = getChildByField(node, 'name');
    if (nameNode) {
      const text = getNodeText(nameNode, source);
      const first = text.charCodeAt(0);
      return first >= 65 && first <= 90; // A-Z
    }
    return false;
  },
  getReceiverType: (node, source) => {
    // Go method_declaration has a "receiver" field: func (sl *scrapeLoop) run(...)
    // The receiver is a parameter_list containing a parameter_declaration
    // with a type that may be a pointer_type (*scrapeLoop) or plain type (scrapeLoop)
    const receiver = getChildByField(node, 'receiver');
    if (!receiver) return undefined;
    // Find the type identifier inside the receiver
    const text = getNodeText(receiver, source);
    // Extract type name from "(sl *Type)", "(sl Type)", "(*Type)", "(Type)" and
    // generic receivers "(s *Stack[T])". Anchor on the opening "(" and skip an
    // optional receiver var name; the old `name)`-anchored pattern never matched
    // the `[T])` suffix, so generic-type methods were orphaned from their type
    // (no struct→method `contains` edge). (#583)
    const match = text.match(/\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*([A-Za-z_]\w*)/);
    return match?.[1];
  },
};
