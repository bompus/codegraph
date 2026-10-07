import { describe, it, expect } from 'vitest';
import { extractFromSource } from '../src/extraction';

describe('native TypeScript and JavaScript extraction', () => {
  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('named object literals own their members: %s (#2300)', (ext, language) => {
    const source = [
      '/* é😀 */ const Api = { read() { helper(); }, close: () => helper(), gen: function* () { yield 1; }, \'quoted-key\': function () {}, [dyn()]() { helper(); }, eager: helper(), alias: helper, helper };',
      'function helper() {}',
      '/** Saved. */',
      'export const Exported = { save() { helper(); } };',
      '(function () { const Local = { run() { helper(); } }, data = { x: 1 }; Local.run(); window.WS = { ws() { Local.run(); } }; })();',
      'App.utils = { pad(s) { return s; } };',
      '// The page.',
      'dw_page = { start() { helper(); } };',
      'function setup() { ns.late = { go() {} }; let h; h = { on() {} }; }',
      'module.exports = { cjs() {} };',
      'Foo.prototype = { proto() {} };',
      'self.handlers = { click() {} };',
      'consume({ ephemeral() {} });',
      '',
    ].join('\n');
    for (const ending of ['\n', '\r\n']) {
      const result = extractFromSource(`literal.${ext}`, source.replace(/\n/g, ending), language);
      expect(result.nodes.map((n) => n.qualifiedName)).toEqual(expect.arrayContaining([
        'Api::read', 'Api::close', 'Api::gen', 'Api::quoted-key', 'Exported::save', 'Local::run', 'window.WS',
        'window.WS::ws', 'App.utils::pad', 'dw_page::start', 'ns.late::go', 'self.handlers::click',
      ]));
      // A plain name reassigned inside a function is a local, not a namespace.
      for (const name of ['cjs', 'proto', 'ephemeral', 'data', 'on']) expect(result.nodes.some((n) => n.name === name), name).toBe(false);
    }
  });

  it.each(['bundle.js', 'vendor-min.js', 'vendor.min.js'])('a minified bundle keeps its literals unowned: %s (#2300)', (file) => {
    // Mostly long lines dense with code punctuation, as a minifier writes them.
    const line = `var a={b:function(){return c(1,2)},d:function(e){return e}};window.L={f:function(){return a.b()}};`.repeat(40);
    const result = extractFromSource(file, `${line}\n${line}\n`, 'javascript');
    expect(result.nodes.some((n) => n.kind === 'function' && (n.name === 'b' || n.name === 'f'))).toBe(false);
  });

  describe.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('a store exported by a later statement: %s', (ext, language) => {
    it.each(['LF', 'CRLF'])('keeps its actions (%s)', (ending) => {
      // `export default useStore;` is found by a multiline regex over the
      // source. JS's `$` matches before the `\r` of a CRLF line ending; the
      // kernel's must too, or a Windows checkout loses the store's actions.
      const source = [
        'import { create } from "zustand";',
        'const useStore = create((set) => ({ inc: () => set({}) }));',
        'export default useStore;',
        '',
      ].join(ending === 'CRLF' ? '\r\n' : '\n');
      const result = extractFromSource(`store.${ext}`, source, language);
      expect(result.nodes.some((n) => n.kind === 'function' && n.name === 'inc')).toBe(true);
    });

    it.each([
      ['items$', 'export { items$ };', true],
      ['$items', 'export { $items as default };', true],
      ['items', 'export { items$ };', false],
      ['items', 'export { $items };', false],
    ] as const)('an export clause names %s only whole: `%s`', (name, exportLine, kept) => {
      // `\b` takes a `$` for a separator: it can't bound `items$` or
      // `$items`, and it finds `items` inside both. `other` keeps the
      // negative cases explicit without a parity backend.
      const source = [
        'import { create } from "zustand";',
        `const ${name} = create((set) => ({ inc: () => set({}) }));`,
        'const other = 1;',
        exportLine,
        '',
      ].join('\n');
      const result = extractFromSource(`store.${ext}`, source, language);
      expect(result.nodes.some((n) => n.kind === 'function' && n.name === 'inc')).toBe(kept);
    });
  });

  describe.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('a store whose name has a `$`, exported by a later statement: %s', (ext, language) => {
    // The later-export check builds a regex from the name. Its `$` must match
    // the character, as the kernel's escaped pattern does, not a line end.
    const store = (name: string, ...exportLines: string[]) => [
      'import { create } from "zustand";',
      `const ${name} = create((set) => ({ inc: () => set({}) }));`,
      ...exportLines,
      '',
    ].join('\n');

    it.each([
      ['items$', 'export default items$;'],
      ['$store', 'export default $store;'],
      ['a$b', 'export { a$b };'],
    ])('keeps its actions (%s)', (name, exportLine) => {
      const result = extractFromSource(`store.${ext}`, store(name, exportLine), language);
      expect(result.nodes.some((n) => n.kind === 'function' && n.name === 'inc')).toBe(true);
    });

    it('is not exported by a different name at a line end', () => {
      const result = extractFromSource(`store.${ext}`, store('items$', 'const items = 1;', 'export {', '  items', '};'), language);
      expect(result.nodes.some((n) => n.kind === 'function' && n.name === 'inc')).toBe(false);
    });
  });

  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('same-line accessors retain distinct identities after Unicode: %s (#1349)', (ext, language) => {
    const source = 'class Point { /* é😀 */ get x() { return read(); } set x(v) { write(v); } }';
    const result = extractFromSource(`point.${ext}`, source, language);
    const x = result.nodes.filter((n) => n.name === 'x');
    expect(x).toHaveLength(2);
    expect(x[1]!.id).toBe(`${x[0]!.id}:${source.indexOf('set x')}`);
    expect(result.unresolvedReferences.filter((r) => r.referenceKind === 'calls').map((r) => [r.fromNodeId, r.referenceName]))
      .toEqual([[x[0]!.id, 'read'], [x[1]!.id, 'write']]);
    // No state leaks between files or repeated extractions.
    const stable = ({ durationMs: _duration, ...snapshot }: typeof result) => ({
      ...snapshot, nodes: snapshot.nodes.map(({ updatedAt: _timestamp, ...node }) => node),
    });
    expect(stable(extractFromSource(`point.${ext}`, source, language))).toEqual(stable(result));
  });

  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('leaves nested identifier receivers unresolved and keeps argument calls: %s (#1566)', (ext, language) => {
    const result = extractFromSource(`fixture.${ext}`, `
function readKey() { return 'answer'; }
function local() {
  const values = new Map();
  return values.get(readKey());
}
function nested(holder) {
  holder.values.get(readKey());
  holder.values?.get(readKey());
  holder['values'].get(readKey());
  holder.deep.values.get(readKey());
}
`, language);
    const nested = result.nodes.find((n) => n.name === 'nested' && n.kind === 'function');
    expect(nested).toBeDefined();
    expect(result.unresolvedReferences.filter((r) => r.referenceKind === 'calls' && r.fromNodeId === nested!.id)
      // Qualified sites are retained for effects; computed keys still make no
      // receiver claim. All four calls inside arguments must also survive.
      .map((r) => r.referenceName)).toEqual([
        'holder.values.get', 'readKey', 'holder.values.get', 'readKey',
        'readKey', 'holder.deep.values.get', 'readKey',
      ]);
    expect(result.unresolvedReferences.some((r) => r.referenceName === 'values.get')).toBe(true);
  });

  describe.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('private field receivers: %s (#1987)', (ext, language) => {
    it.each(['LF', 'CRLF'])('preserves private fields and optional calls (%s)', (ending) => {
      const source = `
class Mailer { send() {} }
class Vault {
  #mailer = new Mailer();
  #items = new Set();
  notify() { this.#mailer?.send(); }
  optional() { this.#mailer.send?.(); }
  put() { this.#items?.add('x'); }
}
`;
      const result = extractFromSource(`vault.${ext}`, ending === 'CRLF' ? source.replace(/\n/g, '\r\n') : source, language);
      expect(result.unresolvedReferences.filter(r => r.referenceKind === 'calls').map(r => r.referenceName))
        .toEqual(['this.#mailer.send', 'this.#mailer.send', 'this.#items.add']);
    });
  });

  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('peels transparent receivers and drops untyped expression receivers: %s', (ext, language) => {
    const typed = language === 'typescript' || language === 'tsx';
    const result = extractFromSource(`fixture.${ext}`, `
function list() { return []; }
class Runner { go() { return 1; } }
async function exprReceivers(x, y) {
  (await list()).map(g);
  (x).run();
  ${typed ? 'x!.run(); (y as X).run(); (x satisfies X).stop(); getTarget("a")!.install(); if (x && y!.c.has(1)) {} if (x && this.e!.c.has(1)) {}' : ''}
  (a ?? b).map(g);
  arr[0].run();
  f().list.map(g);
  (() => 1).call(null);
  this.a.b.run();
  super.stop();
  new Runner().go();
  window.Api.start();
}
`, language);
    const fn = result.nodes.find((n) => n.name === 'exprReceivers');
    expect(result.unresolvedReferences.filter((r) => r.referenceKind === 'calls' && r.fromNodeId === fn!.id)
      .map((r) => r.referenceName)).toEqual([
        'list().map', 'list', 'x.run',
        ...(typed ? ['x.run', 'y.run', 'x.stop', 'getTarget().install', 'getTarget', 'has'] : []),
        'f', 'run', 'stop', 'go', 'window.Api.start',
      ]);
  });

  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'], ['js', 'javascript'], ['jsx', 'jsx'],
  ] as const)('walks a module-scope destructuring declaration like a body does: %s (#2340)', (ext, language) => {
    const typed = language === 'typescript' || language === 'tsx';
    const result = extractFromSource(`fixture.${ext}`, `
import { handler } from './h';
const { a } = useFoo(1);
let [b, c] = pair();
var { d: { e } } = nested();
const { f = fallback() } = withDefault(handler);
const [g = other(), ...rest] = list(() => inArrow(handler));
export const { h } = exported(new Store());
export const { useGetUserQuery } = api;
${typed ? 'const { k }: Shape = make();' : ''}
function probe() { const { j } = inner(); return j; }
export function second() { return probe(); }
`, language);
    const calls = result.unresolvedReferences.filter((r) => r.referenceKind === 'calls').map((r) => r.referenceName);
    expect(calls).toEqual(expect.arrayContaining([
      'useFoo', 'pair', 'nested', 'fallback', 'withDefault', 'other', 'list', 'inArrow', 'exported', 'inner',
      ...(typed ? ['make'] : []),
    ]));
  });

  it.each([
    ['ts', 'typescript'], ['tsx', 'tsx'],
  ] as const)('typed styled tags are components, parsed as comparisons or not: %s', (ext, language) => {
    // `styled.div<Props>\`…\`` parses as `(styled.div < Props) > \`…\``; the
    // type argument's own operators sit between (`<A | B>`, `<Partial<A>>`).
    const result = extractFromSource(`styles.${ext}`, `
import styled, { css } from 'styled-components';
import { s } from './theme';
type Props = { align: 'start' | 'end' };
const Plain = styled.div\`color: red;\`;
export const Wrapper = styled.div<Props>\`color: \${(p) => s(p.align)};\`;
const CloseAction = styled.div<{ animation: Animation | null }>\`top: 0;\`;
const Content = styled(Plain)<Props>\`padding: 4px;\`;
const NudeButton = styled(Plain).attrs((props: Props) => ({ type: "button" }))<Props>\`\`;
const Either = styled.div<Props | Other>\`color: red;\`;
const Nested = styled.div<Partial<Props>>\`color: red;\`;
const Mixin = css<Props>\`color: red;\`;
const Compared = styled.length < LIMIT > 2;
const lowerCase = styled.div<Props>\`color: red;\`;
`, language);
    const kind = (name: string) => result.nodes.filter((n) => n.name === name).map((n) => n.kind);
    for (const name of ['Plain', 'Wrapper', 'CloseAction', 'Content', 'NudeButton', 'Either', 'Nested']) {
      expect(kind(name), name).toEqual(['component']);
    }
    for (const name of ['Mixin', 'Compared', 'lowerCase']) expect(kind(name), name).toEqual(['constant']);
  });

});
