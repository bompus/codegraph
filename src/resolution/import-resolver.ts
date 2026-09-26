/**
 * Import Resolver
 *
 * Resolves import paths to actual files and symbols.
 */

import * as fs from 'fs';
import * as path from 'path';
import { Binding, Language } from '../types';
import { UnresolvedRef,  ResolutionContext, ImportMapping } from './types';
import { applyAliases } from './path-aliases';
import { resolveWorkspaceImport } from './workspace-packages';

/**
 * Extension resolution order by language
 */
const EXTENSION_RESOLUTION: Record<string, string[]> = {
  typescript: ['.ts', '.tsx', '.d.ts', '.js', '.jsx', '/index.ts', '/index.tsx', '/index.js'],
  // ArkTS imports both `.ets` components and plain `.ts` logic modules —
  // HarmonyOS projects are always a mix. `/Index.ets` (capital I) is ohpm's
  // module-entry convention, hit when a bare workspace import ("data") is
  // rewritten to the member's directory; lowercase variants for safety.
  arkts: ['.ets', '.ts', '.d.ts', '.js', '/Index.ets', '/index.ets', '/index.ts', '/index.js'],
  javascript: ['.js', '.jsx', '.mjs', '.cjs', '.xsjs', '.xsjslib', '/index.js', '/index.jsx'],
  tsx: ['.tsx', '.ts', '.d.ts', '.js', '.jsx', '/index.tsx', '/index.ts', '/index.js'],
  jsx: ['.jsx', '.js', '/index.jsx', '/index.js'],
  // SFC consumers import plain TS/JS, sibling components, and barrels
  // (`./lib` → `./lib/index.ts`). Without a list, relative imports from a
  // `.svelte`/`.vue` file resolve to nothing, so barrel callers vanish (#629).
  svelte: ['.ts', '.js', '.svelte', '.tsx', '.jsx', '/index.ts', '/index.js', '/index.svelte'],
  vue: ['.ts', '.js', '.vue', '.tsx', '.jsx', '/index.ts', '/index.js', '/index.vue'],
  astro: ['.ts', '.js', '.astro', '.tsx', '.jsx', '/index.ts', '/index.js', '/index.astro'],
  python: ['.py', '/__init__.py'],
  go: ['.go'],
  rust: ['.rs', '/mod.rs'],
  java: ['.java'],
  c: ['.h', '.c'],
  cpp: ['.h', '.hpp', '.hxx', '.cpp', '.cc', '.cxx'],
  csharp: ['.cs'],
  php: ['.php'],
  ruby: ['.rb'],
  objc: ['.h', '.m', '.mm'],
  nix: ['.nix', '/default.nix'],
};

// Per-context memo for import-specifier → file resolution: pure given a
// stable file set, which is exactly the window between
// ReferenceResolver.clearCaches() calls — clearImportResolverMemos() is
// invoked there, so the staleness discipline matches the resolver's own caches.
const importPathMemos = new WeakMap<ResolutionContext, Map<string, string | null>>();

/** Drop the per-context memo tables (see ReferenceResolver.clearCaches). */
export function clearImportResolverMemos(context: ResolutionContext): void {
  importPathMemos.delete(context);
  cobolCopybookIndexes.delete(context);
}

export function resolveImportPath(
  importPath: string,
  fromFile: string,
  language: Language,
  context: ResolutionContext
): string | null {
  let memo = importPathMemos.get(context);
  if (!memo) {
    memo = new Map();
    importPathMemos.set(context, memo);
  }
  const key = `${language}\0${fromFile}\0${importPath}`;
  const hit = memo.get(key);
  if (hit !== undefined || memo.has(key)) return hit ?? null;
  const resolved = resolveImportPathUncached(importPath, fromFile, language, context);
  memo.set(key, resolved);
  return resolved;
}

function resolveImportPathUncached(
  importPath: string,
  fromFile: string,
  language: Language,
  context: ResolutionContext
): string | null {
  // COBOL COPY/EXEC SQL INCLUDE names a copybook member, not a path — the
  // compiler searches a library, so we match against indexed file basenames.
  // Must run before isExternalImport: a bare member name would otherwise be
  // misclassified as an external package.
  if (language === 'cobol') {
    return resolveCobolCopybook(importPath, fromFile, context);
  }

  // Skip external/npm packages — but pass the context so the
  // bare-specifier heuristic can consult the project's tsconfig
  // alias map first (custom prefixes like `@components/*` would
  // otherwise be misclassified as npm).
  if (isExternalImport(importPath, language, context)) {
    return null;
  }

  const projectRoot = context.getProjectRoot();
  const fromDir = path.dirname(path.join(projectRoot, fromFile));

  // Handle relative imports
  if (importPath.startsWith('.')) {
    return resolveRelativeImport(importPath, fromDir, language, context);
  }

  // Handle absolute/aliased imports (like @/ or src/)
  const aliased = resolveAliasedImport(importPath, projectRoot, language, context);
  if (aliased) return aliased;

  // C/C++ include directory search: when neither relative nor aliased
  // resolution found a match, search -I directories from
  // compile_commands.json or heuristic probing.
  if (language === 'c' || language === 'cpp') {
    return resolveCppIncludePath(importPath, language, context);
  }

  return null;
}

