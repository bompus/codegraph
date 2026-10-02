import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { vaporResolver } from '../src/resolution/frameworks/swift';

const source = `import Vapor
struct Service {
    func show() -> String { "ok" }
}
struct Decoy {
    func show() -> String { "wrong" }
}
func nested(_ body: () -> Void) { body() }
func deferred() {}
func adjacent() {}
func routes(_ app: Application, service: Service) {
    try await app.get("hello") { req in
        service.show()
        do { let service = Decoy(); service.show() }
        service.show()
        let quoted = "adjacent() }"
        let raw = #"deferred() }"#
        let multiline = """
        app.get("phantom") { adjacent() }
        """
        service.show().description()
        // adjacent()
        /* deferred() */
        nested { service.show() }
        func local() { deferred() }
        return quoted + raw
    }
    app.grouped("api").grouped("v1").on(.POST, "item", body: .collect(maxSize: "1mb")) { req in
        service.show()
    }
    app.get("shadow-first") { req in
        do { let service = Decoy(); service.show() }
        service.show()
    }
    adjacent()
    if let value = req.parameters.get("owner") { deferred() }
    req.client.get("https://example.com") { req in deferred() }
}
`;

describe('Vapor closure handler calls', () => {
  it('preserves shadowed receivers and excludes text and nested declarations', () => {
    const { nodes, references } = vaporResolver.extract!('Sources/App/routes.swift', source);
    const calls = references.filter((r) => r.fromNodeId === nodes[0]!.id);
    expect([...new Set(calls.map((r) => r.referenceName))].sort()).toEqual(['Decoy', 'nested', 'service.show']);
    expect(calls.filter((r) => r.referenceName === 'service.show')).toHaveLength(5);
    expect(new Set(calls.map((r) => `${r.line}:${r.column}`)).size).toBe(calls.length);
    expect(nodes.map((n) => n.name)).toEqual(['GET /hello', 'POST /api/v1/item', 'GET /shadow-first']);
    expect(references.filter((r) => r.fromNodeId === nodes[1]!.id).map((r) => r.referenceName)).toEqual(['service.show']);
  });

  it('resolves shadowed receivers separately and preserves repeated call sites', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-closure-calls-'));
    let cg: CodeGraph | undefined;
    try {
      fs.mkdirSync(path.join(root, 'Sources/App'), { recursive: true });
      fs.writeFileSync(path.join(root, 'Sources/App/routes.swift'), source);
      cg = await CodeGraph.init(root, { index: true });
      const graph = cg;
      const routes = graph.getNodesByKind('route');
      for (const route of routes) {
        const calls = graph.getOutgoingEdges(route.id).filter((e) => e.kind === 'calls').map((e) => graph.getNode(e.target)!.qualifiedName).sort();
        expect.soft(calls, route.name).toEqual(route.name === 'GET /hello' ? ['Decoy::show', 'Service::show', 'Service::show', 'Service::show', 'Service::show', 'nested'] : route.name === 'GET /shadow-first' ? ['Decoy::show', 'Service::show'] : ['Service::show']);
      }
      expect(routes.map((r) => r.name).sort()).toEqual(['GET /hello', 'GET /shadow-first', 'POST /api/v1/item']);
    } finally {
      cg?.close();
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
