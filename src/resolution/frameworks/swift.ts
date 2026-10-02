/**
 * Swift Framework Resolver
 *
 * Handles SwiftUI, UIKit, and Vapor (server-side Swift) patterns.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { stripCommentsForRegex } from '../strip-comments';
import { pickByNameAndKind } from './name-heuristic';
import { parseSourceTreeSync, type TreeNode } from '../../extraction/parse-tree';

// No extract(): a SwiftUI view is its own struct node, and a UIKit controller
// its class. A one-line `component`/`class` twin per `struct X: View` (and per
// `@main` app, view controller and UIView subclass) carried no edges and
// showed up in search and the symbol view as a dead-end duplicate.
export const swiftUIResolver: FrameworkResolver = {
  name: 'swiftui',
  languages: ['swift'],

  detect(context: ResolutionContext): boolean {
    // Check for SwiftUI imports in Swift files
    const allFiles = context.getAllFiles();
    for (const file of allFiles) {
      if (file.endsWith('.swift')) {
        const content = context.readFile(file);
        if (content && content.includes('import SwiftUI')) {
          return true;
        }
      }
    }

    // Check for Xcode project with SwiftUI
    for (const file of allFiles) {
      if (file.endsWith('.xcodeproj') || file.endsWith('.xcworkspace')) {
        return true;
      }
    }

    return false;
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Swift's conventions, for Swift's refs: an Objective-C `@interface SDDiskCache
    // : NSObject <SDDiskCache>` is no SwiftUI view or model (languages gates only extraction).
    if (ref.language !== 'swift') return null;
    // Pattern 1: View references (SwiftUI views are PascalCase ending in View)
    if (ref.referenceName.endsWith('View') && /^[A-Z]/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref, VIEW_KINDS, VIEW_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.85,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 2: ViewModel/ObservableObject references
    if (ref.referenceName.endsWith('ViewModel') || ref.referenceName.endsWith('Store') || ref.referenceName.endsWith('Manager')) {
      const result = resolveByNameAndKind(ref, CLASS_KINDS, VIEWMODEL_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.85,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Model references
    if (/^[A-Z][a-zA-Z]+$/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref, MODEL_KINDS, MODEL_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.7,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },
};

export const uikitResolver: FrameworkResolver = {
  name: 'uikit',
  languages: ['swift'],

  detect(context: ResolutionContext): boolean {
    const allFiles = context.getAllFiles();
    for (const file of allFiles) {
      if (file.endsWith('.swift')) {
        const content = context.readFile(file);
        if (content && (
          content.includes('import UIKit') ||
          content.includes('UIViewController') ||
          content.includes('UIView')
        )) {
          return true;
        }
      }
    }

    return false;
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Swift's conventions, for Swift's refs: an Objective-C `@interface SDDiskCache
    // : NSObject <SDDiskCache>` is no SwiftUI view or model (languages gates only extraction).
    if (ref.language !== 'swift') return null;
    // Pattern 1: ViewController references
    if (ref.referenceName.endsWith('ViewController')) {
      const result = resolveByNameAndKind(ref, CLASS_KINDS, VC_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.85,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 2: UIView subclass references
    if (ref.referenceName.endsWith('View') && !ref.referenceName.endsWith('ViewController')) {
      const result = resolveByNameAndKind(ref, CLASS_KINDS, UIVIEW_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Cell references
    if (ref.referenceName.endsWith('Cell')) {
      const result = resolveByNameAndKind(ref, CLASS_KINDS, CELL_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.85,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 4: Delegate/DataSource references
    if (ref.referenceName.endsWith('Delegate') || ref.referenceName.endsWith('DataSource')) {
      const result = resolveByNameAndKind(ref, PROTOCOL_KINDS, [], context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },
};

export const vaporResolver: FrameworkResolver = {
  name: 'vapor',
  languages: ['swift'],

  detect(context: ResolutionContext): boolean {
    // Check for Package.swift with Vapor dependency
    const packageSwift = context.readFile('Package.swift');
    if (packageSwift && packageSwift.includes('vapor')) {
      return true;
    }

    // Check for Vapor imports
    const allFiles = context.getAllFiles();
    for (const file of allFiles) {
      if (file.endsWith('.swift')) {
        const content = context.readFile(file);
        if (content && content.includes('import Vapor')) {
          return true;
        }
      }
    }

    return false;
  },

  // A route's handler ref (`SearchController@show`, `@index`) names no declared
  // symbol, so resolveOne's pre-filter would drop it before resolve() runs.
  claimsReference(name: string): boolean {
    return VAPOR_HANDLER.test(name);
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Swift's conventions, for Swift's refs: an Objective-C `@interface SDDiskCache
    // : NSObject <SDDiskCache>` is no SwiftUI view or model (languages gates only extraction).
    if (ref.language !== 'swift') return null;
    // Pattern 0: a route's handler — `use: SearchController.show` arrives as
    // `SearchController@show`, `use: self.index` / `use: index` as `@index`.
    // Resolved on the type the route names, never by the method's name alone:
    // every controller has a `show`. No match, or two, is left unresolved.
    const handler = VAPOR_HANDLER.exec(ref.referenceName);
    if (handler) {
      const target = resolveVaporHandler(handler[1] ?? null, handler[2]!, ref, context);
      return target ? { original: ref, targetNodeId: target, confidence: 0.9, resolvedBy: 'framework' } : null;
    }

    // Pattern 1: Controller references
    if (ref.referenceName.endsWith('Controller')) {
      const result = resolveByNameAndKind(ref, VAPOR_CONTROLLER_KINDS, VAPOR_CONTROLLER_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.85,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 2: Model references (Fluent)
    if (/^[A-Z][a-zA-Z]+$/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref, CLASS_KINDS, FLUENT_MODEL_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.75,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Middleware references
    if (ref.referenceName.endsWith('Middleware')) {
      const result = resolveByNameAndKind(ref, VAPOR_CONTROLLER_KINDS, VAPOR_MIDDLEWARE_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },

  extract(filePath, content) {
    if (!filePath.endsWith('.swift')) return { nodes: [], references: [] };
    const nodes: Node[] = [];
    const references: UnresolvedRef[] = [];
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'swift');

    // Build a group-var → path-prefix map first. Modern Vapor routes live on a
    // grouped builder (`let todos = routes.grouped("todos"); todos.get(use: index)`
    // or `routes.group("todos") { todos in todos.get(use: index) }`), so the path
    // comes from the group, not the call. Roots (app/routes/router) have no prefix.
    const groupPrefix = new Map<string, string>();
    const segJoin = (existing: string, segsStr: string): string => {
      const segs = (segsStr.match(/"([^"]*)"/g) || []).map((s) => s.slice(1, -1));
      return existing + segs.map((s) => '/' + s).join('');
    };
    let gm: RegExpExecArray | null;
    // let X = Y.grouped("a", "b")
    const groupedRegex = /\blet\s+(\w+)\s*=\s*(\w+)\.grouped\s*\(([^)]*)\)/g;
    while ((gm = groupedRegex.exec(safe)) !== null) {
      groupPrefix.set(gm[1]!, segJoin(groupPrefix.get(gm[2]!) ?? '', gm[3]!));
    }
    // Y.group("a") { X in ... }
    const groupClosureRegex = /\b(\w+)\.group\s*\(([^)]*)\)\s*\{\s*(\w+)\s+in/g;
    while ((gm = groupClosureRegex.exec(safe)) !== null) {
      groupPrefix.set(gm[3]!, segJoin(groupPrefix.get(gm[1]!) ?? '', gm[2]!));
    }

    // Vapor: <builder>.METHOD([path segs,] use: handler). Any receiver (app,
    // routes, or a grouped var); path segments optional and may be non-string
    // (`BlogUser.parameter`, `:id`, a path constant) so accept any comma-separated
    // args before `use:` — the label keeps only the string parts. `use:`
    // discriminates a real route from Environment.get("X")/req.parameters.get("X").
    // Each arg repetition must end at a comma, and `,` is outside the char class,
    // so the split is unique and matching stays linear. The earlier
    // `(?:[^,()]+,\s*)*` was ambiguous — the trailing `\s*` and the next
    // iteration's `[^,()]+` could both claim the same spaces — which backtracked
    // exponentially on a long arg list that never reaches `use:`.
    // The tail is `\s*` rather than a lazy `[^,()]*?` on purpose: both are
    // linear, but the lazy form drops the "`use:` is preceded by a comma"
    // requirement and widens the match set — `req.get(foo.use: bar)` would then
    // be indexed as a route (groups `["req","get","foo.","bar"]`) where both
    // this pattern and the original match nothing.
    // Segments may carry one paren level so typed-route-enum args index:
    // `SiteURL.api(.search).pathComponents` (see postExtract) — each arg is
    // `[^,()]+` runs joined by a single `(...)`; `,` stays outside the class
    // so the split before `use:` remains unique and linear. Nested parens
    // (`SiteURL.pkg(.show(x(y)))`) fail the segment → no node, matching the
    // old skip behavior for unresolvable paths.
    const routeRegex = /\b(\w+)\.(get|post|put|patch|delete|head|options)\s*\(\s*((?:(?:[^,()]+(?:\([^()]*\)[^,()]*)*),)*\s*)use:\s*([A-Za-z_][\w.]*)/g;
    // `let todos = TodoController()` — what a `use: todos.index` receiver is.
    const receiverTypes = new Map<string, string>();
    const receiverRegex = /\b(?:let|var)\s+([a-z_]\w*)\s*(?::\s*([A-Z]\w*)\s*)?=\s*([A-Z][\w.]*)\s*\(/g;
    while ((gm = receiverRegex.exec(safe)) !== null) receiverTypes.set(gm[1]!, gm[2] ?? gm[3]!);
    let match: RegExpExecArray | null;
    while ((match = routeRegex.exec(safe)) !== null) {
      const [, receiver, method, segsStr, handlerExpr] = match;
      const line = safe.slice(0, match.index).split('\n').length;
      const upper = method!.toUpperCase();
      const routePath = (groupPrefix.get(receiver!) ?? '') + segJoin('', segsStr!) || '/';

      const routeNode: Node = {
        id: `route:${filePath}:${line}:${upper}:${routePath}`,
        kind: 'route',
        name: `${upper} ${routePath}`,
        qualifiedName: `${filePath}::route:${routePath}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'swift',
        updatedAt: now,
      };
      nodes.push(routeNode);

      const handlerName = vaporHandlerRef(handlerExpr!, receiverTypes);
      if (handlerName) {
        references.push({
          fromNodeId: routeNode.id,
          referenceName: handlerName,
          referenceKind: 'references',
          line,
          column: 0,
          filePath,
          language: 'swift',
        });
      }
    }

    // `routes.on(.POST, "x", use: handler)` names its method as the first argument.
    // Arguments may hold one level of parentheses (`body: .collect(maxSize: "1mb")`);
    // only unlabeled string arguments are path segments.
    const onRegex = /\b(\w+)\.on\s*\(\s*\.([A-Z]+)\s*,\s*((?:(?:[^,()]|\([^()]*\))+,)*\s*)use:\s*([A-Za-z_][\w.]*)/g;
    const pathArgs = (argText: string) => argText.split(',').filter((a) => /^\s*"[^"]*"\s*$/.test(a)).join(',');
    while ((match = onRegex.exec(safe)) !== null) {
      const [, receiver, method, segsStr, handlerExpr] = match;
      const line = safe.slice(0, match.index).split('\n').length;
      const routePath = (groupPrefix.get(receiver!) ?? '') + segJoin('', pathArgs(segsStr!)) || '/';
      const id = `route:${filePath}:${line}:${method}:${routePath}`;
      nodes.push({
        id, kind: 'route', name: `${method} ${routePath}`, qualifiedName: `${filePath}::route:${routePath}`,
        filePath, startLine: line, endLine: line, startColumn: 0, endColumn: match[0].length, language: 'swift', updatedAt: now,
      });
      const handlerName = vaporHandlerRef(handlerExpr!, receiverTypes);
      if (handlerName) references.push({ fromNodeId: id, referenceName: handlerName, referenceKind: 'references', line, column: 0, filePath, language: 'swift' });
    }

    // A trailing closure is the handler. Read actual call nodes so string
    // literals, comments and adjacent declarations cannot invent route calls.
    const tree = parseSourceTreeSync(content, 'swift');
    try {
      for (const call of tree?.rootNode.descendantsOfType('call_expression') ?? []) {
        let statement = call;
        while (statement.parent && /^(try|await)_expression$/.test(statement.parent.type)) statement = statement.parent;
        if (statement.parent?.type !== 'statements' && statement.parent?.type !== 'source_file') continue;
        const callee = call.namedChildren[0];
        const suffix = call.namedChildren.find((n) => n.type === 'call_suffix');
        const closure = suffix?.namedChildren.find((n) => n.type === 'lambda_literal');
        const args = suffix?.namedChildren.find((n) => n.type === 'value_arguments');
        if (!callee || callee.type !== 'navigation_expression' || !closure || !args) continue;
        const verb = callee.namedChildren.at(-1)?.text.replace(/^\./, '').trim();
        if (!verb || !/^(get|post|put|patch|delete|head|options|webSocket|on)$/.test(verb)) continue;
        const builder = vaporRouteBuilder(callee.namedChildren[0]!, groupPrefix);
        if (!builder || builder.receiver === 'client') continue;
        const values = args.namedChildren.filter((n) => n.type === 'value_argument');
        if (values.some((n) => /^use\s*:/.test(n.text))) continue;
        let method = verb === 'webSocket' ? 'WS' : verb.toUpperCase();
        if (verb === 'on') {
          const on = /^\.([A-Z]+)$/.exec(values.shift()?.text.trim() ?? '');
          if (!on) continue;
          method = on[1]!;
        }
        const segments = values.filter((n) => n.namedChildren.length === 1 && n.namedChildren[0]?.type === 'line_string_literal').map((n) => n.text);
        if (segments.some((s) => /^"https?:/.test(s))) continue;
        const routePath = builder.prefix + segJoin('', segments.join(',')) || '/';
        const line = call.startPosition.row + 1;
        const id = `route:${filePath}:${line}:${method}:${routePath}`;
        nodes.push({
          id, kind: 'route', name: `${method} ${routePath}`,
          qualifiedName: `${filePath}::route:${routePath}`, filePath,
          startLine: line, endLine: closure.endPosition.row + 1,
          startColumn: call.startPosition.column, endColumn: closure.endPosition.column,
          language: 'swift', updatedAt: now,
        });
        // Preserve each written call so overload labels and branch sites
        // reach resolution with their original positions.
        const collectCalls = (node: TreeNode): void => {
          if (/^(function|class|protocol)_declaration$/.test(node.type)) return;
          if (node.type === 'call_expression') {
            const head = node.namedChildren[0];
            // A call on a computed receiver needs type inference; never turn
            // `.get()` following `query()` into an unrelated bare `get`.
            const name = head?.text.replace(/[\s?!]/g, '');
            if (name && /^(?:[A-Za-z_]\w*\.)*[A-Za-z_]\w*$/.test(name)) {
              references.push({ fromNodeId: id, referenceName: name, referenceKind: 'calls', line: node.startPosition.row + 1, column: node.startPosition.column, filePath, language: 'swift' });
            }
          }
          for (const child of node.namedChildren) collectCalls(child);
        };
        collectCalls(closure);
      }
    } finally {
      tree?.delete();
    }

    return { nodes, references };
  },

  /**
   * Typed-route enums (SwiftPackageIndex's `SiteURL.x.pathComponents` shape).
   * `app.get(SiteURL.privacy.pathComponents, use: showPrivacy)` carries no
   * string literal, so `extract` labels the route `GET /` — the real path is
   * computed by the enum's `var path` switch. Cross-file, so it runs here:
   *
   *   enum SiteURL { case api(Api); case privacy
   *     var path: String { switch self {
   *       case .api(let a):   return "api" + a.path   // delegation, literal prefix
   *       case .privacy:      return "privacy"         // pure literal
   *       case .author(let a): return a.path           // pure delegation
   *   } } }
   *
   * Resolution is ALL-OR-NOTHING: `SiteURL.api(.search)` → `"api" + Api.search`
   * composes only if every delegation step ends at a literal case (associated
   * enum type from the `case api(Api)` declaration). A dynamic component
   * (`show(author: r)`) resolves to null and the label stays as extract left
   * it — a half-right path is worse than a bare one. `id`/`qualifiedName`
   * are preserved; the original label is recomputed from the same inputs, so
   * a repeat run is a no-op.
   */
  postExtract(context: ResolutionContext): Node[] {
    const files = context.getAllFiles().filter((f) => f.endsWith('.swift'));
    const enums = new Map<string, EnumPathInfo>();
    const routeFiles: string[] = [];
    for (const f of files) {
      const content = context.readFile(f);
      if (!content) continue;
      if (content.includes('enum') && content.includes('var path')) {
        collectEnumPaths(stripCommentsForRegex(content, 'swift'), enums);
      }
      if (content.includes('.pathComponents') && content.includes('use:')) {
        routeFiles.push(f);
      }
    }
    if (enums.size === 0 || routeFiles.length === 0) return [];

    const CALL_RE = /\b(\w+)\.(get|post|put|patch|delete|head|options)\s*\(\s*([A-Z]\w*)\.(\w+)(\([^)]*\))?\.pathComponents\s*,\s*use:/g;
    const updates: Node[] = [];
    for (const file of routeFiles) {
      const content = context.readFile(file)!;
      const safe = stripCommentsForRegex(content, 'swift');
      // Rebuild this file's group-var prefixes (same rules as extract).
      const groupPrefix = swiftGroupPrefixes(safe);
      const routesAtLine = new Map<number, Node[]>();
      for (const n of context.getNodesInFile(file)) {
        if (n.kind === 'route') {
          let arr = routesAtLine.get(n.startLine);
          if (!arr) { arr = []; routesAtLine.set(n.startLine, arr); }
          arr.push(n);
        }
      }
      CALL_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = CALL_RE.exec(safe)) !== null) {
        const [, receiver, method, enumName, caseName, innerParens] = m;
        const inner = innerParens ? innerParens.slice(1, -1).trim() : null;
        const resolved = enumPathSegment(enums, enumName!, caseName!, inner, 0);
        if (resolved === null) continue;
        const full = `${groupPrefix.get(receiver!) ?? ''}${resolved.startsWith('/') ? resolved : `/${resolved}`}`;
        const line = safe.slice(0, m.index).split('\n').length;
        const upper = method!.toUpperCase();
        for (const route of routesAtLine.get(line) ?? []) {
          const newName = `${upper} ${full}`;
          if (route.name !== newName && route.name.startsWith(`${upper} `)) {
            updates.push({ ...route, name: newName, updatedAt: Date.now() });
          }
        }
      }
    }
    return updates;
  },
};

