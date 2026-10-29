/** Final target and written-name validation for Go framework results. */
import * as path from 'path';
import type { Node } from '../types';
import type { ImportMapping, ResolutionContext, ResolvedRef, UnresolvedRef } from './types';
function lineAt(ref: UnresolvedRef, context: ResolutionContext): string | undefined {
  return context.getFileLines?.(ref.filePath)?.[ref.line - 1] ?? context.readFile(ref.filePath)?.split(/\r?\n/)[ref.line - 1];
}
function goPackageDir(file: string): string { const dir = path.posix.dirname(file.replace(/\\/g, '/')); return dir === '.' ? '' : dir; }
function goImportPackageDir(name: string, file: string, context: ResolutionContext): string | null | undefined {
  const imp = context.getImportMappings(file, 'go').find((m) => m.localName === name);
  return imp ? context.getGoPackageDir?.(imp.source, file) ?? null : undefined;
}
function goPackageTypes(name: string, dir: string, context: ResolutionContext): Node[] {
  return context.getNodesByName(name).filter((n) => n.language === 'go' && GO_TYPE_KINDS.has(n.kind) && goPackageDir(n.filePath) === dir);
}
function preferCallSiteFile(nodes: Node[], file: string): Node[] {
  return [...nodes].sort((a, b) => Number(b.filePath === file) - Number(a.filePath === file));
}
export const GO_TYPE_KINDS: ReadonlySet<string> = new Set(['struct', 'interface', 'type_alias']);

/** The project packages a Go file dot-imports, and whether it dot-imports one from outside the project. */
interface GoDotImports {
  dirs: string[];
  outside: boolean;
  /** The `type` declarations the file's text holds — function-local ones included — read on demand. */
  types?: Set<string>;
}

const GO_DOT_IMPORTS = new WeakMap<ResolutionContext, Map<string, GoDotImports>>();

function goDotImports(filePath: string, context: ResolutionContext): GoDotImports {
  let memo = GO_DOT_IMPORTS.get(context);
  if (!memo) GO_DOT_IMPORTS.set(context, (memo = new Map()));
  let found = memo.get(filePath);
  if (found) return found;
  found = { dirs: [], outside: false };
  for (const line of context.getFileLines?.(filePath) ?? context.readFile(filePath)?.split(/\r?\n/) ?? []) {
    // The imports come before any other declaration.
    if (/^(?:func|type|var|const)\b/.test(line)) break;
    const dot = /^\s*(?:import\s+)?\.\s+"([^"]+)"/.exec(line);
    if (!dot || !context.getGoPackageDir) continue;
    const dir = context.getGoPackageDir(dot[1]!, filePath);
    if (dir) found.dirs.push(dir);
    // A relative or `/internal/` path is the project's own, mapped or not.
    else if (!dot[1]!.startsWith('.') && !dot[1]!.includes('/internal/')) found.outside = true;
  }
  memo.set(filePath, found);
  return found;
}

/**
 * The directory of the package that declares a Go type written bare in
 * `filePath` (`Controller`, `*Manager`), as Go reads the name: the file's own
 * package, else a package of the project the file dot-imports that declares
 * it (harbor's config tests read `Controller` from `. ".../controller/config"`).
 * Null for an exported name that only a package from outside the project the
 * file dot-imports can give it (`. "github.com/onsi/gomega"`): no type of the
 * project's. The file's own package when no package declares the name, as
 * before: a type the index doesn't hold there, like one a function declares
 * (ginkgo's `type CompleteSpecContext interface {…}` inside a test), which
 * hides a dot import's.
 */
