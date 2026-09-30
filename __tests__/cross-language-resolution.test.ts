import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

describe('cross-language name resolution (#1986)', () => {
  let dir: string;
  let cg: CodeGraph | undefined;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-language-')); });
  afterEach(() => {
    cg?.close();
    cg = undefined;
    fs.rmSync(dir, { recursive: true, force: true });
  });

  async function index(files: Record<string, string>) {
    for (const [file, source] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
      fs.writeFileSync(path.join(dir, file), source);
    }
    cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    return cg;
  }

  function targets(name: string) {
    const source = cg!.getNodesByName(name).find((n) => n.kind !== 'file');
    expect(source, name).toBeDefined();
    return cg!.getOutgoingEdges(source!.id)
      .filter((e) => ['calls', 'instantiates', 'extends', 'implements'].includes(e.kind))
      .map((e) => cg!.getNode(e.target)!.name);
  }

  it('rejects foreign calls, explicit constructors, extends and implements', async () => {
    await index({
      'lib.rs': 'pub fn parse() -> Result<u32, String> { Ok(1) }',
      'Ok.scala': 'object Ok { def apply(): Int = 1 }',
      'total.py': 'def total():\n    return round(1.2)\n',
      'round.ts': 'export function round() { return 1; }',
      'consumer.swift': 'func consumer() { mystery() }',
      'foreign.py': 'def mystery():\n    return 1\nclass ForeignThing:\n    pass\nclass ForeignBase:\n    pass\n',
      'contract.java': 'public interface ForeignContract {}',
      'App.tsx': 'export function App() { return <ForeignThing/>; }',
      'Page.vue': '<template><ForeignThing @click="mystery"/></template><script>export default {};</script>',
      'construct.ts': 'export function construct() { return new ForeignThing(); }\n' +
        'export class Child extends ForeignBase {}\nexport class Impl implements ForeignContract {}',
      'plain.go': 'package main\nfunc UnexportedABI() {}\n',
      'native.c': 'void nativeConsumer(void) { UnexportedABI(); }',
    });
    for (const name of ['parse', 'total', 'consumer', 'construct', 'Child', 'Impl', 'nativeConsumer']) {
      expect(targets(name), name).toEqual([]);
    }
    const app = cg!.getNodesByName('App').find((n) => n.kind === 'component' || n.kind === 'function')!;
    expect(app).toBeDefined();
    expect(cg!.getOutgoingEdges(app.id).filter((e) => cg!.getNode(e.target)?.language === 'python')).toEqual([]);
    const page = cg!.getNodesInFile('Page.vue').find((n) => n.kind === 'component')!;
    expect(page).toBeDefined();
    expect(cg!.getOutgoingEdges(page.id).filter((e) => cg!.getNode(e.target)?.language === 'python')).toEqual([]);
  });

  it('preserves web, JVM, native and .NET interoperability', async () => {
    await index({
      'helper.js': 'export function helper() { return 1; }\nexport class WebBase {}',
      'web.ts': "import { helper, WebBase } from './helper.js';\n" +
        'export function webCaller() { return helper(); }\nexport class WebChild extends WebBase {}',
      'JvmBase.java': 'public class JvmBase {}',
      'Child.kt': 'class JvmChild : JvmBase()',
      'native.c': 'int nativeHelper(void) { return 1; }',
      'Native.swift': 'func nativeCaller() { nativeHelper() }',
      'NetBase.cs': 'public class NetBase {}',
      'NetChild.vb': 'Public Class NetChild\n Inherits NetBase\nEnd Class',
      'widget.js': 'export function WebWidget() { return null; }',
      'View.vue': '<template><WebWidget @click="localHandler"/></template>' +
        '<script>import { WebWidget } from "./widget.js"; function localHandler() {} export default {};</script>',
    });
    expect(targets('webCaller')).toContain('helper');
    expect(targets('WebChild')).toContain('WebBase');
    expect(targets('JvmChild')).toContain('JvmBase');
    expect(targets('nativeCaller')).toContain('nativeHelper');
    expect(targets('NetChild')).toContain('NetBase');
    expect(targets('View')).toEqual(expect.arrayContaining(['WebWidget', 'localHandler']));
  });

  it('requires ABI evidence for free functions outside the native family', async () => {
    await index({
      'export.go': 'package main\nimport "C"\n//export StopGo\nfunc StopGo() {}\n',
      'export.rs': '#[no_mangle]\npub extern "C" fn stop_rust() {}\n#[no_mangle]\npub unsafe extern "C" fn stop_unsafe() {}\n',
      'Native.swift': 'func stopNative() {\n StopGo()\n stop_rust()\n stop_unsafe()\n}',
      'ordinary.py': 'def stop_python():\n    return 1\n',
      'Wrong.swift': 'func wrongNative() { stop_python() }',
      'cfunc.c': 'int cfunc(void) { return 1; }',
      'call.go': 'package main\n/* int cfunc(void); */\nimport "C"\nfunc useC() { C.cfunc() }',
      'wrong.py': 'def wrongC():\n    return cfunc()\n',
    });
    expect(targets('stopNative')).toEqual(expect.arrayContaining(['StopGo', 'stop_rust', 'stop_unsafe']));
    expect(targets('wrongNative')).toEqual([]);
    expect(targets('wrongC')).toEqual([]);
  });

  // Upstream's unit version of this case drove name-matcher.ts directly; the
  // fork resolves in the kernel, so the same fixture is indexed and the edges
  // those unit assertions imply are checked end to end.
  it('gates qualified, method and fuzzy winners without choosing a replacement', async () => {
    await index({
      'caller.swift': 'func caller() {\n  Foreign.act()\n  fuzzyName()\n  FUZZYNAME()\n  Container.Winner()\n}',
      'cfunc.c': 'int cfunc(void) { return 1; }',
      'call.go': 'package main\nimport "C"\nfunc useC() { C.cfunc() }',
      'call.rs': 'extern "C" { fn cfunc() -> i32; }\npub fn use_c() { unsafe { cfunc(); } }',
      'foreign.py': 'class Foreign:\n    def act(self):\n        pass\ndef fuzzyName():\n    pass\n',
      'near/pick.py': 'class Container:\n    class Winner:\n        pass\n',
      'far/Winner.swift': 'class Winner {}',
    });
    const caller = cg!.getNodesByName('caller').find((n) => n.kind === 'function')!;
    expect(caller).toBeDefined();
    const edges = cg!.getOutgoingEdges(caller.id).filter((e) => e.kind !== 'contains');
    const reached = edges.map((e) => cg!.getNode(e.target)!);
    // The qualified method, the exact-case fuzzy winner and the nested class
    // are all Python: a Swift caller reaches none of them.
    expect(reached.filter((n) => n.language === 'python').map((n) => n.qualifiedName)).toEqual([]);
    // Rejecting the Python `Container.Winner` does not fall through to the
    // Swift type that shares its bare name.
    expect(reached.filter((n) => n.name === 'Winner')).toEqual([]);
    // Fuzzy is case-exact in Swift: `FUZZYNAME` names nothing.
    expect(edges.filter((e) => (e.metadata as { refName?: string } | undefined)?.refName === 'FUZZYNAME')).toEqual([]);
    // The ABI declarations in call.go and call.rs let a C function stand for
    // their `cfunc` calls; nothing else, and no other language's `cfunc`, does.
    for (const [name, own] of [['useC', 'go'], ['use_c', 'rust']] as const) {
      const from = cg!.getNodesByName(name).find((n) => n.kind === 'function')!;
      expect(from, name).toBeDefined();
      const callees = cg!.getOutgoingEdges(from.id).filter((e) => e.kind === 'calls').map((e) => cg!.getNode(e.target)!);
      for (const n of callees) {
        expect(n.name, name).toBe('cfunc');
        expect(['c', own], name).toContain(n.language);
      }
    }
  });
});
