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
  /** `X` of each `use …::X::*` (`super` for `use super::*`), grouped or not. */
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
    for (const g of rustUseGlobs(tree)) uses.globs.add(g);
  }
  memo.set(filePath, uses);
  return uses;
}

/**
 * The module of each `*` in a `use` tree: `X` of `X::*`, also inside a group
 * (`crate::{error::*, util::*}`), and the group's owner for a group's own `*`
 * item (`X::{self, *}`, `X::{b::{c}, *}`). Mirrored in the kernel
 * (name_scope.rs collect_rust_use_globs).
 */
export function rustUseGlobs(tree: string): Set<string> {
  const globs = new Set<string>();
  // The identifier before a `::` that ends at `end`, whitespace allowed.
  const ownerBefore = (end: number): string | null => {
    let i = end;
    while (i > 0 && /\s/.test(tree[i - 1]!)) i--;
    if (i < 2 || tree.slice(i - 2, i) !== '::') return null;
    i -= 2;
    while (i > 0 && /\s/.test(tree[i - 1]!)) i--;
    const stop = i;
    while (i > 0 && /\w/.test(tree[i - 1]!)) i--;
    return i < stop ? tree.slice(i, stop) : null;
  };
  // The owner of each open group, innermost last.
  const groups: Array<string | null> = [];
  for (let i = 0; i < tree.length; i++) {
    const c = tree[i];
    if (c === '{') groups.push(ownerBefore(i));
    else if (c === '}') groups.pop();
    else if (c === '*') {
      const owner = ownerBefore(i) ?? groups[groups.length - 1] ?? null;
      if (owner) globs.add(owner);
    }
  }
  return globs;
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

/** `c` is part of a Rust identifier (a raw-string prefix is not). */
function isRustIdentChar(c: string): boolean {
  return /[A-Za-z0-9_]/.test(c);
}

/**
 * `src` with comments, and the contents of strings and characters, blanked to
 * spaces (newlines kept) — so a brace or a `#[cfg(test)]` inside a string or
 * a comment is not mistaken for code. Offsets and line structure are those of
 * the original.
 */
export function blankRustCode(src: string): string {
  const n = src.length;
  const parts: string[] = [];
  let last = 0;
  // `src[from, to)` as spaces, its newlines kept; the text before it as it is.
  const blank = (from: number, to: number): void => {
    if (to <= from) return;
    parts.push(src.slice(last, from));
    const chunk = src.slice(from, to);
    parts.push(chunk.includes('\n') ? chunk.replace(/[^\n]/g, ' ') : ' '.repeat(to - from));
    last = to;
  };
  const special = /[/"']/g;
  for (let m = special.exec(src); m; m = special.exec(src)) {
    const i = m.index;
    const c = src[i];
    const d = src[i + 1];
    if (c === '/') {
      if (d === '/') {
        const end = src.indexOf('\n', i);
        const to = end < 0 ? n : end;
        blank(i, to);
        special.lastIndex = to;
      } else if (d === '*') {
        let depth = 1;
        let j = i + 2;
        while (j < n && depth > 0) {
          if (src[j] === '/' && src[j + 1] === '*') { depth++; j += 2; }
          else if (src[j] === '*' && src[j + 1] === '/') { depth--; j += 2; }
          else j++;
        }
        blank(i, j);
        special.lastIndex = j;
      }
      continue;
    }
    if (c === '"') {
      // A raw string: `r"…"`, `r#"…"#`, also after a `b` or `c` prefix —
      // the `r` no end of a longer name.
      let k = i - 1;
      while (k >= 0 && src[k] === '#') k--;
      let p = k - 1;
      if (src[p] === 'b' || src[p] === 'c') p--;
      if (src[k] === 'r' && (p < 0 || !isRustIdentChar(src[p]!))) {
        const close = `"${'#'.repeat(i - 1 - k)}`;
        const end = src.indexOf(close, i + 1);
        const to = end < 0 ? n : end;
        blank(i + 1, to);
        special.lastIndex = Math.min(n, to + close.length);
        continue;
      }
      let j = i + 1;
      while (j < n && src[j] !== '"') j += src[j] === '\\' ? 2 : 1;
      blank(i + 1, Math.min(j, n));
      special.lastIndex = Math.min(n, j + 1);
      continue;
    }
    // `'\n'`, `'\u{1F600}'`; `'x'`; a character outside the BMP; else a lifetime or a label.
    if (d === '\\') {
      const end = src.indexOf('\'', i + 3);
      if (end > 0 && end - i <= 12) {
        blank(i + 1, end);
        special.lastIndex = end + 1;
      }
    } else if (src[i + 2] === '\'' && d !== undefined && d !== '\n') {
      blank(i + 1, i + 2);
      special.lastIndex = i + 3;
    } else if (d !== undefined && /[\uD800-\uDBFF]/.test(d) && src[i + 3] === '\'') {
      blank(i + 1, i + 3);
      special.lastIndex = i + 4;
    }
  }
  parts.push(src.slice(last));
  return parts.join('');
}