/**
 * COBOL copybook lookup: `COPY CVACT01Y` (or `EXEC SQL INCLUDE X`) names a
 * library member resolved by the compiler's copybook search path, so we match
 * the member against indexed file basenames, case-insensitively. `.cpy` wins
 * over a same-named program; a same-directory hit wins within a tier. The
 * stem index is built once per resolution context (a per-ref scan of every
 * file node would go quadratic on copybook-heavy repos).
 */
const cobolCopybookIndexes = new WeakMap<ResolutionContext, Map<string, string[]>>();

function resolveCobolCopybook(
  member: string,
  fromFile: string,
  context: ResolutionContext
): string | null {
  let index = cobolCopybookIndexes.get(context);
  if (!index) {
    index = new Map();
    for (const fileNode of context.getNodesByKind('file')) {
      const normalized = fileNode.filePath.replace(/\\/g, '/');
      const base = normalized.split('/').pop() ?? '';
      const dot = base.lastIndexOf('.');
      const stem = (dot > 0 ? base.slice(0, dot) : base).toLowerCase();
      const paths = index.get(stem);
      if (paths) paths.push(fileNode.filePath);
      else index.set(stem, [fileNode.filePath]);
    }
    cobolCopybookIndexes.set(context, index);
  }

  const candidates = index.get(member.toLowerCase());
  if (!candidates || candidates.length === 0) return null;

  const fromDir = fromFile.replace(/\\/g, '/').split('/').slice(0, -1).join('/');
  let best: string | null = null;
  let bestScore = -1;
  for (const candidate of candidates) {
    const normalized = candidate.replace(/\\/g, '/');
    const ext = normalized.slice(normalized.lastIndexOf('.')).toLowerCase();
    let score = 0;
    if (ext === '.cpy') score += 4;
    else if (ext === '.cbl' || ext === '.cob' || ext === '.cobol') score += 2;
    if (normalized.split('/').slice(0, -1).join('/') === fromDir) score += 1;
    if (score > bestScore) {
      bestScore = score;
      best = candidate;
    }
  }
  return best;
}

/**
 * C and C++ standard library header names (without delimiters).
 * Used by isExternalImport to filter system includes from resolution.
 */
const C_CPP_STDLIB_HEADERS = new Set([
  // C standard library headers
  'assert.h', 'complex.h', 'ctype.h', 'errno.h', 'fenv.h', 'float.h',
  'inttypes.h', 'iso646.h', 'limits.h', 'locale.h', 'math.h', 'setjmp.h',
  'signal.h', 'stdalign.h', 'stdarg.h', 'stdatomic.h', 'stdbool.h',
  'stddef.h', 'stdint.h', 'stdio.h', 'stdlib.h', 'stdnoreturn.h',
  'string.h', 'tgmath.h', 'threads.h', 'time.h', 'uchar.h', 'wchar.h',
  'wctype.h',
  // C++ C-library wrappers (cname form)
  'cassert', 'ccomplex', 'cctype', 'cerrno', 'cfenv', 'cfloat',
  'cinttypes', 'ciso646', 'climits', 'clocale', 'cmath', 'csetjmp',
  'csignal', 'cstdalign', 'cstdarg', 'cstdbool', 'cstddef', 'cstdint',
  'cstdio', 'cstdlib', 'cstring', 'ctgmath', 'ctime', 'cuchar',
  'cwchar', 'cwctype',
  // C++ STL headers
  'algorithm', 'any', 'array', 'atomic', 'barrier', 'bit', 'bitset',
  'charconv', 'chrono', 'codecvt', 'compare', 'complex', 'concepts',
  'condition_variable', 'coroutine', 'deque', 'exception', 'execution',
  'expected', 'filesystem', 'format', 'forward_list', 'fstream',
  'functional', 'future', 'generator', 'initializer_list', 'iomanip',
  'ios', 'iosfwd', 'iostream', 'istream', 'iterator', 'latch',
  'limits', 'list', 'locale', 'map', 'mdspan', 'memory', 'memory_resource',
  'mutex', 'new', 'numbers', 'numeric', 'optional', 'ostream', 'print',
  'queue', 'random', 'ranges', 'ratio', 'regex', 'scoped_allocator',
  'semaphore', 'set', 'shared_mutex', 'source_location', 'span',
  'spanstream', 'sstream', 'stack', 'stacktrace', 'stdexcept',
  'stdfloat', 'stop_token', 'streambuf', 'string', 'string_view',
  'strstream', 'syncstream', 'system_error', 'thread', 'tuple',
  'type_traits', 'typeindex', 'typeinfo', 'unordered_map',
  'unordered_set', 'utility', 'valarray', 'variant', 'vector',
  'version',
]);

/**
 * Languages whose imports are ES-module specifiers, extracted by
 * `extractJSImports` and therefore classified by the same bare-specifier /
 * alias / workspace rules. Svelte, Vue and Astro belong here: an SFC imports
 * inside its `<script>` block (Astro: the `---` frontmatter) with exactly the
 * same syntax, and leaving them out made `isExternalImport` answer "not
 * external" for every npm specifier in an SFC.
 */
