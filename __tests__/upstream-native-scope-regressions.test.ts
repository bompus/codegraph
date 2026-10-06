import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { CodeGraph } from '../src';
import { vueTemplateCalls } from '../src/extraction/vue-template-calls';

let root: string | undefined;
let graph: CodeGraph | undefined;
afterEach(() => {
  graph?.close();
  graph = undefined;
  if (root) fs.rmSync(root, { recursive: true, force: true });
  root = undefined;
});
async function project(files: Record<string, string>) {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-native-scope-'));
  for (const [file, source] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), source);
  }
  graph = await CodeGraph.init(root, { index: true });
}
function calls(name: string) {
  const from = [...graph!.getNodesByKind('function'), ...graph!.getNodesByKind('method')]
    .find(node => node.name === name)!;
  expect(from).toBeDefined();
  return graph!.getOutgoingEdges(from.id).filter(edge => edge.kind === 'calls')
    .map(edge => graph!.getNode(edge.target)!)
    .map(node => `${node.filePath}:${node.qualifiedName}`).sort();
}

describe('upstream ports preserve declaration scope', () => {
  it('does not borrow a same-file global host object through a shadowing parameter', async () => {
    await project({ 'main.js': 'window.WS = { wsM() {} };\nfunction shadow(window) { return window.WS.wsM(); }\nfunction known() { return window.WS.wsM(); }\n' });
    expect(calls('shadow')).toEqual([]);
    expect(calls('known')).toEqual(['main.js:window.WS::wsM']);
  });
  it('keeps a path-assigned object in the scope of its local root', async () => {
    await project({ 'main.js': 'function a() { const App = {}; App.utils = { pad() {} }; App.utils.pad(); }\nfunction b() { App.utils.pad(); }\n' });
    expect(calls('b')).toEqual([]);
    expect(calls('a')).toEqual(['main.js:App.utils::pad']);
  });
  it.each([['window', false], ['window', true], ['{ window }', false], ['{ window }', true]] as const)('does not publish an object assigned through a local host parameter %s (same file %s)', async (parameter, sameFile) => {
    const declaration = `function define(${parameter}) { window.WS = { wsM() {} }; }\n`;
    const consumer = 'function outside() { window.WS.wsM(); WS.wsM(); }\n';
    await project(sameFile ? { 'main.js': declaration + consumer } : { 'ns.js': declaration, 'use.js': consumer });
    expect(calls('outside')).toEqual([]);
  });
  it.each([false, true])('does not publish a path assigned through a destructured root (same file %s)', async sameFile => {
    const declaration = 'var App = {}; function define({ App }) { App.utils = { pad() {} }; }\n';
    const consumer = 'function outside() { App.utils.pad(); }\n';
    await project(sameFile ? { 'main.js': declaration + consumer } : { 'ns.js': declaration, 'use.js': consumer });
    expect(calls('outside')).toEqual([]);
  });
  it.each([false, true])('keeps a loop-owned path out of global consumers (same file %s)', async sameFile => {
    const declaration = 'var App = {}; for (const App of apps) { App.utils = { pad() {} }; }\n';
    const consumer = 'function outside() { App.utils.pad(); }\n';
    await project(sameFile ? { 'main.js': declaration + consumer } : { 'ns.js': declaration, 'use.js': consumer });
    expect(calls('outside')).toEqual([]);
  });
  it.each(['main.js', 'Page.vue', 'Page.svelte', 'Page.astro'].flatMap(file => [false, true].map(multiline => ({ file, multiline }))))('keeps a destructured member inside its declaring function in $file (multiline $multiline)', async ({ file, multiline }) => {
    const code = "const label = '日本🙂'; const api = { post() {} }; function local() { const { post } = api; return post(); } function outside() { return post(); }\n";
    const script = multiline ? code.replace('; function local()', ';\nfunction local()') : code;
    const source = file.endsWith('.vue') ? `<template><p>日本🙂</p></template>\n<script setup>\n${script}</script>`
      : file.endsWith('.svelte') ? `<p>日本🙂</p>\n<script>\n${script}</script>`
      : file.endsWith('.astro') ? `---\n${script}---\n` : script;
    await project({ [file]: source });
    expect(calls('local')).toEqual([`${file}:api::post`]);
    expect(calls('outside')).toEqual([]);
  });
  it('binds a destructure source at its declaration rather than a later captured call', async () => {
    await project({ 'main.js': 'const api = { post() {} };\nfunction shadow(api) { const { post } = api; return post(); }\nfunction capture() { const { post } = api; function captured(api) { return post(); } return captured(); }\n' });
    expect(calls('shadow')).toEqual([]);
    expect(calls('captured')).toEqual(['main.js:api::post']);
  });
  it('retains a call through a parameter-owned path after its literal assignment', async () => {
    await project({ 'main.js': 'var App = {}; function define({ App }) { App.utils = { pad() {} }; App.utils.pad(); } function outside() { App.utils.pad(); }\n' });
    expect(calls('define')).toEqual(['main.js:App.utils::pad']);
    expect(calls('outside')).toEqual([]);
  });
  it('distinguishes a path root parameter from a global root on the same line', async () => {
    await project({ 'main.js': 'var App = {}; function define() { App.utils = { pad() {} }; } function use(App) { App.utils.pad(); }\n' });
    expect(calls('use')).toEqual([]);
  });
  it.each([
    ['items.map(format => format()) && format()', ['items.map', 'format']],
    ['(function(format) { return format(); })(cb) + format()', ['format']],
    ['items.map(format => { return format(); }) && format()', ['items.map', 'format']],
    ['items.map(({ key: format }) => format()) && key()', ['items.map', 'key']],
  ] as const)('keeps callback parameters local to their expression body: %s', (expression, expected) => {
    expect(vueTemplateCalls(`<template><p>{{ ${expression} }}</p></template>`).map(call => call.name)).toEqual(expected);
  });
  it('binds a renamed Vue slot value without binding its property key', () => {
    const source = '<template><Row #default="{ row: item }">{{ row() }} {{ item() }}</Row></template>';
    expect(vueTemplateCalls(source).map(call => call.name)).toEqual(['row']);
  });
  it.each([false, true])('does not borrow another VB project member when inheritance is %s', async (inherited) => {
    const vbproj = '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup></PropertyGroup></Project>\n';
    await project({
      'A/A.vbproj': vbproj,
      'B/B.vbproj': vbproj,
      'A/Service.vb': `Public Class Service\n${inherited ? ' Inherits BaseA\n' : ''}End Class\nPublic Class BaseA\n Public Sub Ping()\n End Sub\nEnd Class\n`,
      'B/Service.vb': 'Public Class Service\n Public Sub Ping()\n End Sub\nEnd Class\n',
      'A/Main.vb': 'Public Class Main\n Public Sub Run(s As Service)\n s.Ping()\n End Sub\nEnd Class\n',
    });
    expect(calls('Run')).toEqual(inherited ? ['A/Service.vb:BaseA::Ping'] : []);
  });
  it('does not read a Dart getter when assigning its setter', async () => {
    await project({ 'lib/main.dart': 'class Box { int get area => 1; set area(int value) {} }\nvoid write(Box x) { x.area = 4; }\nint read(Box x) => x.area;\n' });
    expect(calls('write')).toEqual([]);
    expect(calls('read')).toEqual(['lib/main.dart:Box::area']);
  });
  it('evaluates a Vue loop source before binding the loop alias', () => {
    const source = '<template><li v-for="rowsFor in rowsFor(user)">{{ rowsFor() }}</li></template>';
    expect(vueTemplateCalls(source).map(call => call.name)).toEqual(['rowsFor']);
  });
});
