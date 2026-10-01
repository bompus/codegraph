import { afterEach, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';

let root: string;
let graph: CodeGraph | undefined;
afterEach(() => { graph?.close(); graph = undefined; if (root) fs.rmSync(root, { recursive: true, force: true }); });
async function index(files: Record<string, string>) {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-module-controls-'));
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    fs.writeFileSync(path.join(root, name), text);
  }
  graph = await CodeGraph.init(root, { index: true });
}
function calls(file: string) {
  return graph!.getOutgoingEdgesFrom(graph!.getNodesInFile(file).map(n => n.id)).filter(e => e.kind === 'calls')
    .map(e => { const n = graph!.getNode(e.target)!; return `${n.filePath}:${n.name}`; }).sort();
}

it('keeps CommonJS local and parameter shadows and refuses forwarding cycles', async () => {
  await index({
    'impl.js': 'function real() { return 1; }\nmodule.exports = real;\n',
    'a.js': "module.exports = require('./b');\n",
    'b.js': "module.exports = require('./a');\n",
    'use.js': "const external = require('outside');\nconst run = require('./impl');\nconst cycle = require('./a');\nfunction direct() { return run(); }\nfunction shadow(run) { return run(); }\nfunction own() { const external = () => 2; return external(); }\nfunction unresolved() { cycle(); external(); }\n",
  });
  expect(calls('use.js')).toEqual(['impl.js:real', 'use.js:external']);
});

it('follows nested namespaces while refusing shadowed roots and namespace cycles', async () => {
  await index({
    'impl.ts': 'export function work() { return 1; }\n',
    'inner.ts': "export * as util from './impl';\n",
    'barrel.ts': "export * as core from './inner';\nexport * as loop from './barrel';\n",
    'use.ts': "import * as api from './barrel';\nexport function use() { return api.core.util.work(); }\nexport function shadow(api: any) { return api.core.util.work(); }\nexport function cycle() { return api.loop.loop.work(); }\n",
  });
  expect(calls('use.ts')).toEqual(['impl.ts:work']);
});

it('does not treat a private Lua function or another table as a module export', async () => {
  await index({
    'module.lua': 'local M = {}\nlocal function hidden(v) return v end\nlocal other = { renamed = hidden }\nfunction M.visible(v) return v end\nreturn M\n',
    'use.lua': 'local hidden = require("module").hidden\nlocal renamed = require("module").renamed\nlocal visible = require("module").visible\nlocal function run() hidden(1); renamed(2); visible(3) end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:visible']);
});

it('keeps same-line Lua aliases and does not leak a do-block alias', async () => {
  await index({
    'module.lua': 'local M = {}\nfunction M.work(v) return v end\nreturn M\n',
    'use.lua': 'local m = require("module"); local work = m.work\nlocal function run() work(1) end\ndo local hidden = require("module").work; hidden(2) end\nlocal function outside() hidden(3) end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:work', 'module.lua:work']);
});

it('refuses a shadowed route config and a computed path fragment', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'paths.ts': "export const paths = { login: { getHref: () => '/login' }, other: { getHref: (id: string) => `/login${id}` } };\n",
    'app.tsx': "import { Route, Link } from 'react-router';\nimport { paths } from './paths';\nexport function Login() { return null; }\nexport function Routes() { return <Route path='/login' element={<Login/>}/>; }\nexport function Valid() { return <Link to={paths.login.getHref()}>login</Link>; }\nexport function Shadow(paths: any) { return <Link to={paths.login.getHref()}>shadow</Link>; }\nexport function Computed(id: string) { return <Link to={paths.other.getHref(id)}>computed</Link>; }\n",
  });
  const origins = graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n => n.id)).filter(e => e.kind === 'navigates').map(e => graph!.getNode(e.source)!.name);
  expect(origins).toEqual(['Valid']);
});