const ESM_IMPORT_LANGUAGES = new Set<Language>([
  'typescript', 'tsx', 'javascript', 'jsx', 'arkts', 'svelte', 'vue', 'astro',
]);

/** Rust path roots that always name a standard-library crate. */
const RUST_STDLIB_ROOTS = new Set(['std', 'core', 'alloc', 'proc_macro']);

/**
 * Check if an import is external (npm package, etc.)
 *
 * `context` is consulted for project-defined path aliases
 * (tsconfig/jsconfig `paths`). Without that check, custom prefixes
 * like `@components/*` would fail the bare-specifier heuristic and
 * be classified as external before alias resolution can run.
 */
function isExternalImport(
  importPath: string,
  language: Language,
  context?: ResolutionContext
): boolean {
  // Relative imports are not external
  if (importPath.startsWith('.')) {
    return false;
  }

  // Workspace-member imports (`@scope/ui`, `@scope/ui/widgets`) are LOCAL to
  // a monorepo even though they look like bare npm specifiers. Consult the
  // workspace map first so they aren't misclassified as external (#629). The
  // map is null for single-package repos, so this is a no-op there.
  const workspaces = context?.getWorkspacePackages?.();
  if (workspaces && resolveWorkspaceImport(importPath, workspaces)) {
    return false;
  }

  // Common external patterns
  if (ESM_IMPORT_LANGUAGES.has(language)) {
    // Node built-ins
    if (['fs', 'path', 'os', 'crypto', 'http', 'https', 'url', 'util', 'events', 'stream', 'child_process', 'buffer'].includes(importPath)) {
      return true;
    }
    // Project-defined alias prefix? Treat as local.
    const aliases = context?.getProjectAliases?.();
    if (aliases) {
      for (const pat of aliases.patterns) {
        if (importPath.startsWith(pat.prefix)) return false;
      }
    }
    // Scoped packages or bare specifiers that don't start with aliases
    if (!importPath.startsWith('@/') && !importPath.startsWith('~/') && !importPath.startsWith('src/')) {
      // Likely an npm package
      return true;
    }
  }

  if (language === 'python') {
    // Standard library modules
    const stdLibs = ['os', 'sys', 'json', 're', 'math', 'datetime', 'collections', 'typing', 'pathlib', 'logging'];
    if (stdLibs.includes(importPath.split('.')[0]!)) {
      return true;
    }
  }

  if (language === 'go') {
    // Relative imports (rare in idiomatic Go but the grammar allows them).
    if (importPath.startsWith('.')) {
      return false;
    }
    // In-module imports look like `<module-path>/sub/pkg` — local to
    // this project. Without the module-path check we'd flag every
    // cross-package call in a Go monorepo as external (issue #388).
    const mod = context?.getGoModule?.();
    if (mod && (importPath === mod.modulePath || importPath.startsWith(mod.modulePath + '/'))) {
      return false;
    }
    // `internal/` packages stay local even when go.mod is missing —
    // preserves the pre-#388 escape hatch for repos without a parsed module path.
    if (importPath.includes('/internal/')) {
      return false;
    }
    // Anything else is the Go standard library or a third-party module.
    return true;
  }

  if (language === 'c' || language === 'cpp') {
    // C/C++ standard library headers — both C-style (<stdio.h>) and
    // C++-style (<cstdio>, <vector>) forms. Checked against the import
    // path (which the extractor strips of <> or "" delimiters).
    if (C_CPP_STDLIB_HEADERS.has(importPath)) return true;
    // C++ headers without .h extension (e.g. "vector", "string")
    const withoutExt = importPath.replace(/\.h$/, '');
    if (C_CPP_STDLIB_HEADERS.has(withoutExt)) return true;
  }

  return false;
}

/**
 * Resolve a relative import
 */
function resolveRelativeImport(
  importPath: string,
  fromDir: string,
  language: Language,
  context: ResolutionContext
): string | null {
  const projectRoot = context.getProjectRoot();
  const extensions = EXTENSION_RESOLUTION[language] || [];

  // Python dotted-relative imports (`from .certs import x`, `from ..pkg.mod
  // import y`): leading dots are PACKAGE levels (1 = current package), and the
  // remainder is a dotted submodule path. `path.resolve(dir, '.certs')` would
  // treat `.certs` as a literal hidden filename, so translate the Python form
  // to a real filesystem-relative path before resolving.
  if (language === 'python' && importPath.startsWith('.')) {
    const dots = importPath.length - importPath.replace(/^\.+/, '').length;
    const up = '../'.repeat(Math.max(0, dots - 1));    // 1 dot = current dir
    const rest = importPath.slice(dots).replace(/\./g, '/'); // 'sub.mod' -> 'sub/mod'
    const pyBase = path.resolve(fromDir, up + rest);
    const pyRel = path.relative(projectRoot, pyBase).replace(/\\/g, '/');
    for (const ext of extensions) {
      if (context.fileExists(pyRel + ext)) return pyRel + ext;
    }
    if (pyRel && context.fileExists(pyRel)) return pyRel;
    return null;
  }

  // Try the path as-is first
  const basePath = path.resolve(fromDir, importPath);
  const relativePath = path.relative(projectRoot, basePath).replace(/\\/g, '/');

  // Try each extension
  for (const ext of extensions) {
    const candidatePath = relativePath + ext;
    if (context.fileExists(candidatePath)) {
      return candidatePath;
    }
  }

  // Try without extension (might already have one)
  if (context.fileExists(relativePath)) {
    return relativePath;
  }

  return findSourceForEmittedSpecifier(relativePath, language, context);
}