function goBareTypeDir(name: string, filePath: string, context: ResolutionContext): string | null {
  const own = goPackageDir(filePath);
  const dots = goDotImports(filePath, context);
  if ((dots.dirs.length === 0 && !dots.outside) || goPackageTypes(name, own, context).length > 0) return own;
  const dir = dots.dirs.find((d) => goPackageTypes(name, d, context).length > 0);
  if (dir !== undefined) return dir;
  if (!dots.outside || !/^[A-Z]\w*$/.test(name)) return own;
  if (!dots.types) {
    dots.types = new Set();
    for (const line of context.getFileLines?.(filePath) ?? context.readFile(filePath)?.split(/\r?\n/) ?? []) {
      for (const m of line.replace(/\/\/.*$/, '').matchAll(/(?:^|[^\w.])type\s+([A-Za-z_]\w*)/g)) dots.types.add(m[1]!);
    }
  }
  return dots.types.has(name) ? own : null;
}

/**
 * What a Go embedding written bare (`*Manager`) names when the file's own
 * package doesn't declare the name: the type of the package the file
 * dot-imports that does (goBareTypeDir), not another package's namesake name
 * matching found as near the file, or nothing for a name only a package from
 * outside the project can give the file. Any other is left as it is.
 */
export function goDotImportedSupertype(result: ResolvedRef, ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
  const name = ref.referenceName;
  if (!/^[A-Za-z_]\w*$/.test(name)) return result;
  const dir = goBareTypeDir(name, ref.filePath, context);
  if (dir === goPackageDir(ref.filePath) || goRefQualification(ref, context).written !== undefined) return result;
  if (dir === null) return null;
  const target = context.getNodeById?.(result.targetNodeId);
  if (target && goPackageDir(target.filePath) === dir) return result;
  return { ...result, targetNodeId: preferCallSiteFile(goPackageTypes(name, dir, context), ref.filePath)[0]!.id };
}

interface GoQualification {
  /** The package name written before the reference's name, as spelled. */
  written?: string;
  /** The file's import that name is. */
  imported?: ImportMapping;
}



export function goRefQualification(ref: UnresolvedRef, context: ResolutionContext): GoQualification {
  if (ref.referenceKind === 'imports') return {};
  const name = ref.referenceName.split('.').pop()!;
  if (!/^[A-Za-z_]\w*$/.test(name)) return {};
  const line = context.getFileLines?.(ref.filePath)?.[ref.line - 1] ?? context.readFile(ref.filePath)?.split(/\r?\n/)[ref.line - 1] ?? '';
  const at = Math.max(0, ref.column);
  // The qualifier right before the name at the reference's column, or the
  // line's only spelling of the name. A variadic `...chunks.Meta` is written
  // through `chunks` too: the ellipsis is no receiver.
  const written = line.startsWith(name, at) ? /(?:^|[^\w.]|\.{3})([A-Za-z_]\w*)\.$/.exec(line.slice(0, at))?.[1]
    : !new RegExp(`(?<![\\w.])${name}\\b`).test(line) ? new RegExp(`(?:^|[^\\w.]|\\.{3})([A-Za-z_]\\w*)\\.${name}\\b`).exec(line)?.[1] : undefined;
  const imported = written ? context.getImportMappings(ref.filePath, 'go').find((m) => m.localName === written) : undefined;
  const found = { written, imported };
  return found;
}

/**
 * The import a Go reference is written through — `context` in
 * `context.Context`, `store` in `store.Manager` — read from its line, since
 * the index keeps only the name. Undefined for a name written bare, or
 * through anything that isn't one of the file's imports.
 */
export function goRefQualifier(ref: UnresolvedRef, context: ResolutionContext): ImportMapping | undefined {
  return goRefQualification(ref, context).imported;
}

/**
 * Whether a Go reference is written through a package that is none of its
 * file's imports as the index knows them: `clientv3` in `clientv3.KV` under an
 * unaliased `import "go.etcd.io/etcd/client/v3"`, a package named neither by
 * its path's last element (`v3`) nor by the name goimports assumes for it
 * (`client`). Which package that is cannot be told from here.
 */
export function isGoUnknownQualified(ref: UnresolvedRef, context: ResolutionContext): boolean {
  const { written, imported } = goRefQualification(ref, context);
  return written !== undefined && imported === undefined;
}

