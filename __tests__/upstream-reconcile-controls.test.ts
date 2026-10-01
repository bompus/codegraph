import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let graph: CodeGraph;

const script = `import buildlogic.testDb

testDb {
  dialects("sqlite")
  dependencies {
    implementation("example:driver:1")
  }
}

dependencies {
  implementation("example:unrelated:1")
}
`;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-upstream-reconcile-'));
  const files: Record<string, string> = {
    'contexts.ts': `export class Base {
  constructor() {}
  run(): string { return "base"; }
}
export class Derived extends Base {
  constructor() { super(); }
  override run(): string { return "derived"; }
}
`,
    'tree.ts': `export class TreeNode {
  inner: unknown;
  other!: { visit(): void };
  visit(): void {
    (this.inner as TreeNode).visit();
    this.other.visit();
  }
}
`,
    'buildSrc/src/main/kotlin/org/gradle/api/Project.kt': `package org.gradle.api
class Project
`,
    'buildSrc/src/main/kotlin/buildlogic/TestDb.kt': `package buildlogic
import org.gradle.api.Project

class DependencyBlock {
  fun implementation(notation: String) {}
}
class TestDb {
  fun dialects(vararg names: String) {}
  fun dependencies(block: DependencyBlock.() -> Unit) {
    DependencyBlock().block()
  }
}
fun Project.testDb(block: TestDb.() -> Unit) {
  TestDb().block()
}
`,
    'build.gradle.kts': script,
    'cpp/overloads.cpp': `struct X {
  int f(int x) {
    return f(x, 0);
  }
  int f(int x, int y) {
    return x + y;
  }
};
int choose(bool enabled = (1 < 2), int value = 0) { return value; }
int use() { return choose(true, 2); }
`,
    'js/context.js': `function outer() {
  function handler() { return 1; }
  function run() {
    // const handler = null;
    if (false) { const handler = () => 0; handler(); }
    for (let handler = null; false;) {}
    const obj = { method() { var handler = null; } };
    return handler();
  }
  return run;
}
`,
    'python/objects.py': `class Component:
    def describe(self, parent):
        return parent.device
`,
    'python/filters.py': `def device(queryset):
    return queryset
`,
    'python/models.py': `class Forum:
    def save(self):
        pass
`,
    'python/conftest.py': `import pytest
from .models import Forum
@pytest.fixture
def forum():
    return Forum()
`,
    'python/helpers.py': `def save_forum(forum):
    forum.save()
`,
    'python/test_forum.py': `def test_forum(forum):
    forum.save()
`,
    'java/Instants.java': `class Instants {
  static Object toInstant(Object instant) {
    return toInstant(1 < 2, null);
  }
  static Object toInstant(Object instant, Object epoch) {
    return instant;
  }
}
`,
    'rust/Cargo.toml': '[package]\nname = "recursive-control"\nversion = "0.1.0"\nedition = "2021"\n',
    'rust/src/lib.rs': `pub enum Error {
  Partial(Vec<Error>),
  WithPath { err: Box<Error> },
  Foreign(Box<Error>, external::ForeignError),
  Io,
}
impl Error {
  pub fn is_io(&self) -> bool {
    match self {
      Error::Partial(errs) => errs.len() == 1 && errs[0].is_io(),
      Error::WithPath { err } => err.is_io(),
      Error::Foreign(_, foreign) => foreign.is_io(),
      Error::Io => true,
    }
  }
}
pub trait Matcher {
  fn find_at(&self, haystack: &[u8], at: usize) -> Option<usize>;
}
impl<'a, M: Matcher> Matcher for &'a M {
  fn find_at(&self, haystack: &[u8], at: usize) -> Option<usize> {
    (*self).find_at(haystack, at)
  }
}
`,
    'kong/pdk/response.lua': `local function new()
  local _RESPONSE = {}
  function _RESPONSE.clear_header(name)
    return name
  end
  return _RESPONSE
end
return { new = new }
`,
    'kong/plugins/header_transformer.lua': `local kong = kong
local clear_header = kong.response.clear_header
local clear_request_header = kong.service.request.clear_header
local function transform_response()
  clear_header("X-Response")
end
local function transform_request()
  clear_request_header("X-Request")
end
return { response = transform_response, request = transform_request }
`,
  };
  for (const [relative, content] of Object.entries(files)) {
    const destination = path.join(root, relative);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, content);
  }
  graph = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  graph?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true, maxRetries: 5 });
});