/** A simple builder or chained `grouped` registration, keeping literal prefixes. */
function vaporRouteBuilder(node: TreeNode, prefixes: ReadonlyMap<string, string>): { receiver: string; prefix: string } | null {
  if (node.type === 'simple_identifier') return { receiver: node.text, prefix: prefixes.get(node.text) ?? '' };
  if (node.type !== 'call_expression') return null;
  const callee = node.namedChildren[0];
  if (callee?.type !== 'navigation_expression' || callee.namedChildren.at(-1)?.text !== '.grouped') return null;
  const base = vaporRouteBuilder(callee.namedChildren[0]!, prefixes);
  if (!base) return null;
  const args = node.namedChildren.find((n) => n.type === 'call_suffix')?.namedChildren.find((n) => n.type === 'value_arguments');
  const segments = args?.namedChildren.filter((n) => n.type === 'value_argument' && n.namedChildren.length === 1 && n.namedChildren[0]?.type === 'line_string_literal') ?? [];
  return { receiver: base.receiver, prefix: base.prefix + segments.map((n) => '/' + n.text.slice(1, -1)).join('') };
}

/** Per-enum case maps for `var path` switch resolution (see postExtract). */
interface EnumPathInfo {
  /** `case .c: return "lit"` — case → literal fragment. */
  lit: Map<string, string>;
  /** `case .c(let v): return "pfx" + v.path` — case → literal prefix; the
   *  inner enum type comes from `assoc` (delegation variable name ignored —
   *  the `case c(Inner)` declaration is the source of truth). */
  dele: Map<string, string>;
  /** `case .c(let v): return v.path` — case → `''` (pure delegation). */
  assoc: Map<string, string>;
}