/**
 * What a Go type position — a parameter or result type, a composite
 * literal's type — names: a type, which Go reads from one package. A method
 * or a function is never it; Go reaches those only through a value or a
 * package. Whichever strategy found a declaration of the name, the type is
 * the one of that name in the reference's own package for a bare name, or in
 * the imported project package for `pkg.T`. Without one there, a method or
 * function of the name is nothing the reference means. etcd's
 * `func (ti *treeIndex) KeyIndex(keyi *keyIndex) *keyIndex` linked both
 * `keyIndex` types to the method `treeIndex.keyIndex` beside it,
 * prometheus's `(ec2Client, error)` result to the method the line declares,
 * and its `&config_util.URL{…}` (an outside package) to `Target.URL`. A bare
 * name that found another package's type means its own package's type of
 * that name when there is one: prometheus's `prompb` builds its own
 * `Histogram_CountInt`, not the `write/v2` one.
 */
export function goTypePositionTarget(result: ResolvedRef, ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
  const target = context.getNodeById?.(result.targetNodeId);
  if (!target || target.language !== 'go') return result;
  const isType = GO_TYPE_KINDS.has(target.kind);
  // A composite literal keeps its package in the name (`config_util.URL`);
  // a parameter type leaves it on the line.
  const dot = ref.referenceName.lastIndexOf('.');
  const name = ref.referenceName.slice(dot + 1);
  // The package's directory; null for a package outside the project,
  // undefined for a qualifier that is none of the file's imports as indexed.
  let pkgDir: string | null | undefined;
  let bare = false;
  if (dot >= 0) {
    pkgDir = goImportPackageDir(ref.referenceName.slice(0, dot), ref.filePath, context);
  } else {
    const { written, imported } = goRefQualification(ref, context);
    bare = written === undefined;
    if (bare) {
      pkgDir = goBareTypeDir(name, ref.filePath, context);
      if (pkgDir === null) return null;
    } else if (imported) pkgDir = context.getGoPackageDir?.(imported.source, ref.filePath) ?? null;
  }
  if (isType && (!bare || goPackageDir(target.filePath) === pkgDir)) return result;
  // `''` is a real directory — a module's root package — not a miss.
  const types = pkgDir !== null && pkgDir !== undefined ? goPackageTypes(name, pkgDir, context) : [];
  if (types.length > 0) return { ...result, targetNodeId: preferCallSiteFile(types, ref.filePath)[0]!.id };
  return isType ? result : null;
}

export function isGoBareName(ref: UnresolvedRef, context: ResolutionContext): boolean {
  const name = ref.referenceName;
  if (ref.language !== 'go' || !/^[A-Za-z_]\w*$/.test(name)) return false;
  if (ref.referenceKind === 'calls' && lineAt(ref, context)?.startsWith(name, ref.column)) return true;
  const line = context.getFileLines?.(ref.filePath)?.[ref.line - 1]
    ?? context.readFile(ref.filePath)?.split(/\r?\n/)[ref.line - 1];
  if (line === undefined) return false;
  // A call's column is its expression's: only a conversion's parenthesized
  // type is still bare there.
  if (ref.referenceKind === 'calls') {
    return line[ref.column] === '(' && new RegExp(`^\\(\\s*\\*?\\s*${name}\\s*\\)\\s*\\(`).test(line.slice(ref.column));
  }
  if (line.startsWith(name, ref.column)) {
    let end = ref.column;
    while (end > 0 && /\s/.test(line[end - 1]!)) end--;
    return line[end - 1] !== '.' || (end >= 3 && line.slice(end - 3, end) === '...');
  }
  // A route's handler is recorded at the start of its line: its spelling
  // there, outside the path string.
  const code = line.replace(/"(?:[^"\\]|\\.)*"|`[^`]*`/g, (s) => ' '.repeat(s.length));
  return new RegExp(`(?<![\\w.])${name}\\b`).test(code);
}
