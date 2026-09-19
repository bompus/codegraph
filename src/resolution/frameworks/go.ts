/**
 * Go Framework Resolver
 *
 * Handles Gin, Echo, Fiber, Chi, and standard library patterns.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { stripCommentsForRegex } from '../strip-comments';

export const goResolver: FrameworkResolver = {
  name: 'go',
  languages: ['go'],

  detect(context: ResolutionContext): boolean {
    // Check for go.mod file (Go modules)
    const goMod = context.readFile('go.mod');
    if (goMod) {
      return true;
    }

    // Check for .go files
    const allFiles = context.getAllFiles();
    return allFiles.some((f) => f.endsWith('.go'));
  },

  resolve(ref: UnresolvedRef, context: ResolutionContext): ResolvedRef | null {
    // Pattern 1: Handler references
    if (ref.referenceName.endsWith('Handler') || ref.referenceName.startsWith('Handle')) {
      const result = resolveByNameAndKind(ref.referenceName, 'function', HANDLER_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 2: Service/Repository references
    if (ref.referenceName.endsWith('Service') || ref.referenceName.endsWith('Repository') || ref.referenceName.endsWith('Store')) {
      const result = resolveByNameAndKind(ref.referenceName, null, SERVICE_DIRS, context, SERVICE_KINDS);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.8,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 3: Middleware references
    if (ref.referenceName.endsWith('Middleware') || ref.referenceName.startsWith('Auth') || ref.referenceName.startsWith('Log')) {
      const result = resolveByNameAndKind(ref.referenceName, 'function', MIDDLEWARE_DIRS, context);
      if (result) {
        return {
          original: ref,
          targetNodeId: result,
          confidence: 0.75,
          resolvedBy: 'framework',
        };
      }
    }

    // Pattern 4: Model/Entity references (typically PascalCase structs)
    if (/^[A-Z][a-zA-Z]+$/.test(ref.referenceName)) {
      const result = resolveByNameAndKind(ref.referenceName, 'struct', MODEL_DIRS, context);
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
    if (!filePath.endsWith('.go')) return { nodes: [], references: [] };
    const nodes: Node[] = [];
    const references: UnresolvedRef[] = [];
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'go');

    // Group-prefix tracking: a registration on a GROUP var serves the group's
    // prefix + its own literal — `v1 := r.Group("/v1"); v1.GET("/x", h)` is
    // `/v1/x`, not `/x` — so the group bind must be composed onto the label.
    // Three shapes: gin/echo `x := r.Group("p")` (var-scoped), gorilla
    // `s := r.PathPrefix("p").Subrouter()` (var-scoped), chi
    // `r.Route("p", func(r chi.Router) { … })` (the literal's param, scoped to
    // its braces). Group vars are function-locals, so bindings reset at each
    // top-level `func` decl — two funcs may both declare a `v1` group with
    // different prefixes. Nested groups/Route literals compose.
    interface SpanPrefix { param: string; prefix: string; start: number; end: number; }
    const spans: SpanPrefix[] = [];
    type Event =
      | { pos: number; k: 'reset' }
      | { pos: number; k: 'assign'; lhs: string; rhs: string; path: string }
      | { pos: number; k: 'span'; recv: string; path: string; param: string; start: number; end: number }
      | { pos: number; k: 'route'; m: RegExpExecArray };
    const events: Event[] = [];
    const RESET_RE = /^func\s/gm;
    const GROUP_ASSIGN_RE = /(\w+)\s*:?=\s*(\w+)\s*\.\s*(?:Group|PathPrefix)\s*\(\s*"([^"]*)"/g;
    const ROUTE_SPAN_RE = /(\w+)\s*\.\s*Route\s*\(\s*"([^"]+)"\s*,\s*func\s*\(\s*(\w+)[^)]*\)\s*\{/g;
    let match: RegExpExecArray | null;
    while ((match = RESET_RE.exec(safe)) !== null) events.push({ pos: match.index, k: 'reset' });
    while ((match = GROUP_ASSIGN_RE.exec(safe)) !== null) {
      events.push({ pos: match.index, k: 'assign', lhs: match[1]!, rhs: match[2]!, path: match[3]! });
    }
    while ((match = ROUTE_SPAN_RE.exec(safe)) !== null) {
      const brace = match.index + match[0].length - 1;
      const close = findMatchingBrace(safe, brace);
      if (close < 0) continue;
      // exec resumes INSIDE the literal's body, so a nested `r.Route` is found
      // as its own span and composes on top.
      events.push({
        pos: match.index, k: 'span',
        recv: match[1]!, path: match[2]!, param: match[3]!, start: brace, end: close,
      });
    }

    // <anyVar>.METHOD("/path", handler) — Gin (GET/POST/...), Chi (Get/Post/...),
    // net/http (HandleFunc/Handle). The receiver is ANY identifier, not just
    // router|r|mux|app|e: real apps route on GROUP vars (`v1.GET`, `PublicGroup.GET`,
    // `userRouter.POST`), which the fixed name list missed (gin-vue-admin: 4 routes
    // for 625 files). The verb + string-path + handler-arg gates keep it route-specific.
    const routeRegex = /\b(\w+)\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Get|Post|Put|Patch|Delete|Handle|HandleFunc)\s*\(\s*"([^"]+)"\s*,\s*([^)]+)\)/g;
    while ((match = routeRegex.exec(safe)) !== null) {
      events.push({ pos: match.index, k: 'route', m: match });
    }
    events.sort((a, b) => a.pos - b.pos);

    // Sequential fold: `varPrefix` is the last-seen group binding per var
    // (function-scope approximated by the top-level `func` resets); `spans`
    // accumulates chi Route literals — innermost enclosing span wins, else
    // fall back to the var binding.
    const varPrefix = new Map<string, string>();
    const innermostSpan = (pos: number, param: string): SpanPrefix | null => {
      let best: SpanPrefix | null = null;
      for (const sp of spans) {
        if (sp.start < pos && pos < sp.end && sp.param === param && (!best || sp.start > best.start)) best = sp;
      }
      return best;
    };
    const prefixOf = (recv: string, pos: number): string =>
      innermostSpan(pos, recv)?.prefix ?? varPrefix.get(recv) ?? '';
    const routeMatches: { m: RegExpExecArray; prefix: string }[] = [];
    for (const ev of events) {
      if (ev.k === 'reset') varPrefix.clear();
      else if (ev.k === 'assign') {
        varPrefix.set(ev.lhs, joinUrlPath(prefixOf(ev.rhs, ev.pos), ev.path));
      } else if (ev.k === 'span') {
        spans.push({ param: ev.param, prefix: joinUrlPath(prefixOf(ev.recv, ev.pos), ev.path), start: ev.start, end: ev.end });
      } else {
        const recv = ev.m[1]!;
        routeMatches.push({ m: ev.m, prefix: prefixOf(recv, ev.pos) });
      }
    }

    for (const { m, prefix } of routeMatches) {
      match = m;
      const [, , rawMethod, routePath, handlerExpr] = match;

      // The first argument must be URL-shaped, or this is just a method that
      // happens to share a verb name — `cache.Put("key", val)`, `store.Get(...)`,
      // `bus.Handle("user.created", h)` all polluted the route index (#1259).
      // Real registrations use "/path" (every router), or net/http's Go 1.22
      // "METHOD /path" patterns on Handle/HandleFunc.
      const methodPrefix = matchGo122MethodPattern(routePath!, rawMethod!);
      if (!routePath!.startsWith('/') && !methodPrefix) continue;

      const line = safe.slice(0, match.index).split('\n').length;
      // "GET /users/{id}" -> method GET, path /users/{id}. The group prefix is
      // composed AFTER the Go 1.22 method-pattern strip (`"GET /x"` → `/x`
      // then gains the prefix — a group var could front a HandleFunc too).
      const literal = methodPrefix ? routePath!.slice(methodPrefix.length).trimStart() : routePath!;
      const path = prefix ? joinUrlPath(prefix, literal) : literal;
      const method = methodPrefix
        ? methodPrefix
        : rawMethod === 'Handle' || rawMethod === 'HandleFunc'
          ? 'ANY'
          : rawMethod!.toUpperCase();

      const routeNode: Node = {
        id: `route:${filePath}:${line}:${method}:${path}`,
        kind: 'route',
        name: `${method} ${path}`,
        qualifiedName: `${filePath}::route:${path}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'go',
        updatedAt: now,
      };
      nodes.push(routeNode);

      const handlerName = extractGoTailIdent(handlerExpr!);
      if (handlerName) {
        references.push({
          fromNodeId: routeNode.id,
          referenceName: handlerName,
          referenceKind: 'references',
          line,
          column: 0,
          filePath,
          language: 'go',
        });
      }
    }

    return { nodes, references };
  },
};

/** `/v1` + `/x` → `/v1/x` — slash-tolerant join for route-group prefixes. */
function joinUrlPath(prefix: string, sub: string): string {
  if (!prefix) return sub;
  const p = prefix.endsWith('/') ? prefix.slice(0, -1) : prefix;
  const s = sub.startsWith('/') ? sub : `/${sub}`;
  return `${p}${s}`;
}