/**
 * TypeScript under `moduleResolution: node16 | nodenext | bundler` writes the
 * EMITTED extension in the specifier (`import x from './util.js'` for
 * `util.ts`, `.mjs` for `.mts`, `.cjs` for `.cts`), and the source file with that
 * exact name never exists in the repo. Without this remap the import resolver
 * returned null for every such import, so each imported name fell through to
 * bare-name matching: a method wrapping the same-named helper it imports
 * (`renderDockStyles() { return renderDockStyles() }`) resolved to ITSELF, and
 * any repo-wide same-named symbol could win the cross-module edge.
 */
const EMITTED_TO_SOURCE_EXTENSIONS: ReadonlyArray<readonly [RegExp, readonly string[]]> = [
  [/\.js$/, ['.ts', '.tsx', '.d.ts']],
  [/\.jsx$/, ['.tsx']],
  [/\.mjs$/, ['.mts', '.d.mts']],
  [/\.cjs$/, ['.cts', '.d.cts']],
];

function findSourceForEmittedSpecifier(
  relativePath: string,
  language: Language,
  context: ResolutionContext
): string | null {
  if (!EMITTED_SPECIFIER_LANGUAGES.has(language)) return null;
  for (const [emitted, sources] of EMITTED_TO_SOURCE_EXTENSIONS) {
    if (!emitted.test(relativePath)) continue;
    const stem = relativePath.replace(emitted, '');
    for (const ext of sources) {
      const candidate = stem + ext;
      if (context.fileExists(candidate)) return candidate;
    }
    return null;
  }
  return null;
}

/** Languages whose import specifiers can name the emitted `.js` of a `.ts` source. */
const EMITTED_SPECIFIER_LANGUAGES: ReadonlySet<string> = new Set([
  'typescript', 'tsx', 'javascript', 'jsx', 'vue', 'svelte', 'astro', 'arkts',
]);

/**
 * Resolve an aliased/absolute import.
 *
 * Tries, in order:
 *   1. Project-defined `compilerOptions.paths` (tsconfig/jsconfig).
 *      Each pattern can have multiple replacements; tried in tsconfig
 *      priority order with extension permutations.
 *   2. The legacy hard-coded fallback list (`@/`, `~/`, `src/`, ...)
 *      for projects that have aliases but no tsconfig paths block.
 *   3. Direct path lookup (with extensions).
 */
function resolveAliasedImport(
  importPath: string,
  projectRoot: string,
  language: Language,
  context: ResolutionContext
): string | null {
  const extensions = EXTENSION_RESOLUTION[language] || [];
  const tryWithExt = (basePath: string): string | null => {
    for (const ext of extensions) {
      const candidate = basePath + ext;
      if (context.fileExists(candidate)) return candidate;
    }
    if (context.fileExists(basePath)) return basePath;
    return findSourceForEmittedSpecifier(basePath, language, context);
  };

  // 1. Project tsconfig/jsconfig paths.
  const aliasMap = context.getProjectAliases?.();
  if (aliasMap) {
    const candidates = applyAliases(importPath, aliasMap, projectRoot);
    for (const c of candidates) {
      const hit = tryWithExt(c);
      if (hit) return hit;
    }
  }

  // 1.5 Workspace packages (`@scope/ui/widgets` → `packages/ui/widgets`).
  //     Resolves a monorepo member import to the member's directory; the
  //     extension/index permutations below then find its barrel (#629).
  const workspaces = context.getWorkspacePackages?.();
  if (workspaces) {
    const base = resolveWorkspaceImport(importPath, workspaces);
    if (base) {
      const hit = tryWithExt(base);
      if (hit) return hit;
    }
  }

  // 2. Hard-coded fallback list. Kept for projects that use these
  //    conventional aliases without declaring them in tsconfig.
  const fallbackAliases: Record<string, string> = {
    '@/': 'src/',
    '~/': 'src/',
    '@src/': 'src/',
    'src/': 'src/',
    '@app/': 'app/',
    'app/': 'app/',
  };
  for (const [alias, replacement] of Object.entries(fallbackAliases)) {
    if (importPath.startsWith(alias)) {
      const hit = tryWithExt(importPath.replace(alias, replacement));
      if (hit) return hit;
    }
  }

  // 3. Direct path.
  return tryWithExt(importPath);
}

/**
 * C/C++ include directory cache (keyed by project root).
 * Loaded once per resolver instance, shared across calls.
 */
const cppIncludeDirCache = new Map<string, string[]>();

/**
 * Clear the C/C++ include directory cache (call between indexing runs)
 */
export function clearCppIncludeDirCache(): void {
  cppIncludeDirCache.clear();
}

