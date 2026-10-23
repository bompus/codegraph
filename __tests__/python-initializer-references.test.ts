import { afterEach, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src';
let root: string | undefined;
let cg: CodeGraph | undefined;
afterEach(()=>{cg?.close();cg=undefined;if(root)fs.rmSync(root,{recursive:true,force:true,maxRetries:5});root=undefined;});
it.each([
  ['py_home', ['HANDLERS', 'ROUTES']],
  ['goHome', ['handlers']],
])('attributes %s initializer references once to the declared symbols',async(name, expected)=>{
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-pygo-init-'));
  fs.writeFileSync(path.join(root,'views.py'),'def py_home():\n    pass\n\nHANDLERS={"home":py_home}\nROUTES=[py_home]\n');
  fs.writeFileSync(path.join(root,'handlers.go'),'package main\nfunc goHome() {}\nvar handlers=map[string]func(){"home":goHome}\n');
  cg=await CodeGraph.init(root,{index:true});
  const sources=(name:string)=>{
    const target=cg!.getNodesByKind('function').find(n=>n.name===name)!;
    return cg!.getIncomingEdges(target.id).filter(e=>e.metadata?.fnRef===true).map(e=>cg!.getNode(e.source)?.name).sort();
  };
  expect(sources(name as string)).toEqual(expected);
});
