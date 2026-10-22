import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import CodeGraph from '../src';
import type { DatabaseConnection } from '../src/db';
import { requireKernel, type ResolveRefIn } from '../src/extraction/kernel/loader';

interface NativePython {
  close(): void;
  isPythonFromImportModule(ref: ResolveRefIn): boolean;
  resolvePythonModule(spec: string, from: string): string | null;
  pythonPackageRoots(): string;
  pythonPackageReopenScope(changed: string[]): { targetFiles: string[]; sourceDirs: string[]; moduleLeaves: string[] };
  pythonPackageImportBindings(inits: string[]): { filePath: string; localName: string; binding: string | null }[];
  resolvePythonAttribute(ref: ResolveRefIn): { claimed: boolean; targetNodeId: string | null; typeFrom: string[] };
  isPythonOutsideModuleReceiver(name: string, ref: ResolveRefIn): boolean;
  resolvePythonGlobalCall(receiver: string, member: string, ref: ResolveRefIn): { targetNodeId: string | null };
}
let dir: string | undefined;
let cg: CodeGraph | undefined;
let native: NativePython | undefined;
afterEach(() => { native?.close(); native=undefined; cg?.close(); cg=undefined; if(dir)fs.rmSync(dir,{recursive:true,force:true,maxRetries:5}); dir=undefined; });
async function index(files: Record<string,string>) {
  dir=fs.mkdtempSync(path.join(os.tmpdir(),'cg-native-py-'));
  for(const [file,source] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(dir,file)),{recursive:true}); fs.writeFileSync(path.join(dir,file),source); }
  cg=await CodeGraph.init(dir,{index:true});
  // The helper reads a checkpointed snapshot; it never races an index write.
  (cg as unknown as {db: DatabaseConnection}).db.getDb().exec('PRAGMA wal_checkpoint(TRUNCATE)');
  const Kernel=requireKernel().KernelResolver as unknown as new (config: object)=>NativePython;
  native=new Kernel({dbPath:path.join(dir,'.codegraph','codegraph.db'),projectRoot:dir,nodeBuiltinSpecifiers:[],frameworksActive:false,supertypesComplete:true,snapshot:true});
}
function ref(filePath: string, referenceName: string, line: number): ResolveRefIn {
  const node=cg!.getNodesByKind('method').find(n=>n.filePath===filePath && n.startLine<=line && (n.endLine??n.startLine)>=line)
    ??cg!.getNodesByKind('function').find(n=>n.filePath===filePath && n.startLine<=line && (n.endLine??n.startLine)>=line);
  return {fromNodeId:node?.id??'',referenceName,referenceKind:'calls',filePath,language:'python',line,column:8};
}
describe('native Python integration contracts',()=>{
  it('uses service/build roots and limits fixture reach; exposes the same reopen scope',async()=>{
    await index({'pyproject.toml':'[tool.setuptools.packages.find]\nwhere=["lib"]\n','lib/pkg/__init__.py':'','lib/pkg/f.py':'def f(): pass\n','alt/other.py':'def other(): pass\n','app.py':'import pkg\n','tests/fixtures/json.py':'def loads(): pass\n','nested/utils/json.py':'def loads(): pass\n'});
    expect(native!.resolvePythonModule('pkg.f','app.py')).toBe('lib/pkg/f.py');
    expect(native!.resolvePythonModule('json','app.py')).toBeNull();
    expect(native!.resolvePythonModule('json','tests/fixtures/main.py')).toBe('tests/fixtures/json.py');
    expect(native!.pythonPackageRoots()).toContain('lib');
    expect(native!.pythonPackageReopenScope(['lib/pkg/__init__.py'])).toEqual({targetFiles:['lib/pkg/__init__.py','lib/pkg/f.py'],sourceDirs:['lib/pkg'],moduleLeaves:['pkg']});
    const before = native!.pythonPackageRoots();
    fs.writeFileSync(path.join(dir!, 'pyproject.toml'), '[tool.setuptools.packages.find]\nwhere=["alt"]\n');
    expect(native!.pythonPackageRoots()).not.toBe(before);
  });
  it('propagates package re-export bindings to their importers',async()=>{
    await index({'pkg/__init__.py':'from .impl import Service\n','pkg/impl.py':'class Service: pass\n','use.py':'from pkg import Service\n'});
    expect(native!.isPythonFromImportModule({...ref('use.py','pkg',1),referenceKind:'imports',column:0})).toBe(true);
    expect(native!.isPythonFromImportModule({...ref('use.py','Service',1),referenceKind:'imports',column:16})).toBe(false);
    expect(native!.pythonPackageImportBindings(['pkg/__init__.py'])).toContainEqual({filePath:'use.py',localName:'Service',binding:'pkg/impl.py\0Service'});
  });
  it('types an inherited factory-assigned field and reports files sync must reopen',async()=>{
    await index({'client.py':'class Client:\n    def send(self): pass\n\ndef make() -> Client:\n    return Client()\n','base.py':'from client import make\nclass Base:\n    def __init__(self):\n        self.client=make()\n','svc.py':'from base import Base\nclass Svc(Base):\n    def run(self):\n        self.client.send()\n','other.py':'class Other:\n    def send(self): pass\n'});
    const result=native!.resolvePythonAttribute(ref('svc.py','self.client.send',4));
    expect(result.claimed).toBe(true);
    expect(cg!.getNode(result.targetNodeId!)?.qualifiedName).toBe('Client::send');
    expect(result.typeFrom).toEqual(['base.py','client.py']);
  });
  it('distinguishes awaited factory values from coroutine objects', async () => {
    await index({'svc.py': 'class Client:\n    def send(self): pass\nasync def connect() -> Client:\n    return Client()\nclass Service:\n    async def init(self):\n        self.client = await connect()\n        self.pending = connect()\n    def run(self):\n        self.client.send()\n        self.pending.send()\n'});
    const client = native!.resolvePythonAttribute(ref('svc.py', 'self.client.send', 10));
    expect(client.claimed).toBe(true);
    expect(cg!.getNode(client.targetNodeId!)?.qualifiedName).toBe('Client::send');
    const pending = native!.resolvePythonAttribute(ref('svc.py', 'self.pending.send', 11));
    expect(pending.claimed).toBe(true);
    expect(pending.targetNodeId).toBeUndefined();
  });
  it('does not reuse a parameter annotation after reassignment or an inherited field overwritten in a closure', async () => {
    await index({'svc.py': 'class Client:\n    def send(self): pass\nclass Base:\n    client: Client\nclass Service(Base):\n    def init(self, value: Client):\n        value = unknown()\n        self.client = value\n        def nested():\n            self.client = unknown()\n    def run(self):\n        self.client.send()\n'});
    const result = native!.resolvePythonAttribute(ref('svc.py', 'self.client.send', 12));
    expect(result.claimed).toBe(false);
    expect(result.targetNodeId).toBeUndefined();
  });
  it('uses the subclass receiver for a classmethod called directly in a chain', async () => {
    await index({'svc.py': 'class Base:\n    @classmethod\n    def create(cls):\n        return cls()\nclass Child(Base):\n    def send(self): pass\ndef use():\n    Child.create().send()\n'});
    const result = native!.resolvePythonAttribute(ref('svc.py', 'Child.create().send', 8));
    expect(result.claimed).toBe(true);
    expect(cg!.getNode(result.targetNodeId!)?.qualifiedName).toBe('Child::send');
  });
  it('keeps builtin fields and external modules away from project namesakes',async()=>{
    await index({'svc.py':'import subprocess\nclass Svc:\n    data=[]\n    def run(self):\n        self.data.append(1)\n        subprocess.run([])\n','other.py':'class Other:\n    def append(self): pass\n    def run(self): pass\n'});
    expect(native!.resolvePythonAttribute(ref('svc.py','self.data.append',5))).toMatchObject({claimed:true});
    expect(native!.resolvePythonAttribute(ref('svc.py','self.data.append',5)).targetNodeId).toBeUndefined();
    expect(native!.isPythonOutsideModuleReceiver('subprocess',ref('svc.py','subprocess.run',6))).toBe(true);
  });
  it('calls the module-global instance method even when another class shares the name',async()=>{
    await index({'svc.py':'class Service:\n    def run(self): pass\n\n_svc=Service()\ndef use():\n    _svc.run()\n','other.py':'class Other:\n    def run(self): pass\n'});
    const result=native!.resolvePythonGlobalCall('_svc','run',ref('svc.py','_svc.run',6));
    expect(cg!.getNode(result.targetNodeId!)?.qualifiedName).toBe('Service::run');
  });
});