/**
 * Discover C/C++ include search directories for a project.
 *
 * Strategy:
 * 1. Look for compile_commands.json (Clang compilation database) in the
 *    project root and common build subdirectories. Parse -I and -isystem
 *    flags from compiler commands.
 * 2. If no compilation database is found, probe for common convention
 *    directories (include/, src/, lib/, api/) and top-level directories
 *    containing .h/.hpp files.
 *
 * Returns paths relative to projectRoot.
 */
export function loadCppIncludeDirs(projectRoot: string): string[] {
  const cached = cppIncludeDirCache.get(projectRoot);
  if (cached !== undefined) return cached;

  const dirs = loadCppIncludeDirsFromCompileDB(projectRoot)
    || loadCppIncludeDirsHeuristic(projectRoot);

  cppIncludeDirCache.set(projectRoot, dirs);
  return dirs;
}

/**
 * Try to load include directories from compile_commands.json.
 * Returns null if no compilation database is found (so the heuristic
 * fallback can run). Returns an array (possibly empty) otherwise.
 */
function loadCppIncludeDirsFromCompileDB(projectRoot: string): string[] | null {
  const candidates = [
    path.join(projectRoot, 'compile_commands.json'),
    path.join(projectRoot, 'build', 'compile_commands.json'),
    path.join(projectRoot, 'cmake-build-debug', 'compile_commands.json'),
    path.join(projectRoot, 'cmake-build-release', 'compile_commands.json'),
    path.join(projectRoot, 'out', 'compile_commands.json'),
  ];

  let dbPath: string | undefined;
  for (const c of candidates) {
    try {
      if (fs.existsSync(c)) {
        dbPath = c;
        break;
      }
    } catch {
      // ignore
    }
  }
  if (!dbPath) return null;

  try {
    const content = fs.readFileSync(dbPath, 'utf-8');
    const entries = JSON.parse(content) as Array<{
      directory: string;
      command?: string;
      arguments?: string[];
    }>;
    if (!Array.isArray(entries)) return null;

    const dirSet = new Set<string>();
    for (const entry of entries) {
      const dir = entry.directory || projectRoot;
      const args = entry.arguments || (entry.command ? shlexSplit(entry.command) : []);
      for (let i = 0; i < args.length; i++) {
        const arg = args[i]!;
        let includeDir: string | undefined;
        // -I<dir> (no space)
        if (arg.startsWith('-I') && arg.length > 2) {
          includeDir = arg.substring(2);
        }
        // -isystem <dir> (space-separated)
        else if ((arg === '-isystem' || arg === '-I') && i + 1 < args.length) {
          includeDir = args[i + 1];
          i++; // skip next arg
        }
        if (includeDir) {
          // Normalize: resolve relative to the compilation directory
          const absPath = path.isAbsolute(includeDir)
            ? includeDir
            : path.resolve(dir, includeDir);
          const relPath = path.relative(projectRoot, absPath).replace(/\\/g, '/');
          // Skip system directories and paths outside the project
          // (relative paths starting with .. or absolute paths like
          // /usr/include or C:\usr on Windows)
          if (!relPath.startsWith('..') && relPath.length > 0 && !path.isAbsolute(relPath)) {
            dirSet.add(relPath);
          }
        }
      }
    }
    return Array.from(dirSet);
  } catch {
    return null;
  }
}

/**
 * Minimal shlex-style split for compiler command strings.
 * Handles double-quoted and single-quoted arguments.
 */
function shlexSplit(cmd: string): string[] {
  const result: string[] = [];
  let i = 0;
  while (i < cmd.length) {
    // Skip whitespace
    while (i < cmd.length && /\s/.test(cmd[i]!)) i++;
    if (i >= cmd.length) break;
    const ch = cmd[i]!;
    if (ch === '"') {
      i++;
      let arg = '';
      while (i < cmd.length && cmd[i] !== '"') {
        if (cmd[i] === '\\' && i + 1 < cmd.length) { i++; arg += cmd[i]; }
        else { arg += cmd[i]; }
        i++;
      }
      i++; // closing quote
      result.push(arg);
    } else if (ch === "'") {
      i++;
      let arg = '';
      while (i < cmd.length && cmd[i] !== "'") { arg += cmd[i]; i++; }
      i++; // closing quote
      result.push(arg);
    } else {
      let arg = '';
      while (i < cmd.length && !/\s/.test(cmd[i]!)) { arg += cmd[i]; i++; }
      result.push(arg);
    }
  }
  return result;
}

/**
 * Heuristic include directory discovery when no compile_commands.json exists.
 * Checks common convention directories and scans top-level dirs for headers.
 */
function loadCppIncludeDirsHeuristic(projectRoot: string): string[] {
  const dirs: string[] = [];
  const conventionDirs = ['include', 'src', 'lib', 'api', 'inc'];

  try {
    const entries = fs.readdirSync(projectRoot, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const name = entry.name;
      // Convention directories
      if (conventionDirs.includes(name.toLowerCase())) {
        dirs.push(name);
        continue;
      }
      // Any top-level directory containing .h or .hpp files
      try {
        const subFiles = fs.readdirSync(path.join(projectRoot, name));
        if (subFiles.some(f => /\.(h|hpp|hxx|hh)$/i.test(f))) {
          dirs.push(name);
        }
      } catch {
        // ignore permission errors
      }
    }
  } catch {
    // ignore
  }

  return dirs;
}