it('does not follow a Lua alias through a callable parameter', async () => {
  await index({
    'module.lua': 'local M = {}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local work = require("module").work\nlocal function shadow(work) work() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('does not cache a deep Lua miss as a later shallow miss', async () => {
  await index({
    'A.lua': 'local m = require("B")\nreturn { work = m.work }\n',
    'B.lua': 'local m = require("C")\nreturn { work = m.work }\n',
    'C.lua': 'local m = require("D")\nreturn { work = m.work }\n',
    'D.lua': 'local m = require("E")\nreturn { work = m.work }\n',
    'E.lua': 'local M = {}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local deep = require("A").work\nlocal shallow = require("C").work\nlocal function run() deep(); shallow() end\n',
  });
  expect(calls('use.lua')).toEqual(['E.lua:work']);
});

it('uses a CommonJS forwarded default before a barrel member', async () => {
  await index({
    'impl.js': 'function main() { return 1; }\nmodule.exports = main;\n',
    'barrel.js': "module.exports = require('./impl');\nmodule.exports.extra = function extra() { return 2; };\n",
    'use.js': "const main = require('./barrel');\nfunction run() { return main(); }\n",
  });
  expect(calls('use.js')).toEqual(['impl.js:main']);
});

it('reads the actual default-exported route config', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'config.ts': "const paths = { login: { getHref: () => '/wrong' } };\nconst actual = { login: { getHref: () => '/right' } };\nexport default actual;\n",
    'app.tsx': "import { Route, Link } from 'react-router';\nimport paths from './config';\nexport function Screen() { return null; }\nexport function Routes() { return <><Route path='/right' element={<Screen/>}/><Route path='/wrong' element={<Screen/>}/></>; }\nexport function Use() { return <Link to={paths.login.getHref()}>open</Link>; }\n",
  });
  const destinations = graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n => n.id)).filter(e => e.kind === 'navigates').map(e => graph!.getNode(e.target)!.name);
  expect(destinations).toEqual(['/right']);
});

it('keeps a later global Lua function after a block-local alias expires', async () => {
  await index({
    'module.lua': 'local M = {}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'do local hidden = require("module").work; hidden() end\nfunction hidden() return 2 end\nlocal function outside() hidden() end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:work', 'use.lua:hidden']);
});

it('follows a nested Lua exported table field without selecting its root sibling', async () => {
  await index({
    'module.lua': 'local function root() return 1 end\nlocal function nested() return 2 end\nreturn { work = root, child = { work = nested } }\n',
    'use.lua': 'local run = require("module").child.work\nlocal function use() run() end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:nested']);
});

it('uses a Lua alias declared inside a parameter scope', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local function run(work) local work=require("module").work; work() end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:work']);
});
it('uses a local Lua function that shadows an earlier alias', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local work=require("module").work\nlocal function work() return 2 end\nlocal function run() work() end\n',
  });
  expect(calls('use.lua')).toEqual(['use.lua:work']);
});
it('refuses a route config key overwritten by a later spread', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'paths.ts': "const overrides={login:{getHref:()=>'/right'}}; export const paths={login:{getHref:()=>'/wrong'},...overrides};\n",
    'app.tsx': "import {Route,Link} from 'react-router'; import {paths} from './paths'; export function Right(){return null} export function Wrong(){return null} export function Routes(){return <><Route path='/right' element={<Right/>}/><Route path='/wrong' element={<Wrong/>}/></>} export function Go(){return <Link to={paths.login.getHref()}>go</Link>}\n",
  });
  const destinations=graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n=>n.id)).filter(e=>e.kind==='navigates').map(e=>graph!.getNode(e.target)!.name);
  expect(destinations).not.toContain('/wrong');
});

it('refuses a route config factory argument as the returned config', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'paths.ts': "function replaceConfig(value: any){return {login:{getHref:()=>'/right'}}} export const paths=replaceConfig({login:{getHref:()=>'/wrong'}});\n",
    'app.tsx': "import {Route,Link} from 'react-router'; import {paths} from './paths'; export function Wrong(){return null} export function Routes(){return <Route path='/wrong' element={<Wrong/>}/>} export function Go(){return <Link to={paths.login.getHref()}>go</Link>}\n",
  });
  expect(graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n=>n.id)).filter(e=>e.kind==='navigates')).toEqual([]);
});

it('does not export a method of a shadowed Lua module table', async () => {
  await index({
    'module.lua': 'local M={}\ndo local M={}; function M.work() return 1 end end\nreturn M\n',
    'use.lua': 'local run=require("module").work\nlocal function use() run() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('does not mistake a Lua table method for a bare global after an alias expires', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'do local hidden=require("module").work end; local M={}; function M.hidden() return 2 end; local function outside() hidden() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});
it('reads the exported route config after a same-line private declaration', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'paths.ts': "function make(){const paths={login:{getHref:()=>'/wrong'}};return paths} export const paths={login:{getHref:()=>'/right'}};\n",
    'app.tsx': "import {Route,Link} from 'react-router'; import {paths} from './paths'; export function Right(){return null} export function Wrong(){return null} export function Routes(){return <><Route path='/right' element={<Right/>}/><Route path='/wrong' element={<Wrong/>}/></>} export function Go(){return <Link to={paths.login.getHref()}>go</Link>}\n",
  });
  const destinations=graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n=>n.id)).filter(e=>e.kind==='navigates').map(e=>graph!.getNode(e.target)!.name);
  expect(destinations).toEqual(['/right']);
});

