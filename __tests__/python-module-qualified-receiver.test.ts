/**
 * A Python local typed by a constructor named through a module
 * (`bp = pkg.Blueprint()`, `c.Blueprint()`, `core.Blueprint()`) is typed like
 * one built from a class imported by name, and `local.field.method()` resolves
 * on the class the field holds, as `self.field.method()` does.
 */
import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let dir: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
  dir = undefined;
});

async function project(files: Record<string, string>) {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-py-module-receiver-'));
  for (const [name, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), source);
  }
  graph = await CodeGraph.init(dir, { index: true });
  graph.resolveReferences();
}

/** `Qualified::name` of every call target of the function `caller`. */
function calls(caller: string): string[] {
  const from = graph!.getNodesByKind('function').find(n => n.name === caller);
  expect(from, caller).toBeDefined();
  return graph!
    .getOutgoingEdges(from!.id)
    .filter(e => e.kind === 'calls')
    .map(e => graph!.getNode(e.target)!.qualifiedName)
    .sort();
}

const core = `class Config(dict):
    def from_mapping(self, m):
        return True


class Base:
    def add_x(self):
        return 1


class Blueprint(Base):
    def own(self):
        return 2


class Flask:
    def __init__(self):
        self.config = Config()

    def test_client(self):
        return 3
`;
const files = (test: string, extra: Record<string, string> = {}) => ({
  'pkg/__init__.py': 'from .core import Blueprint as Blueprint\nfrom .core import Flask as Flask\n',
  'pkg/core.py': core,
  'tests/test_a.py': test,
  ...extra,
});

describe('a local typed by a module-qualified constructor', () => {
  it.each([
    ['a re-exported class', 'import pkg\n\n\ndef t():\n    bp = pkg.Blueprint()\n    bp.own()\n'],
    ['a dotted module path', 'import pkg.core\n\n\ndef t():\n    bp = pkg.core.Blueprint()\n    bp.own()\n'],
    ['an aliased module', 'import pkg.core as c\n\n\ndef t():\n    bp = c.Blueprint()\n    bp.own()\n'],
    ['a module imported by name', 'from pkg import core\n\n\ndef t():\n    bp = core.Blueprint()\n    bp.own()\n'],
  ])('reaches the class method through %s', async (_label, test) => {
    await project(files(test));
    expect(calls('t')).toEqual(['Blueprint::own']);
  });

  it('reaches an inherited method', async () => {
    await project(files('import pkg\n\n\ndef t():\n    bp = pkg.Blueprint()\n    bp.add_x()\n'));
    expect(calls('t')).toEqual(['Base::add_x']);
  });

  it('does not type a local from a call on another local object', async () => {
    await project(
      files(
        'import pkg\n\n\ndef make(factory):\n    bp = factory.Blueprint()\n    bp.own()\n',
        { 'other.py': 'class Blueprint:\n    def own(self):\n        return 9\n' },
      ),
    );
    expect(calls('make')).toEqual([]);
  });
});

describe('a call through a field of a typed local', () => {
  it.each([
    ['a class imported by name', 'from pkg import Flask\n\n\ndef t():\n    app = Flask()\n    app.config.from_mapping({})\n'],
    ['a module-qualified class', 'import pkg\n\n\ndef t():\n    app = pkg.Flask()\n    app.config.from_mapping({})\n'],
  ])('resolves on the class the field holds for %s', async (_label, test) => {
    await project(files(test));
    expect(calls('t')).toEqual(['Config::from_mapping']);
  });

  it('does not borrow a same-named method of an unrelated class', async () => {
    await project(
      files('from pkg import Flask\n\n\ndef t():\n    app = Flask()\n    app.config.save({})\n', {
        'other.py': 'class Unrelated:\n    def save(self, m):\n        return 1\n',
      }),
    );
    expect(calls('t')).toEqual([]);
  });

  it('does not resolve when the local has no known type', async () => {
    await project(files('def t(app):\n    app.config.from_mapping({})\n'));
    expect(calls('t')).toEqual([]);
  });

  it('does not resolve a field the class never assigns', async () => {
    await project(files('from pkg import Flask\n\n\ndef t():\n    app = Flask()\n    app.missing.from_mapping({})\n'));
    expect(calls('t')).toEqual([]);
  });
});
