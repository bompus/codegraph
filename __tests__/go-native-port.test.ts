import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src';
import type { DatabaseConnection } from '../src/db';
import type { QueryBuilder } from '../src/db/queries';
import type { ResolutionContext } from '../src/resolution/types';
import { goFuncFieldEdges } from '../src/resolution/go-func-field-synthesizer';
import { requireKernel } from '../src/extraction/kernel/loader';

interface NativeGo {
  declarationAt(name: string, line: number, column: number): {line: number; column: number; declaredType?: string} | null;
}
let root: string | undefined;
let cg: CodeGraph | undefined;
let native: NativeGo | undefined;
afterEach(() => {
  native=undefined; cg?.close(); cg=undefined;
  if(root) fs.rmSync(root,{recursive:true,force:true,maxRetries:5}); root=undefined;
});
async function index(source: string) {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-native-go-'));
  fs.writeFileSync(path.join(root,'go.mod'),'module example.com/app\n\ngo 1.22\n');
  fs.writeFileSync(path.join(root,'main.go'),source);
  cg=await CodeGraph.init(root,{index:true});
  const internal=cg as unknown as {db: DatabaseConnection; queries: QueryBuilder; resolver: {context: ResolutionContext}};
  const Scope=requireKernel().GoSourceScope!;
  native=new Scope(source);
  return internal;
}
describe('native Go synthesis bridge',()=>{
  it('links a func field only to values put in that struct, including a field copy',async()=>{
    const internal=await index('package main\n'
      +'type Options struct { F func() }\n'
      +'type Runner struct { f func() }\n'
      +'type Other struct { f func() }\n'
      +'func target() {}\nfunc distractor() {}\n'
      +'func build(o Options) Runner { return Runner{f:o.F} }\n'
      +'func wire() { o := Options{F:target}; _ = build(o); _ = Other{f:distractor} }\n'
      +'func (r *Runner) run() { r.f() }\n');
    const edges=await goFuncFieldEdges(internal.queries,internal.resolver.context,async()=>{},40,(name,site)=>{
      const decl=native!.declarationAt(name,site.line,site.column);
      return decl ? {line:decl.line,column:decl.column,type:decl.declaredType} : null;
    });
    expect(edges.map(e=>[cg!.getNode(e.source)?.qualifiedName,cg!.getNode(e.target)?.name,e.metadata?.synthesizedBy,e.metadata?.via])).toEqual([
      ['Runner::run','target','go-func-field','Options.F'],
    ]);
    expect(edges[0]!.provenance).toBe('heuristic');
    expect(edges[0]!.metadata?.registeredAt).toBe('main.go:8');
  });
  it('returns the in-scope declaration and excludes names inside a func result type',async()=>{
    await index('package main\nfunc use(x *other.Client) func(hidden T) {\n switch n {\n case 1: x := v; x.Send()\n case 2: x.Send()\n }\n x.Send()\n}\nfunc future() { later(); later := target; _ = later }\n');
    expect(native!.declarationAt('x',5,9)).toMatchObject({line:2,declaredType:'*other.Client'});
    expect(native!.declarationAt('x',4,17)).toMatchObject({line:4});
    expect(native!.declarationAt('x',4,17)?.declaredType).toBeUndefined();
    expect(native!.declarationAt('later',9,16)).toBeNull();
    expect(native!.declarationAt('hidden',7,1)).toBeNull();
  });
});
