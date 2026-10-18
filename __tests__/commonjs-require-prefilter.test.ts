/**
 * The CommonJS require pass re-parses a file only when its text could hold a
 * literal `require('./x')` call. The text check is sound only for a tree
 * without syntax errors (recovery can make a call of `require@('./x')`), so it
 * applies only when the kernel reports that its parse of the same text was
 * clean.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { commonJsRequireRefs, mayRequireModulePath } from '../src/extraction/commonjs-requires';
import { tryKernelExtractRaw } from '../src/extraction/kernel';
import * as parseTree from '../src/extraction/parse-tree';
import type { Language } from '../src/types';

// Absolute, so Bun resolves it the same way (oven-sh/bun#43685).
vi.mock(`${__dirname}/../src/extraction/parse-tree`, async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/extraction/parse-tree')>();
  return { ...actual, parseSourceTreeSync: vi.fn(actual.parseSourceTreeSync) };
});
const parse = vi.mocked(parseTree.parseSourceTreeSync);
beforeEach(() => { parse.mockClear(); });

const names = (source: string, language: Language, clean: boolean) =>
  commonJsRequireRefs('entry.ts', source, language, clean).map((ref) => ref.referenceName);

describe('CommonJS require pre-filter', () => {
  it('skips the re-parse of a clean file that only mentions require', () => {
    const source = `// images that do not require color spaces
if (typeof require === 'function') module.exports = factory(require('jquery'));
const f = __webpack_require__(12), g = jest.requireActual('./x'), required = true;`;
    expect(names(source, 'javascript', true)).toEqual([]);
    expect(parse).not.toHaveBeenCalled();
  });

  it('still parses when a clean file has a literal module-path call, through comments, optional calls and empty slots', () => {
    expect(names("const a = require /* loader */ ?.( // path\n './a');", 'javascript', true)).toEqual(['./a']);
    expect(names("import x = require('./b');", 'typescript', true)).toEqual(['./b']);
    expect(names('const c = require<Mod>(`./c`);', 'typescript', true)).toEqual(['./c']);
    // The grammar allows an empty argument slot, so this file parses cleanly.
    expect(names("require(, './d');", 'javascript', true)).toEqual(['./d']);
    expect(parse).toHaveBeenCalledTimes(4);
  });

  it('keeps the tree walk for a file that did not parse cleanly', () => {
    // Error recovery reads this as a call; the text check would not.
    expect(mayRequireModulePath("require@('#x')", 'javascript')).toBe(false);
    expect(names("require@('#x')", 'javascript', false)).toEqual(['#x']);
  });

  it('never rules out a clean file the walk reads a require from', () => {
    const pieces = ['require', 'require', ',', ', ', '...', ' ', '\n', '\t', '/* c */', '// c\n', '?.', '(', '(', ')', "'./a'", '"b/c"', '`./t`', "'pkg'", "'#x'",
      "'.'", '<T>', '<', '>', '<!-- x\n', '-->', '\u2028', '\u00a0', '\u200b', ';', 'x', '=', 'typeof ', '.', ',', '`', '${', '}', '\\',
      "'\\x2e/a'", 'import r = ', 'function(require){', '{', '[', ']', '!', '@', 'new ', 'const ', '+', '-', '#', ':', '"', "'", '*', '/'];
    const literals = ["'./a'", '"b/c"', '`./t`', "'#x'", "'..'", "'pkg'", "'\\x2e/a'", '`./${x}`'];
    let seed = 7;
    const pick = <T>(list: T[]) => {
      seed = (seed + 0x6d2b79f5) >>> 0; // mulberry32
      let z = Math.imul(seed ^ (seed >>> 15), seed | 1);
      z ^= z + Math.imul(z ^ (z >>> 7), z | 61);
      return list[((z ^ (z >>> 14)) >>> 0) % list.length]!;
    };
    const junk = (max: number) => Array.from({ length: pick([...Array(max + 1).keys()]) }, () => pick(pieces)).join('');
    const languages: Language[] = ['javascript', 'jsx', 'typescript', 'tsx'];
    let checked = 0;
    let read = 0;
    for (let n = 0; n < 6000; n++) {
      const source = junk(2) + 'require' + junk(1) + '(' + junk(1) + pick(literals) + junk(1) + ')' + junk(2);
      const language = languages[n % 4]!;
      const tree = parseTree.parseSourceTreeSync(source, language);
      const clean = !!tree && !tree.rootNode.hasError;
      tree?.delete();
      if (!clean) continue;
      checked++;
      const walked = names(source, language, false);
      if (walked.length) read++;
      expect(names(source, language, true), `${language}: ${JSON.stringify(source)}`).toEqual(walked);
    }
    expect(checked).toBeGreaterThan(400);
    expect(read).toBeGreaterThan(150);
  });
});

describe('kernel parse flag', () => {
  it('is set for a clean parse of the file itself and clear otherwise', () => {
    expect(tryKernelExtractRaw('a.js', "const a = require('./a');\n", 'javascript')?.sourceParsedClean).toBe(true);
    expect(tryKernelExtractRaw('b.js', "const b = require@('./b');\n", 'javascript')?.sourceParsedClean).toBe(false);
    // A Flow file is parsed from a blanked copy, not its own text.
    expect(tryKernelExtractRaw('c.js', "// @flow\ntype T = {| a: number |};\nconst c = require('./c');\n", 'tsx')?.sourceParsedClean).toBe(false);
  });
});
