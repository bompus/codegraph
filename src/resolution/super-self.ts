/**
 * `super.didMoveToWindow()` inside an override of `didMoveToWindow` calls
 * the PARENT's implementation, never the method making the call. Extraction
 * keeps a `super` call under the bare method name, so a name match finds the
 * enclosing method itself: one expo-camera view had eleven of its overrides
 * "calling" themselves. The parent's method is usually a framework's (UIKit,
 * Android, React); a self-edge is never it. Real recursion keeps its edge:
 * only a call written through `super` / `base` (C#) / `[super …]`
 * (Objective-C) / `parent::` (PHP) / `super().` (Python) is declined.
 *
 * Applied to every settled ref (ReferenceResolver.settleKernelRef), like
 * ./rust-scope, so a kernel verdict and a framework hit obey it alike.
 */
import type { ResolutionContext, ResolvedRef, UnresolvedRef } from './types';

export function gateSuperSelfCall(result: ResolvedRef | null, ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
  if (!result || ref.referenceKind !== 'calls' || result.targetNodeId !== ref.fromNodeId) return result;
  const name = ref.referenceName.slice(Math.max(ref.referenceName.lastIndexOf('.'), ref.referenceName.lastIndexOf(':')) + 1);
  if (!/^[A-Za-z_$][\w$]*$/.test(name)) return result;
  const line = (context.getFileLines?.(ref.filePath) ?? context.readFile(ref.filePath)?.split(/\r?\n/))?.[ref.line - 1];
  if (!line) return result;
  const escaped = name.replace(/\$/g, '\\$');
  const viaSuper = new RegExp(
    String.raw`(?:\b(?:super|base)\s*(?:\(\s*(?:[\w.]+\s*,\s*\w+)?\s*\))?\s*\??\.\s*|\[\s*super\s+|\bparent\s*::\s*)` + escaped + String.raw`\b`,
  );
  return viaSuper.test(line) ? null : result;
}
