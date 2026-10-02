import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { vaporResolver } from '../src/resolution/frameworks/swift';

const source = `import Vapor
struct Service { func show() -> String { "outer" } }
struct Decoy { func show() -> String { "inner" } }
func routes(_ app: Application, service: Service, optionalDecoy: Decoy?) {
    app.get("if-first") { req in
        if let service: Decoy = optionalDecoy { service.show() }
        service.show()
    }
    app.get("if-else") { req in
        if let service: Decoy = optionalDecoy {
            service.show()
        } else {
            service.show()
        }
        service.show()
    }
    app.get("while-first") { req in
        while let service: Decoy = optionalDecoy { service.show() }
        service.show()
    }
}
`;

let root = '';
let cg: CodeGraph;
beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-conditional-'));
  fs.mkdirSync(path.join(root, 'Sources/App'), { recursive: true });
  fs.writeFileSync(path.join(root, 'Sources/App/routes.swift'), source);
  cg = await CodeGraph.init(root, { index: true });
});
afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('Vapor conditional receiver bindings', () => {
  it('retains each conditional, else and outer call site', () => {
    const { nodes, references } = vaporResolver.extract!('Sources/App/routes.swift', source);
    const sites = Object.fromEntries(nodes.map((route) => [route.name,
      references.filter((ref) => ref.fromNodeId === route.id).map((ref) => ref.line),
    ]));
    expect(sites).toEqual({
      'GET /if-first': [6, 7],
      'GET /if-else': [11, 13, 15],
      'GET /while-first': [18, 19],
    });
  });

  it('resolves each conditional receiver on Decoy and each outer receiver on Service', () => {
    const routes = cg.getNodesByKind('route');
    expect(routes.map((route) => route.name).sort()).toEqual(['GET /if-else', 'GET /if-first', 'GET /while-first']);
    for (const route of routes) {
      const targets = cg.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls').map((edge) => cg.getNode(edge.target)!.qualifiedName).sort();
      expect.soft(targets, route.name).toEqual(route.name === 'GET /if-else' ? ['Decoy::show', 'Service::show', 'Service::show'] : ['Decoy::show', 'Service::show']);
    }
  });
});

const unsupported = `import Vapor
struct Service { func show() -> String { "outer" } }
struct Decoy { func show() -> String { "inner" } }
func routes(_ app: Application, service: Service, optionalDecoy: Decoy?) {
    app.get("for") { req in
        service.show()
        for service in [Decoy()] { service.show() }
        service.show()
    }
    app.get("guard") { req in
        service.show()
        guard let service = optionalDecoy else { return }
        service.show()
    }
    app.get("tuple") { req in
        service.show()
        let (service, other) = (Decoy(), 1)
        service.show()
    }
    app.get("capture") { req in
        service.show()
        nested { [service = Decoy()] in service.show() }
        service.show()
    }
}
`;

it('never borrows an outer receiver type for unsupported loop, guard, tuple or capture bindings', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-unsupported-bindings-'));
  let graph: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'routes.swift'), unsupported);
    graph = await CodeGraph.init(dir, { index: true });
    const indexed = graph;
    const routes = indexed.getNodesByKind('route');
    expect(routes).toHaveLength(4);
    for (const route of routes) {
      const calls = indexed.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls');
      const targets = calls.map((edge) => indexed.getNode(edge.target)!.qualifiedName);
      const repeatsOuter = route.name === 'GET /for' || route.name === 'GET /capture';
      expect.soft(targets, route.name).toEqual(repeatsOuter ? ['Service::show', 'Service::show'] : ['Service::show']);
      expect.soft(calls.map((edge) => edge.line).sort(), route.name).toEqual(repeatsOuter ? [route.startLine + 1, route.startLine + 3] : [route.startLine + 1]);
    }
  } finally {
    graph?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

it('retains same-name handler calls with different Swift argument labels', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-overload-calls-'));
  let graph: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'routes.swift'), `import Vapor
struct Service {
    func render(_ value: Int) -> String { "number" }
    func render(label value: String) -> String { "label" }
}
func routes(_ app: Application, service: Service) {
    app.get("overloads") { req in
        service.render(1)
        service.render(label: "x")
    }
}
`);
    graph = await CodeGraph.init(dir, { index: true });
    const indexed = graph;
    const route = indexed.getNodesByKind('route')[0]!;
    const calls = indexed.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls');
    expect(calls.map((edge) => indexed.getNode(edge.target)!.startLine).sort()).toEqual([3, 4]);
    expect(new Set(calls.map((edge) => edge.target)).size).toBe(2);
  } finally {
    graph?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

it('uses the receiver declared after another binding in one declaration', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-multiple-bindings-'));
  let graph: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'routes.swift'), `import Vapor
struct Service { func show() -> String { "outer" } }
struct Decoy { func show() -> String { "inner" } }
func routes(_ app: Application, service: Service) {
    app.get("multiple") { req in
        service.show()
        do { let other = 1, service = Decoy(); service.show() }
        service.show()
    }
}
`);
    graph = await CodeGraph.init(dir, { index: true });
    const indexed = graph;
    const route = indexed.getNodesByKind('route')[0]!;
    const calls = indexed.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls');
    expect(calls.map((edge) => [edge.line, indexed.getNode(edge.target)!.qualifiedName]).sort()).toEqual([
      [6, 'Service::show'], [7, 'Decoy::show'], [8, 'Service::show'],
    ]);
  } finally {
    graph?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