/** `let X = Y.grouped("a", "b")` / `Y.group("a") { X in }` — the same
 *  group-var → prefix map `extract` builds (shared so postExtract's labels
 *  match grouped routes). */
function swiftGroupPrefixes(safe: string): Map<string, string> {
  const groupPrefix = new Map<string, string>();
  const segJoin = (existing: string, segsStr: string): string => {
    const segs = (segsStr.match(/"([^"]*)"/g) || []).map((s) => s.slice(1, -1));
    return existing + segs.map((s) => '/' + s).join('');
  };
  let gm: RegExpExecArray | null;
  const groupedRegex = /\blet\s+(\w+)\s*=\s*(\w+)\.grouped\s*\(([^)]*)\)/g;
  while ((gm = groupedRegex.exec(safe)) !== null) {
    groupPrefix.set(gm[1]!, segJoin(groupPrefix.get(gm[2]!) ?? '', gm[3]!));
  }
  const groupClosureRegex = /\b(\w+)\.group\s*\(([^)]*)\)\s*\{\s*(\w+)\s+in/g;
  while ((gm = groupClosureRegex.exec(safe)) !== null) {
    groupPrefix.set(gm[3]!, segJoin(groupPrefix.get(gm[1]!) ?? '', gm[2]!));
  }
  return groupPrefix;
}

