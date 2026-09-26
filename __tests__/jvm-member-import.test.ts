/**
 * A Kotlin file importing a member of a Java class (`import app.api.Routes.get`)
 * binds its bare `get(...)` to that member, not to a same-named Kotlin method
 * the name matcher would otherwise pick. A chained `.get(...)` in the same file,
 * which Kotlin extraction reduces to a bare `get`, is not the import's.
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';

describe('Kotlin → Java member imports', () => {
  let dir: string;
  let cg: CodeGraph;

  const write = (rel: string, body: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), body);
  };

  const callees = (fnName: string) => {
    const fn = cg.getNodesByName(fnName).find((n) => n.kind === 'function' || n.kind === 'method');
    if (!fn) throw new Error(`no ${fnName}`);
    return cg.getCallees(fn.id).map((c) => `${c.node.qualifiedName}`);
  };

  beforeAll(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-jvm-member-import-'));
    write('src/main/java/app/api/Routes.java', [
      'package app.api;',
      '',
      'public class Routes {',
      '    public static void get(String path) {}',
      '}',
      '',
    ].join('\n'));
    // A same-named Kotlin method the name matcher would choose for a bare `get`.
    write('src/test/kotlin/app/testing/Client.kt', [
      'package app.testing',
      '',
      'class Client {',
      '    fun get(path: String): String = path',
      '}',
      '',
    ].join('\n'));
    write('src/test/kotlin/app/RoutesTest.kt', [
      'package app',
      '',
      'import app.api.Routes.get',
      '',
      'fun registersRoute() {',
      '    get("/hello")',
      '}',
      '',
      'fun chainedGet(server: Server) {',
      '    server.config.routes.get("/other")',
      '}',
      '',
    ].join('\n'));
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.java', '**/*.kt'], exclude: [] } });
    await cg.indexAll();
  });

  afterAll(() => {
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('binds a bare call to the imported Java member', () => {
    expect(callees('registersRoute')).toEqual(['app.api::Routes::get']);
  });

  it('leaves a chained call to the name matcher, not the import', () => {
    expect(callees('chainedGet')).not.toContain('app.api::Routes::get');
  });
});
