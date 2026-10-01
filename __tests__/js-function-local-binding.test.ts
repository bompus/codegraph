/**
 * A JS/TS name the calling function binds itself — a parameter, or a `var` /
 * `let` / `const` above the reference — is that local, never a same-named
 * function declared elsewhere in the file. Every lodash helper lives inside
 * `runInContext`, so `baseHas(object, key)`'s `object` and `mixin`'s
 * `object(this.__wrapped__)` reached a `function object() {}` an IIFE declares
 * there. A function that calls itself through its own `const` still does, and
 * `if (handler) handler()` is not a parameter list.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-js-local-'));
  const files: Record<string, string> = {
    'props.ts': `export interface SharedProps { onDiscussionPrefetch?: (id: string) => void; }
export interface OtherProps { onDiscussionPrefetch?: (id: string) => void; }
export interface OpaqueProps { onDiscussionPrefetch: unknown; }
`,
    'callbacks.ts': `import type { SharedProps as ImportedProps, OpaqueProps } from './props';
type DiscussionsListProps = { onDiscussionPrefetch?: (id: string) => void };
export const DiscussionsList = ({ onDiscussionPrefetch }: DiscussionsListProps) => {
  onDiscussionPrefetch?.('one');
};
export const ImportedList = ({ onDiscussionPrefetch: prefetch }: ImportedProps) => {
  prefetch?.('two');
};
export const UnknownList = ({ onDiscussionPrefetch }: any) => {
  onDiscussionPrefetch?.('three');
};
export const ShadowedList = ({ onDiscussionPrefetch }: DiscussionsListProps) => {
  { const onDiscussionPrefetch = unknownCallback; onDiscussionPrefetch?.('four'); }
};
export const InnerShadow = ({ onDiscussionPrefetch }: DiscussionsListProps) => {
  return onDiscussionPrefetch => onDiscussionPrefetch?.('five');
};
export function DeclarationShadow({ onDiscussionPrefetch }: DiscussionsListProps) {
  function InnerDeclaration() {
    function onDiscussionPrefetch() { }
    onDiscussionPrefetch();
  }
  return InnerDeclaration;
}
export function GeneratorShadow({ onDiscussionPrefetch }: DiscussionsListProps) {
  function InnerGenerator() {
    function* onDiscussionPrefetch() { yield 1; }
    onDiscussionPrefetch();
  }
  return InnerGenerator;
}
export function ExpressionShadow({ onDiscussionPrefetch }: DiscussionsListProps) {
  return function onDiscussionPrefetch() { onDiscussionPrefetch(); };
}
export function AncestorVariableShadow({ onDiscussionPrefetch }: DiscussionsListProps) {
  function Middle() {
    const onDiscussionPrefetch = () => null;
    function InnerVariable() { onDiscussionPrefetch(); }
    return InnerVariable;
  }
  return Middle;
}
export function TypedInnerBinding() {
  const onDiscussionPrefetch = unknownCallback;
  return ({ onDiscussionPrefetch }: DiscussionsListProps) => onDiscussionPrefetch?.('six');
}
export function DefaultShadow({ onDiscussionPrefetch }: DiscussionsListProps) {
  return ({ onDiscussionPrefetch = unknownCallback }: any) => onDiscussionPrefetch();
}
export function DefaultReference({ onDiscussionPrefetch }: DiscussionsListProps) {
  const { other = onDiscussionPrefetch }: { other?: unknown } = {};
  function InnerDefault(value = onDiscussionPrefetch) { onDiscussionPrefetch(); }
  return InnerDefault;
}
export const OpaqueList = ({ onDiscussionPrefetch }: OpaqueProps) => {
  onDiscussionPrefetch?.('opaque');
};
`,
    'lodash.js': `function runInContext(context) {
  var baseCreate = (function() {
    function object() {}
    return function(proto) {
      object.prototype = proto;
      return new object;
    };
  }());

  function baseHas(object, key) {
    return object != null && hasOwnProperty.call(object, key);
  }

  function mixin(object, source) {
    var result = object(this.__wrapped__);
    return result;
  }

  function handler() {}

  function run() {
    if (handler) {
      handler();
    }
    const walk = (node) => (node ? walk(node.next) : null);
    return walk(context);
  }

  return { baseCreate, baseHas, mixin, run };
}

function siblingHandler() {} { const siblingHandler = () => null; }
function runSibling() { return siblingHandler(); }

function runVar() {
  if (true) { var handler = () => 1; }
  return handler();
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

const targetsFrom = (qualifiedName: string) => {
  const ids = cg.getNodesInFile('lodash.js').filter((n) => n.qualifiedName === qualifiedName).map((n) => n.id);
  return cg.getOutgoingEdgesFrom(ids).filter((e) => e.kind !== 'contains').map((e) => cg.getNode(e.target)!.qualifiedName);
};

describe('JS names the calling function binds', () => {
  it('resolves typed destructured callbacks to their declared callable member', () => {
    const nodes = cg.getNodesInFile('callbacks.ts');
    const targets = (name: string) => cg.getOutgoingEdgesFrom(nodes.filter((n) => n.name === name).map((n) => n.id))
      .filter((edge) => edge.kind === 'calls').map((edge) => cg.getNode(edge.target)!);
    expect(targets('DiscussionsList').map((n) => n.qualifiedName)).toContain('DiscussionsListProps::onDiscussionPrefetch');
    expect(targets('ImportedList').map((n) => n.qualifiedName)).toContain('SharedProps::onDiscussionPrefetch');
    expect(targets('ImportedList').map((n) => n.qualifiedName)).not.toContain('OtherProps::onDiscussionPrefetch');
    for (const owner of ['TypedInnerBinding', 'DefaultReference']) {
      const innerIds = nodes.filter((n) => n.qualifiedName.startsWith(owner)).map((n) => n.id);
      const innerTargets = cg.getOutgoingEdgesFrom(innerIds).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
      expect(innerTargets).toContain('DiscussionsListProps::onDiscussionPrefetch');
    }
    for (const name of ['UnknownList', 'ShadowedList', 'InnerShadow', 'DefaultShadow', 'OpaqueList']) {
      expect(targets(name).filter((n) => n.qualifiedName.endsWith('Props::onDiscussionPrefetch'))).toEqual([]);
    }
  });

  it('keeps nearer named functions and ancestor local bindings ahead of typed outer parameters', () => {
    const nodes = cg.getNodesInFile('callbacks.ts');
    for (const owner of ['DeclarationShadow', 'GeneratorShadow', 'ExpressionShadow', 'AncestorVariableShadow']) {
      const ids = nodes.filter((n) => n.qualifiedName === owner || n.qualifiedName.startsWith(`${owner}::`)).map((n) => n.id);
      const calls = cg.getOutgoingEdgesFrom(ids).filter((edge) => edge.kind === 'calls');
      const targets = calls.map((edge) => cg.getNode(edge.target)!);
      expect(targets.map((n) => n.qualifiedName)).not.toContain('DiscussionsListProps::onDiscussionPrefetch');
      if (owner !== 'ExpressionShadow') expect(targets.some((n) => n.name === 'onDiscussionPrefetch')).toBe(true);
    }
  });

  it('distinguishes same-line function and closed block declarations', () => {
    expect(targetsFrom('runSibling')).toContain('siblingHandler');
  });

  it('keeps var arrow functions visible outside their declaration block', () => {
    expect(targetsFrom('runVar')).toContain('runVar::handler');
  });

  it('are its parameters, not a same-named function elsewhere in the file', () => {
    expect(targetsFrom('runInContext::baseHas')).not.toContain('runInContext::object');
    expect(targetsFrom('runInContext::mixin')).not.toContain('runInContext::object');
  });

  it('leave unbound names and a const’s own recursion alone', () => {
    expect(targetsFrom('runInContext::run')).toContain('runInContext::handler');
    expect(targetsFrom('runInContext::run::walk')).toContain('runInContext::run::walk');
  });
});