/** Index of the `}` matching the `{` at `open` (Swift `"` strings), -1 if unbalanced. */
function matchingBrace(s: string, open: number): number {
  let depth = 0;
  let inStr = false;
  for (let i = open; i < s.length; i++) {
    const c = s[i]!;
    if (inStr) {
      if (c === '\\') i++;
      else if (c === '"') inStr = false;
      continue;
    }
    if (c === '"') inStr = true;
    else if (c === '{') depth++;
    else if (c === '}' && --depth === 0) return i;
  }
  return -1;
}

/**
 * Scan `enum <Name> { … }` bodies for `var path: String { switch self { … } }`
 * and record each case's literal fragment / delegation prefix / associated
 * enum type (from the `case c(Inner)` declaration).
 */
function collectEnumPaths(safe: string, out: Map<string, EnumPathInfo>): void {
  const ENUM_RE = /\benum\s+([A-Z]\w*)[^;{]*\{/g;
  let em: RegExpExecArray | null;
  while ((em = ENUM_RE.exec(safe)) !== null) {
    const enumName = em[1]!;
    const open = em.index + em[0].length - 1;
    const close = matchingBrace(safe, open);
    if (close < 0) continue;
    const body = safe.slice(open + 1, close);

    // `var path: String { switch self { … } }` — locate its body.
    const pathM = /\bvar\s+path\s*:\s*String\s*\{/.exec(body);
    if (!pathM) continue;
    const pathOpen = pathM.index + pathM[0].length - 1;
    const pathClose = matchingBrace(body, pathOpen);
    if (pathClose < 0) continue;
    const pathBody = body.slice(pathOpen + 1, pathClose);

    const lit = new Map<string, string>();
    const dele = new Map<string, string>();
    const CASE_RET = /\bcase\s+\.(\w+)(?:\([^)]*\))?\s*:\s*return\s+([^\n]+)/g;
    let cm: RegExpExecArray | null;
    while ((cm = CASE_RET.exec(pathBody)) !== null) {
      const ret = cm[2]!.trim();
      const litM = /^"([^"]*)"$/.exec(ret);
      const delPrefix = /^"([^"]*)"\s*\+\s*\w+\.path$/.exec(ret);
      const pureDel = /^(\w+)\.path$/.exec(ret);
      if (litM) lit.set(cm[1]!, litM[1]!);
      else if (delPrefix) dele.set(cm[1]!, delPrefix[1]!);
      else if (pureDel) dele.set(cm[1]!, '');
    }
    if (lit.size === 0 && dele.size === 0) continue;

    // Case declarations → associated enum type: `case api(Api)` (bare-type
    // assoc only — `case show(author: String)` can't statically resolve).
    const assoc = new Map<string, string>();
    const DECL_RE = /\bcase\s+(\w+)\s*(?:\(\s*([A-Z]\w*)\s*\))?/g;
    let dm: RegExpExecArray | null;
    while ((dm = DECL_RE.exec(body)) !== null) {
      if (dm[2]) assoc.set(dm[1]!, dm[2]!);
    }
    out.set(enumName, { lit, dele, assoc });
  }
}

