/**
 * A Vapor route whose handler is a trailing closure — `app.get("hello") { req
 * in … }` — has no handler symbol, so it linked to nothing and the viewer's
 * Steps tab drew it alone (vapor's own repo: 76 of 80 routes). The closure is
 * the handler: its calls are the route's, as an Express inline handler's are.
 * A closure after a non-route call (`if let x = req.parameters.get("x") {`)
 * is not a route body.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vapor-closure-'));
  const files: Record<string, string> = {
    'Package.swift': `// swift-tools-version:5.9
import PackageDescription
let package = Package(name: "App", dependencies: [.package(url: "https://github.com/vapor/vapor.git", from: "4.0.0")])
`,
    'Sources/App/Greeter.swift': `import Vapor
enum Greeter {
    static func greeting(for name: String) -> String { "hi \\(name)" }
    static func audit(_ name: String) {}
}
`,
    'Sources/App/ClientRequest.swift': `struct ClientRequest {
    private struct _ContentContainer { func encode(_ value: String) {} }
}
`,
    'Sources/App/Response.swift': `struct Response {
    var content: ContentContainer { _ContentContainer() }
    private struct _ContentContainer { func encode(_ value: String) {} }
}
protocol ContentContainer {}
`,
    'Sources/App/routes.swift': `import Vapor
func routes(_ app: Application) throws {
    app.get("hello", ":name") { req async throws -> String in
        let name = req.parameters.get("name") ?? "world"
        Greeter.audit(name)
        return Greeter.greeting(for: name)
    }
    app.get("guard") { req in
        let res = Response()
        res.content.encode("hello")
        Greeter.audit("hello")
    }
    app.webSocket("echo") { req, ws in
        ws.onText { ws, text in ws.send(text) }
    }
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

describe('a Vapor closure route', () => {
  it('keeps a proven call and refuses an unrelated private container fallback', () => {
    const route = cg.getNodesByKind('route').find((r) => r.name === 'GET /guard')!;
    const callees = cg.getOutgoingEdgesFrom([route.id], ['calls']).map((e) => cg.getNode(e.target)!.qualifiedName);
    expect(callees).toContain('Greeter::audit');
    expect(callees.filter((name) => name.endsWith('::_ContentContainer::encode'))).toEqual([]);
  });
  it('lends the route its closure’s calls', () => {
    const route = cg.getNodesByKind('route').find((r) => r.name === 'GET /hello/:name')!;
    expect(route).toBeDefined();
    const callees = cg.getOutgoingEdgesFrom([route.id], ['calls']).map((e) => cg.getNode(e.target)!.qualifiedName).sort();
    expect(callees).toEqual(['Greeter::audit', 'Greeter::greeting']);
  });
});
