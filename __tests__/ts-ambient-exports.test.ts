/**
 * A declaration inside `declare module 'x' { … }` or `declare global { … }` is
 * exported, though nothing in it writes `export`:
 *
 *   declare module 'chart.js' {            // ghostfolio's chart.registry.ts
 *     interface PluginOptionsByType<TType extends ChartType> { … }
 *   }
 *   declare global {                       // angular-realworld's app.config.ts
 *     interface Window { __conduit_debug__?: ConduitDebug; }
 *   }
 *
 * Both indexed as unexported file-level interfaces, so the dead-code report
 * listed them: nothing in the repository names them, because chart.js and the
 * browser read them through the type they merge into. TypeScript's binder
 * makes an ambient module body an export context, and a namespace nested in
 * one is ambient too, unless the body holds an export declaration of its own
 * (`export {}`, `export =`, `export default x`). The native extractor now says what
 * the compiler says. `declare namespace X` is unchanged: in a module file it
 * is that file's own namespace and merges with nothing outside. Runs against
 * the native kernel.
 */
import { describe, it, expect, beforeAll, beforeEach, afterEach } from 'vitest';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { tryKernelExtract, resetKernelForTests } from '../src/extraction/kernel';
import type { ExtractionResult, Language } from '../src/types';


/** A module file: augmentations, a global block, and the namespaces that are neither. */
const MODULE_SOURCE = `import type { ChartType } from 'chart.js';

declare module 'chart.js' {
  interface PluginOptionsByType<TType extends ChartType> {
    verticalHoverLine: TType;
  }
  export interface WrittenOut {}
  namespace Plugins {
    interface Nested {}
  }
  type Alias = string;
  enum Position { Top }
  class Positioner {}
  const defaults: Alias;
}

declare global {
  interface Window {
    __conduit_debug__?: unknown;
  }
  namespace NodeJS {
    interface ProcessEnv {
      API_URL?: string;
    }
  }
  var bitwardenContainerService: unknown;
}

declare namespace Settings {
  interface OwnNamespace {}
}

namespace Plain {
  interface Hidden {}
  export interface Shown {}
}

interface FileLocal {}

export const appConfig = {};
`;

/**
 * A script file declaring a module whole, the way an untyped library's typings
 * do. \`export =\` is an export declaration, so the body exports only that.
 */
const SCRIPT_SOURCE = `declare module 'legacy-lib' {
  interface Options {}
  const main: (options: Options) => void;
  export = main;
}

declare module 'listed-lib' {
  interface Unlisted {}
  export interface Listed {}
  export { Unlisted as Renamed };
}

declare module 'open-lib' {
  interface Open {}
}
`;

const ENV_KEYS = ['CODEGRAPH_KERNEL', 'CODEGRAPH_KERNEL_LANGS'] as const;

/** Kinds a statement declares — the nodes that carry an export flag of their own. */
const DECLARATION_KINDS = new Set(['interface', 'class', 'enum', 'type_alias', 'function', 'variable', 'constant', 'component']);

/** `name` → isExported, for every declaration. */
function exportFlags(result: ExtractionResult): Record<string, boolean> {
  const flags: Record<string, boolean> = {};
  for (const node of result.nodes) {
    if (DECLARATION_KINDS.has(node.kind)) flags[node.name] = node.isExported === true;
  }
  return flags;
}

describe('TypeScript ambient module bodies export what they declare', () => {
  let savedEnv: Record<string, string | undefined> = {};

  beforeAll(async () => {
    await initGrammars();
    await loadGrammarsForLanguages(['typescript', 'tsx']);
  });

  beforeEach(() => {
    savedEnv = Object.fromEntries(ENV_KEYS.map((k) => [k, process.env[k]]));
    resetKernelForTests();
  });

  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (savedEnv[k] === undefined) delete process.env[k];
      else process.env[k] = savedEnv[k];
    }
    resetKernelForTests();
  });

  function extract(backend: 'kernel', file: string, source: string, language: Language): ExtractionResult {
    delete process.env.CODEGRAPH_KERNEL;
    process.env.CODEGRAPH_KERNEL_LANGS = 'all';
    const result = tryKernelExtract(file, source, language);
    expect(result, `kernel extraction of ${file}`).not.toBeNull();
    return result!;
  }

  const backends = ['kernel'] as const;

  for (const crlf of [false, true]) {
    const eol = (s: string) => (crlf ? s.replace(/\n/g, '\r\n') : s);
    const label = crlf ? ' (CRLF)' : '';

    it.each(backends)(`declare module and declare global export their declarations: %s${label}`, (backend) => {
      const result = extract(backend, 'src/chart.registry.ts', eol(MODULE_SOURCE), 'typescript');
      expect(exportFlags(result)).toEqual({
        // declare module 'chart.js', and a namespace nested in it.
        PluginOptionsByType: true,
        WrittenOut: true,
        Nested: true,
        Alias: true,
        Position: true,
        Positioner: true,
        defaults: true,
        // declare global, and a namespace nested in it.
        Window: true,
        ProcessEnv: true,
        bitwardenContainerService: true,
        // Neither is an outside scope: a module file's own namespaces.
        OwnNamespace: false,
        Hidden: false,
        Shown: true,
        FileLocal: false,
        appConfig: true,
      });
    });

    it.each(backends)(`a body with an export declaration exports only what it names: %s${label}`, (backend) => {
      const result = extract(backend, 'types/legacy.ts', eol(SCRIPT_SOURCE), 'typescript');
      expect(exportFlags(result)).toEqual({
        Options: false,
        main: false,
        Unlisted: false,
        Listed: true,
        Open: true,
      });
    });

    it.each(backends)(`tsx reads the same blocks: %s${label}`, (backend) => {
      const source = eol(`declare module '@tanstack/react-router' {\n  interface Register {\n    router: unknown;\n  }\n}\nexport const App = () => <main />;\n`);
      const result = extract(backend, 'src/main.tsx', source, 'tsx');
      expect(exportFlags(result).Register).toBe(true);
    });
  }

});