/**
 * Resolve `Enum.case(inner?)` to a full path fragment, or null when any step
 * is dynamic/unknown (bounded recursion; delegation depth ≤4).
 * `inner` is the text inside `(...)` — `.search`, `.show(...)`, or null.
 */
function enumPathSegment(
  enums: Map<string, EnumPathInfo>,
  enumName: string,
  caseName: string,
  inner: string | null,
  depth: number,
): string | null {
  if (depth > 4) return null;
  const info = enums.get(enumName);
  if (!info) return null;
  if (inner === null) {
    return info.lit.get(caseName) ?? null; // bare `SiteURL.privacy` — literal only
  }
  // `Enum.case(.inner)` — must be a delegation case; the inner enum type is
  // the case's associated type, and `inner` names ITS case (`.search`,
  // `.show(…)` — args with their own parens already failed upstream).
  const prefix = info.dele.get(caseName);
  const assoc = info.assoc.get(caseName);
  if (prefix === undefined || !assoc) return null;
  const innerM = /^\.?(\w+)(?:\(\s*\))?$/.exec(inner) || /^\.?(\w+)\.(\w+)(?:\(\s*\))?$/.exec(inner);
  if (!innerM) return null;
  const innerCase = innerM[2] ?? innerM[1]!;
  const sub = enumPathSegment(enums, assoc, innerCase, null, depth + 1);
  if (sub === null) return null;
  return prefix + sub;
}