/**
 * Resolve a C/C++ include path by searching include directories.
 * Called as a fallback after relative and aliased resolution fail.
 */
function resolveCppIncludePath(
  importPath: string,
  language: Language,
  context: ResolutionContext
): string | null {
  const includeDirs = context.getCppIncludeDirs?.() ?? [];
  const extensions = EXTENSION_RESOLUTION[language] ?? [];

  for (const dir of includeDirs) {
    const normalizedDir = dir.replace(/\\/g, '/');
    for (const ext of extensions) {
      const candidate = normalizedDir + '/' + importPath + ext;
      if (context.fileExists(candidate)) return candidate;
    }
    // Try as-is (already has extension)
    const candidate = normalizedDir + '/' + importPath;
    if (context.fileExists(candidate)) return candidate;
  }

  return null;
}

/**
 * Import mappings from a file's binding rows, or null when the file has no
 * rows at all (its extractor emits none, so the source regexes still apply).
 * Local scope is irrelevant here: the mappings answer "what does this local
 * name import", and a `require` inside a function is still that.
 */
export function importMappingsFromBindings(rows: Binding[]): ImportMapping[] | null {
  if (rows.length === 0) return null;
  const out: ImportMapping[] = [];
  for (const r of rows) {
    if (r.kind !== 'import' || !r.targetSpec) continue;
    const exportedName = r.targetName ?? r.name;
    out.push({
      localName: r.name,
      exportedName,
      source: r.targetSpec,
      isDefault: exportedName === 'default',
      isNamespace: exportedName === '*',
    });
  }
  return out;
}

/**
 * Extract import mappings from a file
 */
export function extractImportMappings(
  _filePath: string,
  content: string,
  language: Language
): ImportMapping[] {
  const mappings: ImportMapping[] = [];

  // Every language answers from the `bindings` table
  // (importMappingsFromBindings); no source regex remains here. Kept for the
  // context hook and its callers; returns nothing.
  void content;
  void language;
  return mappings;
}

const rustCrateRootMemos = new WeakMap<ResolutionContext, Map<string, string | null>>();
const rustRsDirIndexes = new WeakMap<ResolutionContext, Map<string, string[]>>();

/** dir → indexed `.rs` files (sorted), built once per context. */
function rustRsDirIndex(context: ResolutionContext): Map<string, string[]> {
  let idx = rustRsDirIndexes.get(context);
  if (!idx) {
    idx = new Map<string, string[]>();
    for (const f of context.getAllFiles()) {
      const rel = f.replace(/\\/g, '/');
      if (!rel.endsWith('.rs')) continue;
      const dir = path.posix.dirname(rel);
      const list = idx.get(dir);
      if (list) list.push(rel);
      else idx.set(dir, [rel]);
    }
    for (const list of idx.values()) list.sort();
    rustRsDirIndexes.set(context, idx);
  }
  return idx;
}

/**
 * Whether `relFile` contains a `mod <stem>;` declaration (comment-stripped;
 * `pub`/`pub(...)` qualifiers allowed). `mod <stem> {` inline modules end in
 * `{`, not `;`, and do not match.
 */
