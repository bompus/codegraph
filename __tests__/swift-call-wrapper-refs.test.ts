/**
 * Swift calls the grammar wraps keep their name and receiver in the kernel's
 * extraction: after `try!`, `try?` and `try` (a Windows grammar build once read
 * `try!` as a logical not, so `try! Message2(data: data)` was a call of
 * `!Message2`), and through a prefix operator, `try` or `await` (`!isReady()`
 * parses as `(!isReady)()`, `try! Decoder().decode(d)` as
 * `(try! Decoder()).decode(d)`).
 *
 * The fork's kernel is the only Swift extractor, so upstream's parity test
 * (kernel versus wasm) does not apply; these are its absolute assertions on the
 * kernel's references, read from the same two fixtures.
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { extractFromSource } from '../src/extraction';

const FIXTURES = path.join(__dirname, 'fixtures', 'kernel-parity');
const read = (name: string): string => fs.readFileSync(path.join(FIXTURES, name), 'utf8');

describe('swift calls after try', () => {
  const source = read('TortureTry.swift');
  const lf = extractFromSource('fixtures/TortureTry.swift', source, 'swift');
  const crlf = extractFromSource('fixtures/TortureTry.swift', source.replace(/(?<!\r)\n/g, '\r\n'), 'swift');
  const lines = source.split(/\r?\n/);
  /** Where `token` starts on the first line containing `text`. */
  const at = (text: string, token: string): { line: number; column: number } => {
    const i = lines.findIndex((l) => l.includes(text));
    expect(i, `fixture line "${text}"`).toBeGreaterThanOrEqual(0);
    return { line: i + 1, column: lines[i]!.indexOf(token, lines[i]!.indexOf(text)) };
  };

  it.each([['LF', lf], ['CRLF', crlf]] as const)('keeps each callee and its position (%s)', (_label, result) => {
    const refs = result.unresolvedReferences;
    const expectRef = (referenceName: string, referenceKind: string, where: { line: number; column: number }) =>
      expect(refs).toContainEqual(expect.objectContaining({ referenceName, referenceKind, ...where }));
    expectRef('Message2', 'calls', at('let msg2 = try! Message2(data: data!)', 'Message2'));
    expectRef('Realm', 'calls', at('try! Realm()', 'Realm'));
    expectRef('Repository', 'calls', at('static let shared = try! Repository', 'Repository'));
    expectRef('JSONDecoder.make', 'calls', at('try! JSONDecoder.make(', 'JSONDecoder'));
    expectRef('Settings.Defaults.load', 'calls', at('try! Settings.Defaults.load', 'Settings'));
    expectRef('Settings', 'references', at('try! Settings.Defaults.load', 'Settings'));
    expectRef('FileManager', 'references', at('try! FileManager.default', 'FileManager'));
    expectRef('Item', 'calls', at('try! Item(raw: $0)', 'Item'));
  });

  it('leaves no reference with the `!` of a `try!` in its name', () => {
    const tryBangLines = new Set(lines.flatMap((l, i) => (l.includes('try!') ? [i + 1] : [])));
    expect(lf.unresolvedReferences.filter((r) => tryBangLines.has(r.line) && r.referenceName.includes('!'))).toEqual([]);
  });
});

describe('swift calls through a prefix operator, try or await', () => {
  const source = read('TortureWrappedCalls.swift');
  const lf = extractFromSource('fixtures/TortureWrappedCalls.swift', source, 'swift');
  const crlf = extractFromSource('fixtures/TortureWrappedCalls.swift', source.replace(/(?<!\r)\n/g, '\r\n'), 'swift');
  const lines = source.split(/\r?\n/);

  it.each([['LF', lf], ['CRLF', crlf]] as const)('keeps each callee and receiver (%s)', (_label, result) => {
    /** The `kind` refs on the first line containing `text`, by name. */
    const on = (text: string, kind: 'calls' | 'references'): string[] => {
      const i = lines.findIndex((l) => l.includes(text));
      expect(i, `fixture line "${text}"`).toBeGreaterThanOrEqual(0);
      return result.unresolvedReferences
        .filter((r) => r.line === i + 1 && r.referenceKind === kind)
        .map((r) => r.referenceName)
        .sort();
    };
    expect(on('let a = !isReady()', 'calls')).toEqual(['isReady']);
    expect(on('let b = !store.contains(id)', 'calls')).toEqual(['store.contains']);
    expect(on('let c = !Config.check()', 'calls')).toEqual(['Config.check']);
    expect(on('let d = -abs(id)', 'calls')).toEqual(['abs']);
    expect(on('let e = !Settings.Defaults.isOn()', 'calls')).toEqual(['Settings.Defaults.isOn']);
    expect(on('let e = !Settings.Defaults.isOn()', 'references')).toEqual(['Settings']);
    expect(on('let h = !Config.shared.isEnabled', 'references')).toEqual(['Config']);
    expect(on('let i = ~Mask.all.rawValue', 'references')).toEqual(['Mask']);
    expect(on('var negated: Bool = !Store.isEmpty()', 'calls')).toEqual(['Store.isEmpty']);
    expect(on('let j = try! Decoder()', 'calls')).toEqual(['Decoder', 'Decoder().decode']);
    expect(on('let k = try Parser.make()', 'calls')).toEqual(['Parser.make', 'Parser.make().parse']);
    expect(on('let l = try! Parser.make().tokens()', 'calls'))
      .toEqual(['Parser.make', 'Parser.make().tokens', 'Parser.make().tokens().first']);
    expect(on('let o = try !isValid(data)', 'calls')).toEqual(['isValid']);
    expect(on('let q = !(items.isEmpty)', 'calls')).toEqual(['!']);
    expect(on('let r = -(id + 1)', 'calls')).toEqual([]);
    expect(on('let s = -(id)', 'calls')).toEqual([]);
    expect(on('return !store.remove(id)', 'calls')).toEqual(['store.remove']);
    expect(on('guard !Task.isCancelled', 'calls')).toEqual(['isReady']);
    expect(on('guard !Task.isCancelled', 'references')).toEqual(['Task']);
    // Unchanged: an implicit member keeps its dot, a lowercase chain stays bare.
    expect(on('let p: Result<Int, Error> = .success(id)', 'calls')).toEqual(['.success']);
    expect(on('let n = await loader.load().items()', 'calls')).toEqual(['items', 'loader.load']);
  });
});
