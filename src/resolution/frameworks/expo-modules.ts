/**
 * Expo Modules framework — close the JS → native flow for Expo SDK packages.
 *
 * Expo Modules use a Swift / Kotlin DSL distinct from the React Native legacy
 * bridge. Each native module is a class extending `Module` whose
 * `definition()` body declares the JS surface via literal `Name(...)`,
 * `Function(...)`, `AsyncFunction(...)`, `Property(...)`, and `View {...}`
 * calls. Tree-sitter parses these as ordinary call_expressions with trailing
 * closures, so the JS-visible methods don't exist as named symbol nodes by
 * default — `Camera.takePictureAsync(...)` on the JS side has nothing to
 * resolve to.
 *
 * This framework extractor walks the file source for those declarative
 * literals and emits method nodes named `takePictureAsync` /
 * `notificationAsync` / `width` / etc., attributed to the Swift / Kotlin
 * file. Calls resolve through the receiver's requireNativeModule binding,
 * which identifies the native module even when several modules share a method.
 *
 * Real-world shape (expo-haptics):
 *
 *   public class HapticsModule: Module {
 *     public func definition() -> ModuleDefinition {
 *       Name("ExpoHaptics")
 *       AsyncFunction("notificationAsync") { ... }
 *       AsyncFunction("impactAsync") { ... }
 *       AsyncFunction("selectionAsync") { ... }
 *     }
 *   }
 *
 * Kotlin Module declarations are the same DSL (the API mirrors Swift).
 *
 * Anti-goals (deferred):
 * - The trailing-closure BODY is not extracted as the method's body — it
 *   remains attributed to `definition()` in the existing extraction. Future
 *   work could synthesize a body-range for richer `trace` output, but the
 *   reachability (which is the bridge's main value) is already complete.
 * - `View { ... }` blocks expose JSX prop bindings; that overlaps with
 *   Fabric (Phase 6) and is left to that phase.
 */
import type { Node } from '../../types';
import { innermostBinding } from '../name-matcher';
import { resolveViaImport } from '../import-resolver';
import { matchBalanced } from '../synth-utils';
import {
  FrameworkExtractionResult,
  FrameworkResolver,
} from '../types';

/**
 * Match `Function("name")`, `AsyncFunction("name")`, or `Property("name")`
 * at the start of an expression (line-anchored after optional whitespace).
 * The trailing closure that follows isn't captured — we just need the name
 * literal that becomes the JS-visible method.
 *
 * NOTE: the regex deliberately requires the open paren to live on the same
 * line as the keyword, which matches every real Expo Module declaration
 * style. Multi-line `AsyncFunction(\n"x"\n)` forms aren't a real shape in
 * the SDK; if any appear we'd extend the regex.
 *
 * The optional `<…>` covers Kotlin's GENERIC-typed declarations
 * (`AsyncFunction<Float>("getBatteryLevelAsync")`, `AsyncFunction<Int, String>(…)`)
 * — without it, every Android Expo Module method was silently dropped, so a JS
 * callsite resolved only to the iOS Swift impl and never the Android one.
 */
