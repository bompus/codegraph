/**
 * Rust Framework Resolver
 *
 * Handles Actix-web, Rocket, Axum, and common Rust patterns.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { stripCommentsForRegex } from '../strip-comments';
import { getCargoWorkspaceCrateMap } from './cargo-workspace';

const cargoWorkspaceMapCache = new WeakMap<ResolutionContext, Map<string, string>>();

function getCachedCargoWorkspaceCrateMap(context: ResolutionContext): Map<string, string> {
  const cached = cargoWorkspaceMapCache.get(context);
  if (cached) return cached;
  const map = getCargoWorkspaceCrateMap(context);
  cargoWorkspaceMapCache.set(context, map);
  return map;
}

export const rustResolver: FrameworkResolver = {
  name: 'rust',
  languages: ['rust'],

  detect(context: ResolutionContext): boolean {
    // Check for Cargo.toml (Rust project signature)
    return context.fileExists('Cargo.toml');
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Pattern 1: Handler references
    if (ref.referenceName.endsWith('_handler') || ref.referenceName.startsWith('handle_')) {
      const result = resolveByNameAndKind(ref.referenceName, FUNCTION_KINDS, HANDLER_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 2: Service/Repository trait implementations
    if (ref.referenceName.endsWith('Service') || ref.referenceName.endsWith('Repository')) {
      const result = resolveByNameAndKind(ref.referenceName, SERVICE_KINDS, SERVICE_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Struct references (PascalCase)
    if (/^[A-Z][a-zA-Z]+$/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref.referenceName, STRUCT_KINDS, MODEL_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.7,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 4: Module references
    if (/^[a-z_]+$/.test(ref.referenceName)) {
      const result = resolveModule(ref.referenceName, context);
      if (result) {
        // Workspace-manifest hits are an exact crate-name -> crate-root
        // mapping straight from Cargo.toml, so we trust them above
        // name-matcher self-file matches (which otherwise win at 0.7
        // because every file containing `use foo::...` has its own
        // import node named `foo`).
        return {
          original: ref,
          targetNodeId: result.targetId,
          confidence: result.fromWorkspace ? 0.95 : 0.6,
          resolvedBy: 'framework',
        };
      }
    }

    return null;
  },

  extract(filePath, content) {
    if (!filePath.endsWith('.rs')) return { nodes: [], references: [] };
    const nodes: Node[] = [];
    const references: UnresolvedRef[] = [];
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'rust');

    // Actix-web / Rocket attribute: #[get("/path")] fn handler(..)
    // Capture the method, path, and the fn identifier that follows.
    const attrRegex = /#\[(get|post|put|patch|delete|head|options)\s*\(\s*["']([^"']+)["'][^\]]*\)\]/g;
    let match: RegExpExecArray | null;
    while ((match = attrRegex.exec(safe)) !== null) {
      const [, method, routePath] = match;
      const line = safe.slice(0, match.index).split('\n').length;
      const upper = method!.toUpperCase();

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
        language: 'rust',
        updatedAt: now,
      };
      nodes.push(routeNode);

      const tail = safe.slice(match.index + match[0].length);
      const fnMatch = tail.match(/\n\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/);
      if (fnMatch) {
        references.push({
          fromNodeId: routeNode.id,
          referenceName: fnMatch[1]!,
          referenceKind: 'references',
          line,
          column: 0,
          filePath,
          language: 'rust',
        });
      }
    }

    // Axum: .route("/path", get(h1).post(h2)…) — balanced-paren scan the route
    // call, then emit one route node per chained method. Handlers may be
    // namespaced (`get(module::handler)`, `get(self::list)`); take the last
    // path segment so the ref names the fn, not the module.
    const routeOpenRegex = /\.route\s*\(/g;
    while ((match = routeOpenRegex.exec(safe)) !== null) {
      const openIdx = safe.indexOf('(', match.index);
      if (openIdx < 0) continue;
      const closeIdx = findMatchingParen(safe, openIdx);
      if (closeIdx < 0) continue;

      const args = safe.slice(openIdx + 1, closeIdx);
      const pathMatch = args.match(/^\s*"([^"]+)"\s*,/);
      if (!pathMatch) continue;
      const routePath = pathMatch[1]!;
      const line = safe.slice(0, match.index).split('\n').length;

      const methodBody = args.slice(pathMatch[0].length);
      const methodHandlerRegex = /\b(get|post|put|patch|delete|head|options|trace)\s*\(\s*([A-Za-z_][\w:]*)/g;
      let mh: RegExpExecArray | null;
      while ((mh = methodHandlerRegex.exec(methodBody)) !== null) {
        const upper = mh[1]!.toUpperCase();
        const handler = mh[2]!.split('::').filter(Boolean).pop();
        if (!handler) continue;

        const routeNode: Node = {
          id: `route:${filePath}:${line}:${upper}:${routePath}`,
          kind: 'route',
          name: `${upper} ${routePath}`,
          qualifiedName: `${filePath}::route:${routePath}`,
          filePath,
          startLine: line,
          endLine: line,
          startColumn: 0,
          endColumn: 0,
          language: 'rust',
          updatedAt: now,
        };
        nodes.push(routeNode);

        references.push({
          fromNodeId: routeNode.id,
          referenceName: handler,
          referenceKind: 'references',
          line,
          column: 0,
          filePath,
          language: 'rust',
        });
      }
    }

    // Actix-web `web::scope("/api")` — every `.service(...)` / `.route(...)`
    // arg in its method chain serves under the scope prefix, so a nested
    // `web::resource("/x")` is really `/api/x`. Each chain element's arg list
    // is a span; a path literal inside the innermost enclosing span gains its
    // (composed) prefix. Nested scopes compose (`web::scope("/a").service(
    // web::scope("/b")…)` → `/a/b/…`); a `service(handler_fn)` arg carries no
    // path literal, so nothing else is prefixed.
    interface ScopeSpan { start: number; end: number; prefix: string; }
    const scopeSpans: ScopeSpan[] = [];
    const scopedPath = (pos: number, lit: string): string => {
      let best: ScopeSpan | null = null;
      for (const sp of scopeSpans) {
        if (pos >= sp.start && pos < sp.end && (!best || sp.start > best.start)) best = sp;
      }
      return best ? joinRustPath(best.prefix, lit) : lit;
    };
    const scopeRe = /web::scope\s*\(\s*"([^"]*)"\s*\)/g;
    while ((match = scopeRe.exec(safe)) !== null) {
      // A scope nested inside an outer scope's service arg composes prefixes.
      const prefix = scopedPath(match.index, match[1]!);
      let p = match.index + match[0].length;
      for (;;) {
        const chain = /^\s*\.\s*(\w+)\s*\(/.exec(safe.slice(p));
        if (!chain) break;
        const open = p + chain[0].length - 1;
        const close = findMatchingParen(safe, open);
        if (close < 0) break;
        if (chain[1] === 'service' || chain[1] === 'route') {
          scopeSpans.push({ start: p + chain[0].indexOf('.'), end: close, prefix });
        }
        p = close + 1;
      }
    }

    // Actix-web builder API (the dominant actix routing style; attribute macros
    // are handled above). The handler lives in `.to(handler)`, not `get(handler)`.
    const pushActixRoute = (routePath: string, method: string, handlerExpr: string, line: number) => {
      const handler = handlerExpr.split('::').filter(Boolean).pop();
      if (!handler) return;
      const upper = method.toUpperCase();
      const routeNode: Node = {
        id: `route:${filePath}:${line}:${upper}:${routePath}`,
        kind: 'route',
        name: `${upper} ${routePath}`,
        qualifiedName: `${filePath}::route:${routePath}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: 0,
        language: 'rust',
        updatedAt: now,
      };
      nodes.push(routeNode);
      references.push({
        fromNodeId: routeNode.id,
        referenceName: handler,
        referenceKind: 'references',
        line,
        column: 0,
        filePath,
        language: 'rust',
      });
    };

    // web::resource("/path") { .route(web::METHOD().to(h)) | .to(h) } — possibly
    // chained. The chain is the resource's OWN `.method(...)` elements, walked
    // element-by-element and bounded by the enclosing arg list's ')' — the old
    // fixed 500-char window bled into a scope's LATER `.route(...)` and
    // mislabeled it as this resource's path.
    const resourceRegex = /web::resource\s*\(\s*"([^"]+)"\s*\)/g;
    while ((match = resourceRegex.exec(safe)) !== null) {
      const routePath = scopedPath(match.index, match[1]!);
      let p = match.index + match[0].length;
      for (;;) {
        const chain = /^\s*\.\s*(\w+)\s*\(/.exec(safe.slice(p));
        if (!chain) break;
        const open = p + chain[0].length - 1;
        const close = findMatchingParen(safe, open);
        if (close < 0) break;
        const args = safe.slice(open + 1, close);
        if (chain[1] === 'route') {
          const m2 = /web::(get|post|put|patch|delete|head)\s*\(\s*\)\s*\.to\s*\(\s*([A-Za-z_][\w:]*)/.exec(args);
          if (m2) {
            pushActixRoute(routePath, m2[1]!, m2[2]!, safe.slice(0, open).split('\n').length);
          }
        } else if (chain[1] === 'to') {
          // Direct `.resource("/x").to(handler)` (all methods).
          const m2 = /^\s*([A-Za-z_][\w:]*)/.exec(args);
          if (m2) {
            pushActixRoute(routePath, 'ANY', m2[1]!, safe.slice(0, open).split('\n').length);
          }
        }
        p = close + 1;
      }
    }

    // App-level: .route("/path", web::METHOD().to(handler)).
    const appRouteRegex = /\.route\s*\(\s*"([^"]+)"\s*,\s*web::(get|post|put|patch|delete|head)\s*\(\s*\)\s*\.to\s*\(\s*([A-Za-z_][\w:]*)/g;
    while ((match = appRouteRegex.exec(safe)) !== null) {
      const line = safe.slice(0, match.index).split('\n').length;
      pushActixRoute(scopedPath(match.index, match[1]!), match[2]!, match[3]!, line);
    }

    return { nodes, references };
  },
};

/** `/api` + `/x` → `/api/x` — slash-tolerant join for `web::scope` prefixes. */
function joinRustPath(prefix: string, sub: string): string {
  if (!prefix) return sub;
  const p = prefix.endsWith('/') ? prefix.slice(0, -1) : prefix;
  const s = sub.startsWith('/') ? sub : `/${sub}`;
  return `${p}${s}`;
}

