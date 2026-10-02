import { beforeAll, afterAll, describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const source = `import Vapor
struct Decoy { func show() -> String { "inner" } }
struct Service {
    func show() -> String { "outer" }
    func children() -> [Decoy] { [] }
    func make() -> Decoy { Decoy() }
}
func routes(_ app: Application, service: Service, optionalDecoy: Decoy?) {
    app.get("switch") { req in
        service.show()
        switch optionalDecoy {
        case let .some(service): service.show()
        default: break
        }
        service.show()
    }
    app.get("catch") { req in
        service.show()
        do { try fail() } catch let service { service.show() }
        service.show()
    }
    app.get("collection") { req in
        service.show()
        for service in service.children() { service.show() }
        service.show()
    }
    app.get("initializer") { req in
        service.show()
        nested { [service = service.make()] in service.show() }
        service.show()
    }
}
`;

let root = '';
let cg: CodeGraph;
beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-pattern-bindings-'));
  fs.writeFileSync(path.join(root, 'routes.swift'), source);
  cg = await CodeGraph.init(root, { index: true });
});
afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

const callsFrom = (name: string) => {
  const route = cg.getNodesByKind('route').find((node) => node.name === name)!;
  return cg.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls').map((edge) => ({
    target: cg.getNode(edge.target)!.qualifiedName,
    line: edge.line,
  })).sort((a, b) => a.line! - b.line!);
};

describe('Vapor pattern receiver scopes', () => {
  it('never binds switch or catch patterns to the outer receiver', () => {
    expect(cg.getNodesByKind('route')).toHaveLength(4);
    expect.soft(callsFrom('GET /switch')).toEqual([
      { target: 'Service::show', line: 10 },
      { target: 'Service::show', line: 15 },
    ]);
    expect.soft(callsFrom('GET /catch')).toEqual([
      { target: 'Service::show', line: 18 },
      { target: 'Service::show', line: 20 },
    ]);
  });

  it('uses the outer receiver in loop collections and capture initializers', () => {
    expect.soft(callsFrom('GET /collection')).toEqual([
      { target: 'Service::show', line: 23 },
      { target: 'Service::children', line: 24 },
      { target: 'Service::show', line: 25 },
    ]);
    expect.soft(callsFrom('GET /initializer')).toEqual([
      { target: 'Service::show', line: 28 },
      { target: 'Service::make', line: 29 },
      { target: 'Service::show', line: 30 },
    ]);
  });
});

it('keeps the implicit catch error separate from an outer receiver', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-implicit-error-'));
  let graph: CodeGraph | undefined;
  try {
    fs.writeFileSync(path.join(dir, 'routes.swift'), `import Vapor
struct Service { func show() {} }
extension Error { func show() {} }
func routes(_ app: Application, error: Service) {
    app.get("implicit-error") { req in
        error.show()
        do { try fail() } catch { error.show() }
        error.show()
    }
}
`);
    graph = await CodeGraph.init(dir, { index: true });
    const indexed = graph;
    const route = indexed.getNodesByKind('route')[0]!;
    const calls = indexed.getOutgoingEdges(route.id).filter((edge) => edge.kind === 'calls');
    expect(calls.map((edge) => [edge.line, indexed.getNode(edge.target)!.qualifiedName]).sort()).toEqual([
      [6, 'Service::show'], [8, 'Service::show'],
    ]);
  } finally {
    graph?.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