const EXPO_DECL_RE =
  /\b(Function|AsyncFunction|Property|Constants)\s*(?:<[^(]*>)?\s*\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']/g;

/**
 * Match the module name literal `Name("ExpoX")`. Used to enrich each emitted
 * method's qualifiedName so the same JS callsite to `Foo.fn` doesn't ambiguate
 * across multiple Expo modules in a monorepo.
 */
const EXPO_MODULE_NAME_RE = /\bName\s*\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']/;

/**
 * Heuristic class-name match — used as a fallback if `Name(...)` literal
 * isn't found. Detects `class XxxModule: Module` (Swift) or
 * `class XxxModule : Module` (Kotlin / with whitespace tolerance).
 */
const EXPO_CLASS_RE =
  /\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*Module\b/;

/**
 * Detect whether a file is plausibly an Expo Module — looking for both
 * the `: Module` inheritance and at least one declarative `Function(...)`
 * / `AsyncFunction(...)` / `Property(...)` / `Name(...)` literal. Any one
 * of those alone produces too many false positives (random Swift code can
 * have `class X: Module` for unrelated reasons).
 */
function isExpoModuleSource(source: string): boolean {
  if (!EXPO_CLASS_RE.test(source)) return false;
  // Reset lastIndex defensively; EXPO_DECL_RE has the `g` flag.
  EXPO_DECL_RE.lastIndex = 0;
  return EXPO_DECL_RE.test(source);
}

/**
 * Extract Expo Module method declarations from a Swift / Kotlin source
 * file. Each `Function("X") { … }` / `AsyncFunction("X") { … }` /
 * `Property("X") { … }` literal becomes a method node named `X`,
 * attributed to the file at the line of the literal.
 */
function extractExpoMethods(filePath: string, source: string, language: 'swift' | 'kotlin'): Node[] {
  if (!isExpoModuleSource(source)) return [];
  const nodes: Node[] = [];

  const nameMatch = source.match(EXPO_MODULE_NAME_RE);
  const classMatch = source.match(EXPO_CLASS_RE);
  // Prefer the explicit `Name("X")` literal — that's the JS-visible
  // module name. Class name is the fallback.
  const moduleName = nameMatch?.[1] ?? classMatch?.[1] ?? 'ExpoModule';

  const now = Date.now();
  const seenAtLine = new Set<string>();
  EXPO_DECL_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = EXPO_DECL_RE.exec(source)) !== null) {
    const kind = m[1]!;
    const methodName = m[2]!;
    // Compute line number from match index.
    const before = source.slice(0, m.index);
    const startLine = before.split('\n').length;
    // Avoid duplicates if the same method literal appears twice in one
    // file (e.g., declared and re-declared inside a `View {...}` block).
    const dedupKey = `${methodName}:${startLine}`;
    if (seenAtLine.has(dedupKey)) continue;
    seenAtLine.add(dedupKey);

    const startColumn = before.length - before.lastIndexOf('\n') - 1;

    // Extend the node's range over the trailing closure — `AsyncFunction("x")
    // { …body… }`, possibly behind a chained modifier like
    // `.runOnQueue(.main)`. Without this the body keeps attributing to the
    // surrounding `definition()`, so enclosing-fn lookup can never place a
    // callsite inside the method it belongs to. Scanning is anchored at this
    // declaration's own `(` and only follows `)` + `.modifier(…)` hops, so an
    // unrelated closure elsewhere in the file is never captured.
    let endIdx = -1;
    const openParen = source.indexOf('(', m.index + kind.length);
    if (openParen !== -1) {
      let cursor = matchBalanced(source, openParen);
      for (let hops = 0; hops < 4 && cursor !== -1; hops++) {
        let i = cursor + 1;
        while (i < source.length && /\s/.test(source[i]!)) i++;
        if (source[i] === '{') {
          endIdx = matchBalanced(source, i);
          break;
        }
        // Chained call: `.name(…)` (optional generics) — hop past it.
        const chain = /^\.\s*[A-Za-z_$][\w$]*\s*(?:<[^(]*>)?\s*\(/.exec(source.slice(i));
        if (!chain) break;
        const close = matchBalanced(source, i + chain[0].length - 1);
        if (close === -1) break;
        cursor = close;
      }
    }
    const endLine = endIdx === -1 ? startLine : source.slice(0, endIdx + 1).split('\n').length;
    const endBefore = endIdx === -1 ? null : source.slice(0, endIdx + 1);
    const endColumn = endBefore
      ? endBefore.length - endBefore.lastIndexOf('\n') - 1
      : startColumn + kind.length + 2 + methodName.length + 2;

    nodes.push({
      id: `expo-module:${filePath}:${moduleName}:${methodName}:${startLine}`,
      kind: 'method',
      name: methodName,
      qualifiedName: `${filePath}::${moduleName}.${methodName}`,
      filePath,
      language,
      startLine,
      endLine,
      startColumn,
      endColumn,
      docstring: `Expo Modules ${kind}("${methodName}") in ${moduleName}`,
      signature: `${kind}("${methodName}")`,
      isExported: true,
      updatedAt: now,
    });
  }

  return nodes;
}

export const expoModulesResolver: FrameworkResolver = {
  name: 'expo-modules',
  languages: ['swift', 'kotlin'],

  /**
   * Detect Expo Modules by looking at the project's package.json or
   * a small scan of source files for the `: Module` + declarative-DSL
   * markers. Either signal suffices.
   */
  detect(context) {
    const pkg = context.readFile('package.json');
    if (pkg && /["']expo-modules-core["']\s*:/.test(pkg)) return true;
    const files = context.getAllFiles();
    for (let i = 0; i < Math.min(files.length, 200); i++) {
      const f = files[i];
      if (!f) continue;
      if (f.endsWith('.swift') || f.endsWith('.kt')) {
        const src = context.readFile(f);
        if (src && isExpoModuleSource(src)) return true;
      }
    }
    return false;
  },

  /**
   * Per-file extraction — the orchestrator invokes this for every
   * `.swift` / `.kt` file in the project. We only emit nodes when the
   * file looks like an Expo Module; otherwise return empty.
   */
  extract(filePath, source): FrameworkExtractionResult {
    const language = filePath.endsWith('.kt') ? 'kotlin' : 'swift';
    return {
      nodes: extractExpoMethods(filePath, source, language),
      references: [],
    };
  },

  resolve(ref, context) {
    if (ref.referenceKind !== 'calls') return null;
    const call = /^([\w$]+)\.([\w$]+)$/.exec(ref.referenceName);
    if (!call) return null;
    const binding = innermostBinding(context.getBindings?.(ref.filePath) ?? [], call[1]!, ref.line);
    const receiverId = binding?.kind === 'import'
      ? resolveViaImport({ ...ref, referenceName: call[1]!, referenceKind: 'references' }, context)?.targetNodeId
      : binding?.nodeId;
    const receiver = receiverId && context.getNodeById?.(receiverId);
    if (!receiver) return null;
    const factory = /^=\s*([\w$]+)\s*(?:<[^>]+>)?\s*\(\s*['"]([\w]+)['"]\s*\)/.exec(receiver.signature ?? '');
    if (!factory) return null;
    const imported = innermostBinding(context.getBindings?.(receiver.filePath) ?? [], factory[1]!, receiver.startLine);
    if (imported?.kind !== 'import' || imported.targetSpec !== 'expo-modules-core' ||
        !['requireNativeModule', 'requireOptionalNativeModule'].includes(imported.targetName ?? '')) return null;
    const target = context.getNodesByName(call[2]!).find(n =>
      n.id.startsWith('expo-module:') && n.qualifiedName.endsWith(`::${factory[2]}.${call[2]}`));
    return target ? { original: ref, targetNodeId: target.id, confidence: 0.9, resolvedBy: 'framework' } : null;
  },
};