/** Index of the `}` matching the `{` at `open` (Go `"`/`'`/backtick strings), -1 if unbalanced. */
function findMatchingBrace(s: string, open: number): number {
  let depth = 0;
  let inStr: string | null = null;
  for (let i = open; i < s.length; i++) {
    const c = s[i]!;
    if (inStr) {
      if (c === '\\' && inStr !== '`') i++;
      else if (c === inStr) inStr = null;
      continue;
    }
    if (c === '"' || c === '`' || c === "'") inStr = c;
    else if (c === '{') depth++;
    else if (c === '}' && --depth === 0) return i;
  }
  return -1;
}

/**
 * Go 1.22 net/http mux patterns: `mux.HandleFunc("GET /users/{id}", h)`.
 * Returns the HTTP method when the pattern starts with one, null otherwise.
 * Only Handle/HandleFunc take these — Gin/Chi verb methods take a bare path.
 */
function matchGo122MethodPattern(routePath: string, rawMethod: string): string | null {
  if (rawMethod !== 'Handle' && rawMethod !== 'HandleFunc') return null;
  const m = routePath.match(/^(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|CONNECT|TRACE)\s+\S/);
  return m ? m[1]! : null;
}

/** Extract the last identifier from an expression like `pkg.Sub.handler` or `handler`. */
function extractGoTailIdent(expr: string): string | null {
  const cleaned = expr.trim().replace(/\s+/g, '').replace(/\(\)$/, '');
  const m = cleaned.match(/(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$/);
  return m ? m[1]! : null;
}

// Directory patterns for framework resolution
const HANDLER_DIRS = ['handler', 'handlers', 'api', 'routes', 'controller', 'controllers'];
const SERVICE_DIRS = ['service', 'services', 'repository', 'store', 'pkg'];
const MIDDLEWARE_DIRS = ['middleware', 'middlewares'];
const MODEL_DIRS = ['model', 'models', 'entity', 'entities', 'domain', 'pkg'];
const SERVICE_KINDS = new Set(['struct', 'interface']);

/**
 * Resolve a symbol by name using indexed queries instead of scanning all files.
 * Uses getNodesByName (O(log n) indexed lookup) instead of iterating every file.
 */
function resolveByNameAndKind(
  name: string,
  kind: string | null,
  preferredDirs: string[],
  context: ResolutionContext,
  kinds?: Set<string>
): string | null {
  const candidates = context.getNodesByName(name);
  if (candidates.length === 0) return null;

  // Filter by kind
  const kindFiltered = candidates.filter((n) => {
    if (kinds) return kinds.has(n.kind);
    if (kind) return n.kind === kind;
    return true;
  });

  if (kindFiltered.length === 0) return null;

  // Prefer candidates in framework-conventional directories
  const preferred = kindFiltered.filter((n) =>
    preferredDirs.some((d) => n.filePath.includes(`/${d}/`))
  );

  if (preferred.length > 0) return preferred[0]!.id;

  // Fall back to any match
  return kindFiltered[0]!.id;
}
