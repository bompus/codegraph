import { describe, expect, it } from 'vitest';
import { expressResolver } from '../src/resolution/frameworks/express';
import type { ResolutionContext, UnresolvedRef } from '../src/resolution/types';
import type { Node } from '../src/types';

function node(id: string, name: string, kind: Node['kind'], overrides: Partial<Node> = {}): Node {
  return {
    id,
    kind,
    name,
    qualifiedName: name,
    filePath: 'server/app.ts',
    language: 'typescript',
    startLine: 1,
    endLine: 1,
    startColumn: 0,
    endColumn: 0,
    updatedAt: 0,
    ...overrides,
  };
}

function context(nodes: Node[]): ResolutionContext {
  return {
    getNodesInFile: (file) => nodes.filter((n) => n.filePath === file),
    getNodesByName: (name) => nodes.filter((n) => n.name === name),
    getNodesByQualifiedName: (name) => nodes.filter((n) => n.qualifiedName === name),
    getNodesByKind: (kind) => nodes.filter((n) => n.kind === kind),
    getNodeById: (id) => nodes.find((n) => n.id === id) ?? null,
    fileExists: (file) => nodes.some((n) => n.filePath === file),
    readFile: () => null,
    getProjectRoot: () => '/project',
    getAllFiles: () => [...new Set(nodes.map((n) => n.filePath))],
    getImportMappings: () => [],
  };
}

function reference(name: string, overrides: Partial<UnresolvedRef> = {}): UnresolvedRef {
  return {
    fromNodeId: 'route',
    referenceName: name,
    referenceKind: 'calls',
    filePath: 'server/app.ts',
    language: 'typescript',
    line: 2,
    column: 0,
    ...overrides,
  };
}

describe('Express middleware declarations', () => {
  it.each([
    ['cors', 'cors'],
    ['authMiddleware', 'auth'],
  ])('does not treat the import %s as a middleware declaration', (calledName, importedName) => {
    const imports = [node('import', importedName, 'import')];
    expect(expressResolver.resolve(reference(calledName), context(imports))).toBeNull();
  });

  it('reaches the same-file middleware function despite same-named import rows', () => {
    const middleware = node('own-middleware', 'validate', 'function');
    const nodes = [
      node('foreign-import', 'validate', 'import', { filePath: 'another/app.ts' }),
      node('own-import', 'validate', 'import'),
      middleware,
    ];
    expect(expressResolver.resolve(reference('validate'), context(nodes))).toMatchObject({
      targetNodeId: middleware.id,
      resolvedBy: 'framework',
    });
  });

  it('refuses a receiverless middleware-like call to a C function-pointer field', () => {
    const field = node('callback-field', 'validate', 'field', {
      filePath: 'server/callbacks.c',
      language: 'c',
      qualifiedName: 'Callbacks::validate',
    });
    const call = reference('validate', { filePath: field.filePath, language: 'c' });
    expect(expressResolver.resolve(call, context([field]))).toBeNull();
  });
});