function method(file: string, qualifiedName: string) {
  const node = graph.getNodesInFile(file).find((n) => n.qualifiedName === qualifiedName);
  expect(node, qualifiedName).toBeDefined();
  return node!;
}

describe('source-backed reconciliation controls', () => {
  it('counts C++ parameters with comparison defaults without treating comparisons as generic brackets', () => {
    const caller = method('cpp/overloads.cpp', 'use');
    const target = method('cpp/overloads.cpp', 'choose');
    expect(graph.getOutgoingEdges(caller.id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target)).toContain(target.id);
  });

  it('bridges virtual overrides without dispatching a base constructor to its descendant', () => {
    const baseConstructor = method('contexts.ts', 'Base::constructor');
    const derivedConstructor = method('contexts.ts', 'Derived::constructor');
    const baseRun = method('contexts.ts', 'Base::run');
    const derivedRun = method('contexts.ts', 'Derived::run');
    const bridges = (id: string) => graph.getOutgoingEdges(id).filter((edge) =>
      edge.kind === 'calls' && edge.metadata?.synthesizedBy === 'interface-impl',
    );
    expect(bridges(baseConstructor.id).map((edge) => edge.target)).not.toContain(derivedConstructor.id);
    expect(bridges(baseRun.id).map((edge) => edge.target)).toContain(derivedRun.id);
  });

  it('resolves nested Gradle DSL receivers without borrowing them for top-level dependencies', () => {
    const dialects = method('buildSrc/src/main/kotlin/buildlogic/TestDb.kt', 'buildlogic::TestDb::dialects');
    const dependencies = method('buildSrc/src/main/kotlin/buildlogic/TestDb.kt', 'buildlogic::TestDb::dependencies');
    const implementation = method('buildSrc/src/main/kotlin/buildlogic/TestDb.kt', 'buildlogic::DependencyBlock::implementation');
    const calls = graph.getOutgoingEdgesFrom(graph.getNodesInFile('build.gradle.kts').map((n) => n.id))
      .filter((edge) => edge.kind === 'calls');
    expect(calls.filter((edge) => edge.line === 4).map((edge) => edge.target)).toContain(dialects.id);
    expect(calls.filter((edge) => edge.line === 5).map((edge) => edge.target)).toContain(dependencies.id);
    expect(calls.filter((edge) => edge.line === 6).map((edge) => edge.target)).toContain(implementation.id);
    expect(calls.filter((edge) => edge.line === 10).map((edge) => edge.target)).not.toContain(dependencies.id);
    expect(calls.filter((edge) => edge.line === 11).map((edge) => edge.target)).not.toContain(implementation.id);
  });

  it('retains recursive Rust enum calls while refusing generic matcher delegation as recursion', () => {
    const recursive = method('rust/src/lib.rs', 'Error::is_io');
    const selfCalls = graph.getOutgoingEdges(recursive.id).filter((edge) => edge.kind === 'calls' && edge.target === recursive.id);
    expect(selfCalls.map((edge) => edge.line)).toContain(10);
    expect(selfCalls.map((edge) => edge.line)).toContain(11);
    expect(selfCalls.map((edge) => edge.line)).not.toContain(12);
    const delegates = graph.getNodesInFile('rust/src/lib.rs').filter((n) =>
      n.name === 'find_at' && n.qualifiedName !== 'Matcher::find_at',
    );
    expect(delegates).toHaveLength(1);
    expect(graph.getOutgoingEdges(delegates[0].id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target))
      .not.toContain(delegates[0].id);
  });

  it('retains a cast-proven recursive receiver without turning an unrelated field call into recursion', () => {
    const visit = method('tree.ts', 'TreeNode::visit');
    const selfCalls = graph.getOutgoingEdges(visit.id).filter((edge) => edge.kind === 'calls' && edge.target === visit.id);
    expect(selfCalls.map((edge) => edge.line)).toContain(5);
    expect(selfCalls.map((edge) => edge.line)).not.toContain(6);
  });

  it('follows a captured Lua host alias while preserving the response and request owner boundary', () => {
    const response = method('kong/pdk/response.lua', '_RESPONSE::clear_header');
    const responseCaller = method('kong/plugins/header_transformer.lua', 'transform_response');
    const requestCaller = method('kong/plugins/header_transformer.lua', 'transform_request');
    expect(graph.getOutgoingEdges(responseCaller.id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target))
      .toContain(response.id);
    expect(graph.getOutgoingEdges(requestCaller.id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target))
      .not.toContain(response.id);
  });

  it('does not infer a top-level Python function from an untyped attribute value', () => {
    const caller = graph.getNodesInFile('python/objects.py').find((n) => n.name === 'describe')!;
    const target = graph.getNodesInFile('python/filters.py').find((n) => n.name === 'device')!;
    expect(caller).toBeDefined();
    expect(target).toBeDefined();
    expect(graph.getOutgoingEdges(caller.id).map((edge) => edge.target)).not.toContain(target.id);
  });

  it('counts a relational expression as one argument when calling another Java overload', () => {
    const methods = graph.getNodesInFile('java/Instants.java').filter((n) => n.name === 'toInstant');
    const caller = methods.find((n) => n.startLine === 2)!;
    const target = methods.find((n) => n.startLine === 5)!;
    expect(caller).toBeDefined();
    expect(target).toBeDefined();
    const calls = graph.getOutgoingEdges(caller.id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target);
    expect(calls).toContain(target.id);
    expect(calls).not.toContain(caller.id);
  });

  it('types fixture parameters in tests without injecting fixtures into ordinary helpers', () => {
    const save = graph.getNodesInFile('python/models.py').find((n) => n.name === 'save')!;
    const test = graph.getNodesInFile('python/test_forum.py').find((n) => n.name === 'test_forum')!;
    const helper = graph.getNodesInFile('python/helpers.py').find((n) => n.name === 'save_forum')!;
    expect(save).toBeDefined();
    expect(test).toBeDefined();
    expect(helper).toBeDefined();
    const targets = (id: string) => graph.getOutgoingEdges(id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target);
    expect(targets(test.id)).toContain(save.id);
    expect(targets(helper.id)).not.toContain(save.id);
  });


  it('selects a compatible C++ sibling overload before rejecting the current method', () => {
    const methods = graph.getNodesInFile('cpp/overloads.cpp').filter((n) => n.name === 'f');
    const caller = methods.find((n) => n.startLine === 2)!;
    const target = methods.find((n) => n.startLine === 5)!;
    expect(caller).toBeDefined();
    expect(target).toBeDefined();
    const calls = graph.getOutgoingEdges(caller.id).filter((edge) => edge.kind === 'calls').map((edge) => edge.target);
    expect(calls).toContain(target.id);
    expect(calls).not.toContain(caller.id);
  });

  it('keeps an outer JavaScript function visible after closed block and loop bindings', () => {
    const caller = graph.getNodesInFile('js/context.js').find((n) => n.name === 'run')!;
    const target = graph.getNodesInFile('js/context.js').find((n) => n.name === 'handler' && n.startLine === 2)!;
    expect(caller).toBeDefined();
    expect(target).toBeDefined();
    const calls = graph.getOutgoingEdges(caller.id).filter((edge) => edge.kind === 'calls' && edge.target === target.id);
    expect(calls.map((edge) => edge.line)).toContain(8);
    expect(calls.map((edge) => edge.line)).not.toContain(5);
  });

});