// Directory patterns
const HANDLER_DIRS = ['/handlers/', '/handler/', '/api/', '/routes/', '/controllers/'];
const SERVICE_DIRS = ['/services/', '/service/', '/repository/', '/domain/'];
const MODEL_DIRS = ['/models/', '/model/', '/entities/', '/entity/', '/domain/', '/types/'];

const FUNCTION_KINDS = new Set(['function']);
const SERVICE_KINDS = new Set(['struct', 'trait']);
const STRUCT_KINDS = new Set(['struct']);

/** Index of the ')' that matches the '(' at openIdx, or -1 if unbalanced. */
function findMatchingParen(s: string, openIdx: number): number {
  let depth = 0;
  for (let i = openIdx; i < s.length; i++) {
    if (s[i] === '(') depth++;
    else if (s[i] === ')') {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

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
  const preferred = kindFiltered.filter((n) =>
    preferredDirPatterns.some((d) => n.filePath.includes(d))
  );

  if (preferred.length > 0) return preferred[0]!.id;

  // Fall back to any match
  return kindFiltered[0]!.id;
}

interface ModuleResolution {
  targetId: string;
  fromWorkspace: boolean;
}

function resolveModule(name: string, context: ResolutionContext): ModuleResolution | null {
  // Rust modules can be either mod.rs in a directory or name.rs
  const localPaths = [`src/${name}.rs`, `src/${name}/mod.rs`];

  const workspaceCrates = getCachedCargoWorkspaceCrateMap(context);
  const cratePath = workspaceCrates.get(name);
  const workspacePaths = cratePath
    ? [`${cratePath}/src/lib.rs`, `${cratePath}/src/main.rs`]
    : [];

  const candidates: Array<{ path: string; fromWorkspace: boolean }> = [
    ...localPaths.map((path) => ({ path, fromWorkspace: false })),
    ...workspacePaths.map((path) => ({ path, fromWorkspace: true })),
  ];

  for (const { path: modPath, fromWorkspace } of candidates) {
    if (!context.fileExists(modPath)) continue;
    const nodes = context.getNodesInFile(modPath);
    const modNode = nodes.find((n) => n.kind === 'module');
    if (modNode) return { targetId: modNode.id, fromWorkspace };
    if (nodes.length > 0) return { targetId: nodes[0]!.id, fromWorkspace };
  }

  return null;
}
