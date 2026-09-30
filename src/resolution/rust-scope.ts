/**
 * A bare Rust name reaches only what is in scope.
 *
 * An enum's variant is in scope bare only through a `use` of it or of its
 * enum's `*`, and never names a TYPE: ripgrep's every `Some(x)` bound to its
 * `EncodingMode::Some` variant, every `Ok(x)` to `ParseResult::Ok`. A prelude
 * name (`Ok`, `Result`, `Box`) is the prelude's unless the file defines it or
 * imports a project item of that name: serde's macro-hygiene tests declare
 * `struct Ok` and `struct Result`, and serde bound its own `Ok(…)` and
 * `Result<…>` to them.
 *
 * The kernel applies the same rule as a candidate filter in its exact-name
 * and fuzzy arms (codegraph-kernel/src/resolve/name_scope.rs), so an in-scope
 * candidate can still win there. This seam gate covers what the kernel does
 * not choose: a framework resolver's hit. Applied to every settled ref
 * (ReferenceResolver.settleKernelRef), like ./swift-type-visibility.
 */
import type { Node } from '../types';
import type { ResolutionContext, ResolvedRef, UnresolvedRef } from './types';
import { stripCommentsForRegex } from './strip-comments';

/** Names the Rust prelude puts in every module; a project item of the same name needs a `use` to shadow one. */
const RUST_PRELUDE = new Set([
  'Ok', 'Err', 'Some', 'None', 'Result', 'Option', 'Box', 'Vec', 'String', 'Default', 'Drop', 'Iterator',
  'IntoIterator', 'From', 'Into', 'Clone', 'Copy', 'Send', 'Sync', 'Sized', 'ToString', 'ToOwned', 'PartialEq',
  'Eq', 'PartialOrd', 'Ord', 'AsRef', 'AsMut', 'Fn', 'FnMut', 'FnOnce', 'Extend', 'drop',
]);

interface RustUses {
  /** Every identifier in the file's project `use` trees — not `std::` / `core::` / `alloc::` ones. */
  names: Set<string>;
  /** `X` of each `use …::X::*` (`super` for `use super::*`). */
  globs: Set<string>;
}
const RUST_USES = new WeakMap<ResolutionContext, Map<string, RustUses>>();

/** Drop the memos (see ReferenceResolver.clearCaches). */
export function clearRustScopeMemos(context: ResolutionContext): void {
  RUST_USES.delete(context);
}

function rustUsesOf(filePath: string, context: ResolutionContext): RustUses {
  let memo = RUST_USES.get(context);
  if (!memo) {
    memo = new Map();
    RUST_USES.set(context, memo);
  }
  const hit = memo.get(filePath);
  if (hit) return hit;
  const uses: RustUses = { names: new Set(), globs: new Set() };
  // Comments first: a doc comment's prose ("…use the Option…") is not a `use`.
  const text = stripCommentsForRegex(context.readFile(filePath) ?? '', 'rust');
  for (const m of text.matchAll(/(?:^|[;{}\s])use\s+([^;]{1,2000});/g)) {
    const tree = m[1]!;
    if (/^\s*(?:::)?(?:std|core|alloc)\b/.test(tree)) continue;
    for (const id of tree.matchAll(/[A-Za-z_]\w*/g)) uses.names.add(id[0]);
    for (const g of tree.matchAll(/(\w+)\s*::\s*(?:\{[^}]*)?\*/g)) uses.globs.add(g[1]!);
  }
  memo.set(filePath, uses);
  return uses;
}

/** The module a Rust file is: `src/glob.rs` → `glob`, `src/walk/mod.rs` → `walk`. */
function rustModuleName(filePath: string): string {
  const parts = filePath.split('/');
  const base = parts[parts.length - 1]!.replace(/\.rs$/, '');
  return base === 'mod' || base === 'lib' || base === 'main' ? parts[parts.length - 2] ?? base : base;
}

/** Does one of the file's globs bring in this candidate's module (or, for `use super::*`, its parent's)? */
function rustGlobCovers(uses: RustUses, candidate: Node, ref: UnresolvedRef): boolean {
  if (uses.globs.has(rustModuleName(candidate.filePath))) return true;
  if (!uses.globs.has('super')) return false;
  const dir = (p: string): string => p.slice(0, p.lastIndexOf('/'));
  // `use super::*` in a child module: the parent's file, or a sibling in the parent's directory.
  return dir(candidate.filePath) === dir(ref.filePath) || dir(candidate.filePath) === dir(dir(ref.filePath));
}

/** Whether a bare Rust name can mean this candidate (see the module comment). */
export function isRustNameInScope(candidate: Node, ref: UnresolvedRef, context: ResolutionContext): boolean {
  const name = ref.referenceName;
  // Bare in the SOURCE: the index keeps `crate::error::Result` by its last
  // segment, and a path is not a prelude lookup.
  const line = context.getFileLines?.(ref.filePath)?.[ref.line - 1] ?? context.readFile(ref.filePath)?.split('\n')[ref.line - 1];
  if (line !== undefined && line.startsWith(name, ref.column) && /::\s*$/.test(line.slice(0, ref.column))) return true;
  if (candidate.kind === 'enum_member') {
    if (ref.referenceKind === 'references') return false;
    const uses = rustUsesOf(ref.filePath, context);
    const cut = candidate.qualifiedName.lastIndexOf('::');
    const owner = cut >= 0 ? candidate.qualifiedName.slice(0, cut).split('::').pop()! : '';
    return (owner !== '' && uses.globs.has(owner)) || (uses.names.has(name) && uses.names.has(owner));
  }
  if (!RUST_PRELUDE.has(name) || candidate.filePath === ref.filePath) return true;
  const uses = rustUsesOf(ref.filePath, context);
  return uses.names.has(name) || rustGlobCovers(uses, candidate, ref);
}

/** The settled result, or null when its target is out of a bare Rust name's scope. */
export function gateRustScope(result: ResolvedRef | null, ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
  if (!result || ref.language !== 'rust' || !/^[A-Za-z_]\w*$/.test(ref.referenceName)) return result;
  const target = context.getNodeById?.(result.targetNodeId);
  return target && !isRustNameInScope(target, ref, context) ? null : result;
}
