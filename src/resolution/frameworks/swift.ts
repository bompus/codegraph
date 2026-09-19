/**
 * Swift Framework Resolver
 *
 * Handles SwiftUI, UIKit, and Vapor (server-side Swift) patterns.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { stripCommentsForRegex } from '../strip-comments';

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
    // Pattern 1: View references (SwiftUI views are PascalCase ending in View)
    if (ref.referenceName.endsWith('View') && /^[A-Z]/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref.referenceName, VIEW_KINDS, VIEW_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, CLASS_KINDS, VIEWMODEL_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, MODEL_KINDS, MODEL_DIRS, context);
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

  extract(filePath, content) {
    if (!filePath.endsWith('.swift')) return { nodes: [], references: [] };
    const nodes: Node[] = [];
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'swift');

    // Extract SwiftUI View structs
    // struct ContentView: View { ... }
    const viewPattern = /struct\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*View/g;

    let match: RegExpExecArray | null;
    while ((match = viewPattern.exec(safe)) !== null) {
      const [, viewName] = match;
      const line = safe.slice(0, match.index).split('\n').length;

      nodes.push({
        id: `view:${filePath}:${viewName}:${line}`,
        kind: 'component',
        name: viewName!,
        qualifiedName: `${filePath}::${viewName}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'swift',
        updatedAt: now,
      });
    }

    // Extract @main App entry point
    const appPattern = /@main\s+struct\s+(\w+)\s*:\s*App/g;

    while ((match = appPattern.exec(safe)) !== null) {
      const [, appName] = match;
      const line = safe.slice(0, match.index).split('\n').length;

      nodes.push({
        id: `app:${filePath}:${appName}:${line}`,
        kind: 'class',
        name: appName!,
        qualifiedName: `${filePath}::${appName}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'swift',
        updatedAt: now,
      });
    }

    return { nodes, references: [] };
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
    // Pattern 1: ViewController references
    if (ref.referenceName.endsWith('ViewController')) {
      const result = resolveByNameAndKind(ref.referenceName, CLASS_KINDS, VC_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, CLASS_KINDS, UIVIEW_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, CLASS_KINDS, CELL_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, PROTOCOL_KINDS, [], context);
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
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'swift');

    // Extract UIViewController subclasses
    const vcPattern = /class\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*UIViewController/g;

    let match: RegExpExecArray | null;
    while ((match = vcPattern.exec(safe)) !== null) {
      const [, vcName] = match;
      const line = safe.slice(0, match.index).split('\n').length;

      nodes.push({
        id: `viewcontroller:${filePath}:${vcName}:${line}`,
        kind: 'class',
        name: vcName!,
        qualifiedName: `${filePath}::${vcName}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'swift',
        updatedAt: now,
      });
    }

    // Extract UIView subclasses
    const viewPattern = /class\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*UIView[^C]/g;

    while ((match = viewPattern.exec(safe)) !== null) {
      const [, viewName] = match;
      const line = safe.slice(0, match.index).split('\n').length;

      nodes.push({
        id: `uiview:${filePath}:${viewName}:${line}`,
        kind: 'class',
        name: viewName!,
        qualifiedName: `${filePath}::${viewName}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'swift',
        updatedAt: now,
      });
    }

    return { nodes, references: [] };
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

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Pattern 1: Controller references
    if (ref.referenceName.endsWith('Controller')) {
      const result = resolveByNameAndKind(ref.referenceName, VAPOR_CONTROLLER_KINDS, VAPOR_CONTROLLER_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, CLASS_KINDS, FLUENT_MODEL_DIRS, context);
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
      const result = resolveByNameAndKind(ref.referenceName, VAPOR_CONTROLLER_KINDS, VAPOR_MIDDLEWARE_DIRS, context);
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

      // Last segment of a dotted handler (self.list / UserController.list -> list)
      const handlerName = handlerExpr!.split('.').pop();
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

const VIEW_KINDS = new Set(['struct', 'component']);
const CLASS_KINDS = new Set(['class']);
const MODEL_KINDS = new Set(['struct', 'class']);
const PROTOCOL_KINDS = new Set(['protocol']);
const VAPOR_CONTROLLER_KINDS = new Set(['class', 'struct']);

/**
 * Resolve a symbol by name using indexed queries instead of scanning all files.
 */
function resolveByNameAndKind(
  name: string,
  kinds: Set<string>,
  preferredDirPatterns: string[],
  context: ResolutionContext,
): string | null {
  const candidates = context.getNodesByName(name);
  if (candidates.length === 0) return null;

  const kindFiltered = candidates.filter((n) => kinds.has(n.kind));
  if (kindFiltered.length === 0) return null;

  // Prefer candidates in framework-conventional directories
  if (preferredDirPatterns.length > 0) {
    const preferred = kindFiltered.filter((n) =>
      preferredDirPatterns.some((d) => n.filePath.includes(d))
    );
    if (preferred.length > 0) return preferred[0]!.id;
  }

  // Fall back to any match
  return kindFiltered[0]!.id;
}
