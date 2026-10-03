/**
 * An override that calls the method it overrides (`super().save()`) is one step
 * longer than the base method's own flow, and explore's Flow prefers the
 * longest chain. When the agent named the override only through a bare token
 * that also named the base (`save` keeps every definition), the chain starts at
 * the base: the override adds a hop, not a mechanism. Paths are measured that
 * way before the longest is chosen. An override the agent named on its own, a
 * different method calling up through `super()`, and a same-named call into a
 * type the caller does NOT extend each keep their first step.
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

    def load(self):
        self.read()

    def read(self):
        pass


class AuditedStore(Store):
    def save(self):
        super().save()


class Audited(Store):
    def audit(self):
        super().save()


class CheckedStore(Store):
    def save(self):
        self.check()
        super().save()

    def check(self):
        pass


class Wrapper:
    def load(self):
        store = Store()
        store.load()
`);
  fs.writeFileSync(path.join(root, 'queue.py'), `class Queue:
    def put(self):
        self.commit()

    def commit(self):
        pass


class Middle(Queue):
    def put(self):
        super().put()


class Child(Middle):
    def put(self):
        super().put()
        self.stage()

    def stage(self):
        self.flush()

    def flush(self):
        pass
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
  it('starts at the base method when one token named the override and the base', () => {
    expect(firstChain('save write')).toEqual(['Store::save', 'Store::write']);
  });

  it('keeps an override the agent named on its own', () => {
    expect(firstChain('AuditedStore.save Store.save write')).toEqual(['AuditedStore::save', 'Store::save', 'Store::write']);
  });

  it('keeps an override the agent named on its own beside the bare name', () => {
    expect(firstChain('CheckedStore.save save write')).toEqual(['CheckedStore::save', 'Store::save', 'Store::write']);
  });

  it('measures each path without its hand-offs before choosing the longest', () => {
    expect(firstChain('put commit flush')).toEqual(['Child::put', 'Child::stage', 'Child::flush']);
  });

  it('keeps a different method that calls up through super()', () => {
    expect(firstChain('Audited.audit Store.save write')).toEqual(['Audited::audit', 'Store::save', 'Store::write']);
  });

  it('keeps the first step of a same-named delegation to an unrelated type', () => {
    expect(firstChain('load read')).toEqual(['Wrapper::load', 'Store::load', 'Store::read']);
  });
});
