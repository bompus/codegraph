/**
 * Kotlin shapes tree-sitter-kotlin misparses, and what extraction does about them.
 *
 * - An assignment to a member of a call result (`res().status = code`) is not
 *   an assignable expression in the grammar. Inside a lambda the error
 *   recovery took the lambda's `}` for the class body's: javalin's
 *   `interface Context` ended at its first `also { res().status = … }` and
 *   every later member came out as a top-level function, which the Kotlin
 *   visibility rule then hid from other packages (`ctx.statusCode()`).
 * - A `fun interface` has no production at all. The recovered interface
 *   spanned only its header line, so its members sat outside it, and one
 *   with type parameters (`fun interface TaskInitializer<CTX : Context>`)
 *   was dropped with its members.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { rewriteKotlinCallMemberAssignments } from '../src/extraction/languages/kotlin';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-kotlin-recovery-'));
  const files: Record<string, string> = {
    'src/main/kotlin/web/http/Context.kt': `package web.http

class Response {
    var status: Int = 200
    var contentType: String = ""
}

class HttpStatus(val code: Int)

interface Context {
    fun res(): Response

    fun contentType(type: String): Context = also { res().contentType = type }

    fun status(status: HttpStatus): Context = also { res().status = status.code }

    fun statusCode(): Int = res().status

    fun count(): Context = also { res().status += 1 }
}

class Plain {
    fun tail() {}
}
`,
    'src/main/kotlin/web/handlers/Handler.kt': `package web.handlers

import web.http.Context

class Handler {
    fun handle(ctx: Context) {
        ctx.statusCode()
    }
}
`,
    'src/main/kotlin/web/tasks/Task.kt': `package web.tasks

fun interface TaskHandler<R> {
    fun handle(): R
}

fun interface Wrapper {
    fun wrap(input: String): String
}

class After
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

const nodeNamed = (file: string, name: string, kind?: string) =>
  cg.getNodesInFile(file).find((n) => n.name === name && (kind === undefined || n.kind === kind))!;

describe('an assignment to a member of a call result', () => {
  const file = 'src/main/kotlin/web/http/Context.kt';

  it('leaves the class around it whole', () => {
    const context = nodeNamed(file, 'Context');
    expect([context.startLine, context.endLine]).toEqual([10, 20]);
    for (const member of ['contentType', 'status', 'statusCode', 'count']) {
      const node = nodeNamed(file, member, 'method');
      expect([member, node.kind, node.qualifiedName]).toEqual([member, 'method', `web.http::Context::${member}`]);
    }
    // The class after it stays its own.
    expect(nodeNamed(file, 'tail').qualifiedName).toBe('web.http::Plain::tail');
  });

  it('keeps the calls inside it and the member a caller in another package reaches', () => {
    const status = nodeNamed(file, 'status', 'method');
    const calls = cg.getOutgoingEdgesFrom([status.id], ['calls']).map((e) => cg.getNode(e.target)?.qualifiedName);
    expect(calls).toContain('web.http::Context::res');
    const handle = nodeNamed('src/main/kotlin/web/handlers/Handler.kt', 'handle');
    const targets = cg.getOutgoingEdgesFrom([handle.id], ['calls']).map((e) => cg.getNode(e.target)?.qualifiedName);
    expect(targets).toEqual(['web.http::Context::statusCode']);
  });

  it('is rewritten at the same offsets, and only where the target is a call result member', () => {
    const src = [
      'also { res().status = code }',
      'a.b(x).c?.d += 1',
      'rows()[0] = v',
      'x.y = 1',
      '(res()).status = 1',
      'fun f() = g().h',
      'res().status == code',
      'res().status=code',
    ].join('\n');
    const out = rewriteKotlinCallMemberAssignments(src);
    expect(out.length).toBe(src.length);
    expect(out.split('\n')).toEqual([
      'also { res().status== code }',
      'a.b(x).c?.d +  1',
      'rows()[0]== v',
      'x.y = 1',
      '(res()).status = 1',
      'fun f() = g().h',
      'res().status == code',
      'res().status=code',
    ]);
  });
});

describe('a fun interface', () => {
  const file = 'src/main/kotlin/web/tasks/Task.kt';

  it('spans its body, type parameters or not, and contains its member', () => {
    for (const [iface, member, lines] of [
      ['TaskHandler', 'handle', [3, 5]],
      ['Wrapper', 'wrap', [7, 9]],
    ] as const) {
      const node = nodeNamed(file, iface);
      expect([iface, node.kind, node.startLine, node.endLine]).toEqual([iface, 'interface', ...lines]);
      const m = nodeNamed(file, member);
      expect(m.qualifiedName).toBe(`web.tasks::${iface}::${member}`);
      expect(m.startLine > node.startLine && m.endLine <= node.endLine).toBe(true);
    }
    expect(nodeNamed(file, 'After').kind).toBe('class');
  });
});