// Directory patterns
const VIEW_DIRS = ['/Views/', '/View/', '/Screens/', '/Components/', '/UI/'];
const VIEWMODEL_DIRS = ['/ViewModels/', '/ViewModel/', '/Stores/', '/Managers/', '/Services/'];
const MODEL_DIRS = ['/Models/', '/Model/', '/Entities/', '/Domain/'];
const VC_DIRS = ['/ViewControllers/', '/ViewController/', '/Controllers/', '/Screens/'];
const UIVIEW_DIRS = ['/Views/', '/View/', '/UI/', '/Components/'];
const CELL_DIRS = ['/Cells/', '/Cell/', '/Views/', '/TableViewCells/', '/CollectionViewCells/'];
const VAPOR_CONTROLLER_DIRS = ['/Controllers/', '/Controller/', '/Routes/'];
const FLUENT_MODEL_DIRS = ['/Models/', '/Model/', '/Entities/', '/Database/'];
const VAPOR_MIDDLEWARE_DIRS = ['/Middleware/', '/Middlewares/'];

/** A Vapor route's handler ref: `Type@method` (the type may be dotted), or `@method`. */
const VAPOR_HANDLER = /^((?:[A-Za-z_]\w*\.)*[A-Za-z_]\w*)?@([A-Za-z_]\w*)$/;

