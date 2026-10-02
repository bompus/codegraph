/**
 * An override that calls the method it overrides (`super().save()`) is one step
 * longer than the base method's own flow, and explore's Flow prefers the
 * longest chain. When the agent named both, the chain starts at the base: the
 * override adds a hop, not a mechanism. A different method calling up through
 * `super()`, or a same-named call into a type the caller does NOT extend, is
 * its own step and stays.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { resolveNamedSymbolFlow } from '../src/graph/named-symbol-flow';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-flow-super-'));
  fs.writeFileSync(path.join(root, 'store.py'), `class Store:
    def save(self):
        self.write()

    def write(self):
        pass


class AuditedStore(Store):
    def save(self):
        super().save()


class Audited(Store):
    def audit(self):
        super().save()


class Wrapper:
    def save(self):
        store = Store()
        store.save()
`);
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

const firstChain = (query: string) =>
  resolveNamedSymbolFlow(cg, query).chains[0]?.steps.map((s) => s.node.qualifiedName);

describe('explore flow and an override that calls its super method', () => {
  it('starts at the base method the override hands off to', () => {
    expect(firstChain('AuditedStore.save Store.save write')).toEqual(['Store::save', 'Store::write']);
  });

  it('keeps a different method that calls up through super()', () => {
    expect(firstChain('Audited.audit Store.save write')).toEqual(['Audited::audit', 'Store::save', 'Store::write']);
  });

  it('keeps the first step of a same-named delegation to an unrelated type', () => {
    expect(firstChain('Wrapper.save Store.save write')).toEqual(['Wrapper::save', 'Store::save', 'Store::write']);
  });
});