function rustFileDeclaresMod(
  relFile: string,
  stem: string,
  context: ResolutionContext
): boolean {
  const lines =
    context.getFileLines?.(relFile) ?? context.readFile(relFile)?.split('\n') ?? null;
  if (!lines) return false;
  const re = new RegExp(
    `^\\s*(?:pub\\s*(?:\\([^)]*\\))?\\s+)?mod\\s+${stem.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*;`
  );
  return lines.some((l) => re.test(l.replace(/\/\/.*$/, '').replace(/\/\*.*?\*\//g, '')));
}

/**
 * Fallback crate-root discovery for layouts whose root file is not
 * `lib.rs`/`main.rs` (e.g. Linux kernel modules rooted at
 * `rust_binder_main.rs`). Climbs the `mod` declaration chain: a file's
 * parent module is the `.rs` file declaring `mod <stem>;` — either
 * `<dir>/<dirname>.rs` one level up (2018 nested modules) or any sibling
 * `.rs` in the parent dir (flat roots, `mod.rs`, `lib.rs`). The file with
 * no declarant is the crate root; its directory is the root dir.
 * Declarants must be indexed `.rs` files (graph-scoped, same as
 * `fileExists`'s fast path).
 */
function rustModChainCrateRoot(
  fromFileAbs: string,
  context: ResolutionContext
): string | null {
  const projectRoot = context.getProjectRoot();
  const toRel = (p: string) => path.relative(projectRoot, p).replace(/\\/g, '/');
  let cur = fromFileAbs.replace(/\\/g, '/');
  for (let i = 0; i < 64; i++) {
    const base = path.posix.basename(cur);
    const dir = path.posix.dirname(cur);
    const [parentDir, stem] =
      base === 'mod.rs'
        ? [path.posix.dirname(dir), path.posix.basename(dir)]
        : [dir, base.replace(/\.rs$/, '')];
    // 2018 nested-module declarant: `<up>/<basename(parent)>.rs` owns
    // `<parent>/` as its module dir (e.g. `binder/node.rs` declares
    // `mod wrapper` for `binder/node/wrapper.rs`).
    const nested = path.posix.join(
      path.posix.dirname(parentDir),
      `${path.posix.basename(parentDir)}.rs`
    );
    let declarant: string | null = null;
    if (nested !== cur && rustFileDeclaresMod(toRel(nested), stem, context)) {
      declarant = nested;
    } else {
      const parentRel = toRel(parentDir) || '.';
      for (const cand of rustRsDirIndex(context).get(parentRel) ?? []) {
        const candAbs = path.posix.join(projectRoot, cand);
        if (candAbs !== cur && rustFileDeclaresMod(cand, stem, context)) {
          declarant = candAbs;
          break;
        }
      }
    }
    if (!declarant) return dir;
    cur = declarant;
  }
  return null;
}

/** The crate-root directory (holds `lib.rs`/`main.rs`), walking up from a file. */
function rustCrateRootDir(fromFileAbs: string, context: ResolutionContext): string | null {
  let memo = rustCrateRootMemos.get(context);
  if (!memo) {
    memo = new Map<string, string | null>();
    rustCrateRootMemos.set(context, memo);
  }
  const key = fromFileAbs.replace(/\\/g, '/');
  if (memo.has(key)) return memo.get(key)!;
  const projectRoot = context.getProjectRoot();
  const toRel = (p: string) => path.relative(projectRoot, p).replace(/\\/g, '/');
  let dir = path.dirname(fromFileAbs);
  let found: string | null = null;
  for (let i = 0; i < 64; i++) {
    if (context.fileExists(toRel(path.join(dir, 'lib.rs'))) ||
        context.fileExists(toRel(path.join(dir, 'main.rs')))) {
      found = dir;
      break;
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  const result = found ?? rustModChainCrateRoot(fromFileAbs, context);
  memo.set(key, result);
  return result;
}

/** Directory under which the current file's module declares its SUBMODULES. */
function rustSelfModuleDir(fromFileAbs: string): string {
  const base = path.basename(fromFileAbs);
  const dir = path.dirname(fromFileAbs);
  // mod.rs / lib.rs / main.rs own their directory; `foo.rs`'s submodules live in `foo/`.
  if (base === 'mod.rs' || base === 'lib.rs' || base === 'main.rs') return dir;
  return path.join(dir, base.replace(/\.rs$/, ''));
}

/**
 * Resolve a Rust module path (segments WITHOUT the leaf symbol) to the file of
 * the last module segment — `crate::a::b` → `<crate>/a/b.rs` (or `.../b/mod.rs`).
 * Anchors on `crate` / `self` / `super`; a bare path is tried crate-relative.
 */
function resolveRustModuleFile(
  segments: string[],
  fromFile: string,
  context: ResolutionContext
): string | null {
  if (segments.length === 0) return null;
  const projectRoot = context.getProjectRoot();
  const fromAbs = path.join(projectRoot, fromFile);
  const toRel = (p: string) => path.relative(projectRoot, p).replace(/\\/g, '/');

  // Walk a sequence of module segments down from `startDir`, mapping each to a
  // `<seg>.rs` or `<seg>/mod.rs` file. Returns the leaf module's file, or null
  // if `startDir` is null or any segment has no file on disk.
  const resolveUnder = (startDir: string | null, rest: string[]): string | null => {
    if (!startDir) return null;
    let dir = startDir;
    let targetFile: string | null = null;
    for (const seg of rest) {
      if (seg === 'self' || seg === 'crate' || seg === 'super') continue;
      const asFile = toRel(path.join(dir, seg + '.rs'));
      const asMod = toRel(path.join(dir, seg, 'mod.rs'));
      if (context.fileExists(asFile)) targetFile = asFile;
      else if (context.fileExists(asMod)) targetFile = asMod;
      else return null;
      dir = path.join(dir, seg);
    }
    return targetFile;
  };

  const first = segments[0]!;
  if (first === 'crate') {
    return resolveUnder(rustCrateRootDir(fromAbs, context), segments.slice(1));
  }
  if (first === 'self') {
    return resolveUnder(rustSelfModuleDir(fromAbs), segments.slice(1));
  }
  if (first === 'super') {
    let supers = 0;
    while (segments[supers] === 'super') supers++;
    let dir: string | null = rustSelfModuleDir(fromAbs);
    for (let s = 0; s < supers && dir; s++) dir = path.dirname(dir);
    return resolveUnder(dir, segments.slice(supers));
  }
  // Bare path. In expression position (`submodule::item()` — the router-assembly
  // and general cross-module-call pattern) the prefix is a SUBMODULE of the
  // current module, i.e. 2018 `self::`-relative — so try self-relative FIRST.
  // Fall back to crate-relative for 2015-edition / crate-root items. External
  // crate paths (`serde::de::Error`) miss both and fall through to name-matching.
  return (
    resolveUnder(rustSelfModuleDir(fromAbs), segments) ??
    resolveUnder(rustCrateRootDir(fromAbs, context), segments)
  );
}

/**
 * Rust `use` declarations, flattened to `localName → full path`.
 *
 * Rust is the one supported language with NO `ImportMapping` extraction (see
 * `extractImportMappings`), so this is the only channel that can tell whether
 * a bare type name in a Rust file was brought in by a `use`. Handles nested
 * groups (`use a::{b::C, d as E}`), globs (skipped — they bind no single
 * name), and `as` aliases.
 */
function collectRustUseBindings(content: string): Map<string, string> {
  const out = new Map<string, string>();

  // Expand one level of `{...}` at a time so `a::{b::{C, D}, E}` flattens.
  const expand = (spec: string): string[] => {
    const open = spec.indexOf('{');
    if (open === -1) return [spec.trim()];
    const prefix = spec.slice(0, open);
    let depth = 0;
    let close = -1;
    for (let i = open; i < spec.length; i++) {
      if (spec[i] === '{') depth++;
      else if (spec[i] === '}') {
        depth--;
        if (depth === 0) { close = i; break; }
      }
    }
    if (close === -1) return [];
    const suffix = spec.slice(close + 1);
    const inner = spec.slice(open + 1, close);
    const parts: string[] = [];
    let depth2 = 0;
    let start = 0;
    for (let i = 0; i <= inner.length; i++) {
      const ch = inner[i];
      if (ch === '{') depth2++;
      else if (ch === '}') depth2--;
      if (i === inner.length || (ch === ',' && depth2 === 0)) {
        const seg = inner.slice(start, i).trim();
        if (seg) parts.push(seg);
        start = i + 1;
      }
    }
    return parts.flatMap((p) => expand(prefix + p + suffix));
  };

  // `use` items end at the first `;`. Attributes/visibility (`pub use`) are
  // irrelevant to the binding itself.
  const useRe = /(^|\n)\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);/g;
  let m: RegExpExecArray | null;
  while ((m = useRe.exec(content)) !== null) {
    for (const spec of expand(m[2]!.replace(/\s+/g, ' '))) {
      const aliasMatch = /^(.*?)\s+as\s+([A-Za-z_]\w*)$/.exec(spec);
      const rawPath = (aliasMatch ? aliasMatch[1]! : spec).trim();
      if (!rawPath || rawPath.endsWith('*')) continue;
      const segments = rawPath.split('::').map((s) => s.trim()).filter(Boolean);
      const leaf = segments[segments.length - 1];
      if (!leaf) continue;
      const local = aliasMatch ? aliasMatch[2]! : leaf;
      out.set(local, segments.join('::'));
    }
  }
  return out;
}

/**
 * Is `name`, as used in `ref`'s file, bound by an import whose module lives
 * OUTSIDE the repository?
 *
 * When it is, no in-repo node can be the referent: the symbol is defined in a
 * third-party crate/package, and any same-named local symbol the name-matcher
 * finds is a coincidence. Rust `use std::error::Error;` + `impl Error for
 * MapperError {}` bound to a local `MapperError::Error` variant, and once
 * non-type kinds were filtered out it simply moved to an unrelated local
 * `type Error` alias — restricting kinds alone RELOCATES the false edge
 * instead of removing it, so locality has to be checked too.
 *
 * Answers only when it can be CERTAIN, because a false "yes" deletes a real
 * edge. Two languages qualify, each with an oracle that cannot be wrong:
 *
 *  - **Rust** — the `use` path is rooted at a standard-library crate
 *    (`std`/`core`/`alloc`/`proc_macro`), which by definition ships outside
 *    any repository. Deliberately NOT generalized to "the module path doesn't
 *    resolve to a file": a crate can re-export another workspace crate's
 *    modules (`pub use pupil_core::{ports, domain};`), so `crate::ports::X`
 *    has no `src/ports/` directory to walk yet is entirely in-repo — that
 *    generalization measured 13 real trait implementations deleted.
 *  - **ES modules** — `isExternalImport`, which already accounts for tsconfig
 *    path aliases and monorepo workspace packages.
 *
 * Everything else returns false and resolves exactly as before. JVM and Python
 * imports notably do NOT go through `resolveImportPath` (they have dedicated
 * FQN/module matchers), so there is no trustworthy oracle to consult here.
 */
export function isBoundToOutOfRepoImport(
  ref: UnresolvedRef,
  context: ResolutionContext
): boolean {
  const name = ref.referenceName;
  if (name.includes('::') || name.includes('.')) return false; // qualified refs resolve by path

  if (ref.language === 'rust') {
    const content = context.readFile(ref.filePath);
    if (!content) return false;
    const usePath = collectRustUseBindings(content).get(name);
    if (!usePath) return false;
    const segments = usePath.split('::');
    if (segments.length < 2 || !RUST_STDLIB_ROOTS.has(segments[0]!)) return false;
    // 2015-edition crate-relative paths can shadow a stdlib root with a local
    // module of the same name — if the path walks to a real file, it's local.
    return resolveRustModuleFile(segments.slice(0, -1), ref.filePath, context) === null;
  }

  if (!ESM_IMPORT_LANGUAGES.has(ref.language)) return false;
  for (const imp of context.getImportMappings(ref.filePath, ref.language)) {
    if (imp.localName !== name) continue;
    return isExternalImport(imp.source, ref.language, context);
  }
  return false;
}