it('does not export fields from a shadowed Lua module table initializer', async () => {
  await index({
    'module.lua': 'local function hidden() return 1 end\nlocal M={}\ndo local M={work=hidden} end\nreturn M\n',
    'use.lua': 'local run=require("module").work\nlocal function use() run() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});
it('does not follow Lua aliases through nil locals or loop variables', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local work=require("module").work\nlocal function one() local work; work() end\nlocal function two() local other, work=1; work() end\nfor work in callbacks() do work() end\nfor work=1,2 do work() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('does not follow an overwritten Lua alias declaration', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local work=require("module").work\nlocal function alternate() return 2 end\nwork=alternate\nlocal function use() work() end\n',
  });
  expect(calls('use.lua')).not.toContain('module.lua:work');
});

it('keeps the outer Lua binding in local initializers and loop iterators', async () => {
  await index({
    'module.lua': 'local M={}\nfunction M.work() return 1 end\nreturn M\n',
    'use.lua': 'local work=require("module").work\nlocal function one() local work=work() end\nfor work in work() do end\n',
  });
  expect(calls('use.lua')).toEqual(['module.lua:work','module.lua:work']);
});

it('keeps Lua callable definitions assigned to forward-declared locals', async () => {
  await index({
    'use.lua': 'local later\nlocal function early() later() end\nlater = function() return 1 end\nlocal other\nfunction other() return 2 end\nlocal function use() other() end\n',
  });
  expect(calls('use.lua')).toEqual(['use.lua:later','use.lua:other']);
});
it('does not bind a captured Lua library member to its later wrapper', async () => {
  await index({
    'use.lua': 'local old_sleep = ngx.sleep\nfunction ngx.sleep() old_sleep() end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('refuses a Lua nil binding before its direct definition and after an overwrite', async () => {
  await index({
    'use.lua': 'local first; first(); first=function() return 1 end\nlocal second; second=function() return 2 end; second=nil; second()\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('keeps nested Lua forward-declared functions with qualified graph names', async () => {
  await index({
    'use.lua': 'local function factory() local later; do function later() return 1 end end; local function run() later() end; return run end\n',
  });
  expect(calls('use.lua')).toEqual(['use.lua:later']);
});
it('does not export a replaced Lua module table or deleted method', async () => {
  await index({
    'a.lua': 'local M={}\nfunction M.work()return 1 end\nM={}\nreturn M\n',
    'b.lua': 'local M={}\nfunction M.work()return 1 end\nM.work=nil\nreturn M\n',
    'use.lua': 'local a=require("a").work\nlocal b=require("b").work\nlocal function use()a();b()end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});
it('does not use a nested return or concatenation as a literal route destination', async () => {
  await index({
    'package.json': JSON.stringify({ dependencies: { react: '^18', 'react-router': '^7' } }),
    'paths.ts': "export const paths={nested:{getHref:()=>{function helper(){return '/wrong'} return '/right'}},joined:{getHref:()=>'/wrong' + '/right'},direct:{getHref:()=>{return '/right';}}};\n",
    'app.tsx': "import {Route,Link} from 'react-router'; import {paths} from './paths'; export function Right(){return null} export function Wrong(){return null} export function Routes(){return <><Route path='/right' element={<Right/>}/><Route path='/wrong' element={<Wrong/>}/></>} export function Go(){return <><Link to={paths.nested.getHref()}>nested</Link><Link to={paths.joined.getHref()}>joined</Link><Link to={paths.direct.getHref()}>direct</Link></>}\n",
  });
  const destinations=graph!.getOutgoingEdgesFrom(graph!.getNodesInFile('app.tsx').map(n=>n.id)).filter(e=>e.kind==='navigates').map(e=>graph!.getNode(e.target)!.name);
  expect(destinations).toEqual(['/right']);
});
it('does not flatten a destructured CommonJS nested-member binding', async () => {
  await index({
    'module.js': 'exports.work=function work(){}; exports.nested={work:function nested(){}};\n',
    'use.js': "const {work}=require('./module').nested; function use(){work();}\n",
  });
  expect(calls('use.js')).toEqual([]);
});
it('does not export an initializer field or same-line method after a Lua table replacement', async () => {
  await index({
    'a.lua': 'local function work()end; local M={work=work}; M={}; return M\n',
    'b.lua': 'local M={}; function M.work()end; M={}; return M\n',
    'use.lua': 'local a=require("a").work; local b=require("b").work; local function use()a();b()end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});

it('refuses a nested Lua export after its parent field is replaced', async () => {
  await index({
    'module.lua': 'local function work()end; local M={child={work=work}}; M.child={}; return M\n',
    'use.lua': 'local run=require("module").child.work; local function use()run()end\n',
  });
  expect(calls('use.lua')).toEqual([]);
});