/**
 * The handler ref for `use: <expr>`, keeping the type the method is on:
 * `API.PackageController.get` → `API.PackageController@get`; `self.index` and
 * a bare `index` → `@index` (the type the route is written in); `todos.index`
 * → the type `todos` was made from, when the file says.
 */
function vaporHandlerRef(expr: string, receiverTypes: ReadonlyMap<string, string>): string | null {
  const segs = expr.split('.').filter((s) => s.length > 0);
  const method = segs.pop();
  if (!method) return null;
  if (segs[0] === 'self' || segs[0] === 'Self') segs.shift();
  if (segs.length === 0) return `@${method}`;
  if (/^[A-Z]/.test(segs[0]!)) return `${segs.join('.')}@${method}`;
  const type = segs.length === 1 ? receiverTypes.get(segs[0]!) : undefined;
  return type ? `${type}@${method}` : `@${method}`;
}

/**
 * One type's method among `candidates`, or null when they belong to different
 * types. Of a type's overloads, the one declared to take a `Request` — what
 * Vapor calls a handler with — else the earliest.
 */
function oneOwnersMethod(candidates: Node[], ownerOf: (n: Node) => string, context: ResolutionContext): string | null {
  if (candidates.length === 0) return null;
  if (new Set(candidates.map(ownerOf)).size > 1) return null;
  const earliest = (nodes: Node[]) => nodes.reduce((a, b) => (a.startLine <= b.startLine ? a : b)).id;
  if (candidates.length === 1) return candidates[0]!.id;
  const takesRequest = candidates.filter((n) => {
    const lines = context.readFile(n.filePath)?.split(/\r?\n/) ?? [];
    const head = lines.slice(n.startLine - 1, Math.min(n.endLine, n.startLine + 2)).join(' ');
    return /\(\s*(?:\w+\s+)?\w+\s*:\s*Request\b/.test(head);
  });
  return earliest(takesRequest.length > 0 ? takesRequest : candidates);
}

function resolveVaporHandler(typePath: string | null, method: string, ref: UnresolvedRef, context: ResolutionContext): string | null {
  const callables = context
    .getNodesByName(method)
    .filter((n) => (n.kind === 'method' || n.kind === 'function') && n.language === 'swift');
  if (callables.length === 0) return null;
  const ownerOf = (n: Node): string => n.qualifiedName.slice(0, Math.max(0, n.qualifiedName.length - method.length - 2));

  if (typePath === null) {
    // `use: self.index` / `use: index`: the type whose body holds the route —
    // its own method in this file, else one declared in another extension of it.
    const owner = context
      .getNodesInFile(ref.filePath)
      .filter((n) => SWIFT_TYPE_KINDS.has(n.kind) && n.startLine <= ref.line && n.endLine >= ref.line)
      .reduce<Node | null>((inner, n) => (!inner || n.startLine >= inner.startLine ? n : inner), null);
    if (!owner) {
      // A route in a top-level `func routes(_ app:)` names a function in scope.
      const functions = callables.filter((n) => n.kind === 'function');
      const sameFile = functions.filter((n) => n.filePath === ref.filePath);
      if (sameFile.length > 0) return oneOwnersMethod(sameFile, ownerOf, context);
      return functions.length === 1 ? functions[0]!.id : null;
    }
    const own = callables.filter(
      (n) => n.filePath === ref.filePath && n.startLine >= owner.startLine && n.endLine <= owner.endLine
    );
    if (own.length > 0) return oneOwnersMethod(own, ownerOf, context);
    typePath = owner.qualifiedName.replace(/::/g, '.');
  }

  const segs = typePath.split('.');
  const type = segs[segs.length - 1]!;
  const full = segs.join('::');
  const exact = callables.filter((n) => ownerOf(n) === full);
  if (exact.length > 0) return oneOwnersMethod(exact, ownerOf, context);
  const nested = callables.filter((n) => ownerOf(n).endsWith(`::${full}`));
  if (nested.length > 0) return oneOwnersMethod(nested, ownerOf, context);
  if (segs.length > 1) {
    // `extension API.PackageController { static func get }` names its node by
    // the last segment (`PackageController::get`) — the same QN a top-level
    // `PackageController`'s method has, so the extension's own line decides.
    const declared = new RegExp(String.raw`\bextension\s+${segs.join(String.raw`\s*\.\s*`)}\b`);
    const inExtension = callables.filter((n) => {
      if (ownerOf(n) !== type) return false;
      const owner = context
        .getNodesInFile(n.filePath)
        .filter((o) => SWIFT_TYPE_KINDS.has(o.kind) && o.name === type && o.startLine <= n.startLine && o.endLine >= n.endLine)
        .reduce<Node | null>((inner, o) => (!inner || o.startLine >= inner.startLine ? o : inner), null);
      if (!owner) return false;
      // The node starts at its attributes (`@available(…)`), the keyword a line or two on.
      const head = (context.readFile(n.filePath)?.split(/\r?\n/) ?? []).slice(owner.startLine - 1, owner.startLine + 2).join(' ');
      return declared.test(head);
    });
    return oneOwnersMethod(inExtension, ownerOf, context);
  }
  // A type named without the namespace it is nested in — only when one type fits.
  return oneOwnersMethod(callables.filter((n) => ownerOf(n).endsWith(`::${type}`)), ownerOf, context);
}

const SWIFT_TYPE_KINDS = new Set(['class', 'struct', 'enum', 'protocol']);

const VIEW_KINDS = new Set(['struct']);
const CLASS_KINDS = new Set(['class']);
const MODEL_KINDS = new Set(['struct', 'class']);
const PROTOCOL_KINDS = new Set(['protocol']);
const VAPOR_CONTROLLER_KINDS = new Set(['class', 'struct']);

/** A framework name heuristic's pick (see name-heuristic.ts), preferring these folders. */
function resolveByNameAndKind(
  ref: UnresolvedRef,
  kinds: Set<string>,
  preferredDirPatterns: string[],
  context: ResolutionContext,
): string | null {
  return pickByNameAndKind(ref, kinds, (f) => preferredDirPatterns.some((d) => f.includes(d)), context);
}
