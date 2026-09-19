/**
 * GoFrame Framework Resolver (route metadata) — issue #747.
 *
 * GoFrame's "standard router" binds routes reflectively, so there is no literal
 * path string at a `.GET("/x", handler)` call site and no static edge from a
 * route to the controller method that serves it. The structural facts live in
 * two places, joined only at runtime by GoFrame:
 *
 *   // api/user/v1/user_sign_in.go — the route lives in a struct tag on the request type
 *   type SignInReq struct {
 *       g.Meta `path:"/user/sign-in" method:"post" tags:"UserService" summary:"…"`
 *       …
 *   }
 *   // internal/controller/user/user_v1_sign_in.go — the handler takes *that* request type
 *   func (c *ControllerV1) SignIn(ctx context.Context, req *v1.SignInReq) (res *v1.SignInRes, err error)
 *   // internal/cmd/cmd.go — reflective binding (no path, no handler name)
 *   group.Bind(user.NewV1())
 *
 * This resolver handles the FIRST half: it reads the `g.Meta` struct tag on a
 * request type into a `route` node (`POST /user/sign-in`). The route → handler
 * EDGE is the genuinely reflective part — the method name is NOT derivable from
 * the request type (`DeptSearchReq` is served by `List`, `DeptAddReq` by `Add`),
 * so the only reliable join is the request type appearing in the method's
 * parameter signature. That whole-graph join is done by the companion
 * `goframeRouteEdges` synthesizer, which reads the request type back out of the
 * route node's qualifiedName.
 *
 * Honesty note: the route node carries the `g.Meta` path verbatim. The group
 * prefix from `s.Group("/api", …)` / nested `group.Group("/v1", …)` is applied
 * by reflective `Bind` at runtime and is deliberately NOT reconstructed here —
 * the discriminating, structural part is the per-route path + method.
 */

import { Node } from '../../types';
import { FrameworkResolver, UnresolvedRef, ResolvedRef, ResolutionContext } from '../types';
import { stripCommentsForRegex } from '../strip-comments';

/**
 * A request type carrying a routable `g.Meta` tag. `g.Meta` is, by GoFrame
 * convention, the first embedded field of the struct, so anchoring on
 * `struct { g.Meta `…` }` is both precise and cheap. Response types embed
 * `g.Meta` too but tag it `mime:"…"` with no `path:` — the path requirement
 * below filters them out.
 */
const GOFRAME_META_RE = /\btype\s+([A-Z]\w*)\s+struct\s*\{\s*g\.Meta\s+`([^`]*)`/g;
const META_PATH_RE = /\bpath:"([^"]+)"/;
const META_METHOD_RE = /\bmethod:"([^"]+)"/;
const GO_PACKAGE_RE = /^\s*package\s+(\w+)/m;

/** Marker embedded in a route node's qualifiedName so the synthesizer can read
 *  back the request type to join on. The value after it is the package-qualified
 *  request type (`cash.ListReq`) — the package disambiguates the many identical
 *  bare names (`ListReq`, `GetReq`) a large app defines, one per module. Falls
 *  back to the bare type when no `package` declaration is found. */
export const GOFRAME_ROUTE_MARKER = '::goframe-route:';

export const goframeResolver: FrameworkResolver = {
  name: 'goframe',
  languages: ['go'],

  detect(context: ResolutionContext): boolean {
    const goMod = context.readFile('go.mod');
    // GoFrame is `github.com/gogf/gf` (v1) or `github.com/gogf/gf/v2` (v2).
    return !!goMod && goMod.includes('github.com/gogf/gf');
  },

  extract(filePath, content) {
    if (!filePath.endsWith('.go')) return { nodes: [], references: [] };
    // Cheap reject: the file must mention g.Meta at all.
    if (!content.includes('g.Meta')) return { nodes: [], references: [] };

    const nodes: Node[] = [];
    const now = Date.now();
    const safe = stripCommentsForRegex(content, 'go');
    const pkg = GO_PACKAGE_RE.exec(safe)?.[1];

    GOFRAME_META_RE.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = GOFRAME_META_RE.exec(safe)) !== null) {
      const [, requestType, tag] = match;
      const pathMatch = META_PATH_RE.exec(tag!);
      if (!pathMatch) continue; // response `g.Meta `mime:…`` and other non-route metadata
      const routePath = pathMatch[1]!;
      const methodMatch = META_METHOD_RE.exec(tag!);
      // GoFrame defaults to all methods when `method:` is omitted.
      const method = methodMatch ? methodMatch[1]!.toUpperCase() : 'ANY';
      const line = safe.slice(0, match.index).split('\n').length;
      // The handler's signature qualifies the request type with its package
      // (`req *cash.ListReq`); encode `pkg.Type` so the synthesizer can match it.
      const joinKey = pkg ? `${pkg}.${requestType}` : requestType!;

      nodes.push({
        id: `route:${filePath}:${line}:${method}:${routePath}`,
        kind: 'route',
        name: `${method} ${routePath}`,
        // The request type is the synthesizer's join key — encode it after the
        // marker. The path stays human-readable in `name`.
        qualifiedName: `${filePath}${GOFRAME_ROUTE_MARKER}${joinKey}`,
        filePath,
        startLine: line,
        endLine: line,
        startColumn: 0,
        endColumn: match[0].length,
        language: 'go',
        updatedAt: now,
      });
    }

    return { nodes, references: [] };
  },

  // The route → controller-method edge is reflective (request-type join across
  // files) and is built by the `goframeRouteEdges` synthesizer after the graph
  // is complete. This resolver creates no references of its own.
  resolve(_ref: UnresolvedRef, _context: ResolutionContext): ResolvedRef | null {
    return null;
  },

  /**
   * Compose the `s.Group("prefix", func(g){ g.Bind(ctrl) })` group prefix onto
   * the `g.Meta` route labels — the route's served path is `prefix + path`
   * (`/system` + `/dept/list`), which reflective `Bind` applies at runtime.
   *
   * The chain back to a request type is: a `Bind` arg names a controller
   * (`&pkg.Type{}`, `pkg.NewX()`, `new(pkg.Type)`) → the type's methods take
   * `*pkg.XxxReq` params → those request types own the `g.Meta` tags in the
   * api files. Each link is checked: an unresolvable arg contributes nothing,
   * and a request type bound under two DIFFERENT prefixes is ambiguous and
   * skipped (silent beats wrong).
   *
   * The `id` and `qualifiedName` are preserved — `qualifiedName` carries the
   * request-type join key the synthesizer needs, and the ORIGINAL path is
   * recovered from the id (`route:file:line:METHOD:path`), keeping a repeat
   * run idempotent.
   */
  postExtract(context: ResolutionContext): Node[] {
    // reqJoinKey (`pkg.Type` or bare `Type`) → group prefix.
    const reqPrefix = new Map<string, string>();
    // Request types bound under two different prefixes — unresolvable, drop.
    const ambiguous = new Set<string>();
    // Receiver type → its method nodes, built once (Go methods qualify as
    // `T::method`).
    let methodsByType: Map<string, Node[]> | null = null;
    const methodsOf = (type: string, dir: string): Node[] => {
      if (!methodsByType) {
        methodsByType = new Map<string, Node[]>();
        for (const m of context.getNodesByKind('method')) {
          if (m.language !== 'go' || !m.signature) continue;
          const recv = m.qualifiedName.split('::')[0]!;
          let arr = methodsByType.get(recv);
          if (!arr) { arr = []; methodsByType.set(recv, arr); }
          arr.push(m);
        }
      }
      return (methodsByType.get(type) ?? []).filter((n) => !dir || n.filePath.includes(dir));
    };

    for (const filePath of context.getAllFiles()) {
      if (!filePath.endsWith('.go')) continue;
      const content = context.readFile(filePath);
      if (!content || !content.includes('.Bind(')) continue;
      const safe = stripCommentsForRegex(content, 'go');
      const imports = goImportAliases(safe);
      // Each (prefix, bindArgExpr) pair from every Bind inside a Group body.
      for (const { prefix, arg } of collectGroupBinds(safe)) {
        const ctrl = bindArgControllerType(arg, imports, context);
        if (!ctrl) continue;
        for (const reqKey of controllerRequestTypes(methodsOf(ctrl.type, ctrl.dir))) {
          const prev = reqPrefix.get(reqKey);
          if (prev !== undefined && prev !== prefix) ambiguous.add(reqKey);
          else reqPrefix.set(reqKey, prefix);
        }
      }
    }
    for (const key of ambiguous) reqPrefix.delete(key);
    if (reqPrefix.size === 0) return [];

    const updates: Node[] = [];
    for (const route of context.getNodesByKind('route')) {
      if (route.language !== 'go') continue;
      const marker = route.qualifiedName.indexOf(GOFRAME_ROUTE_MARKER);
      if (marker < 0) continue;
      const joinKey = route.qualifiedName.slice(marker + GOFRAME_ROUTE_MARKER.length);
      const prefix = reqPrefix.get(joinKey) ?? (joinKey.includes('.') ? reqPrefix.get(joinKey.slice(joinKey.lastIndexOf('.') + 1)) : undefined);
      if (!prefix) continue;
      // `route:file:line:METHOD:path` — the ORIGINAL method+path (id is
      // preserved, so a re-run recomputes from the same input).
      const head = `route:${route.filePath}:${route.startLine}:`;
      if (!route.id.startsWith(head)) continue;
      const tail = route.id.slice(head.length);
      const colon = tail.indexOf(':');
      if (colon < 0) continue;
      const method = tail.slice(0, colon);
      const original = tail.slice(colon + 1);
      const newName = `${method} ${joinUrlPath(prefix, original)}`;
      if (newName !== route.name) updates.push({ ...route, name: newName, updatedAt: Date.now() });
    }
    return updates;
  },
};

/** `/system` + `/dept/list` → `/system/dept/list` — slash-tolerant join. */
function joinUrlPath(prefix: string, sub: string): string {
  if (!prefix) return sub;
  const p = prefix.endsWith('/') ? prefix.slice(0, -1) : prefix;
  const s = sub.startsWith('/') ? sub : `/${sub}`;
  return `${p}${s}`;
}

/**
 * Parse a Go import block: alias → repo-relative dir. The module path prefix
 * isn't in file paths, so keep the last TWO segments (`x/internal/controller/
 * system` → `controller/system`) — one segment (`system`) collides with the
 * api package of the same name; two is still loose but requires a dir match.
 */
function goImportAliases(safe: string): Map<string, string> {
  const out = new Map<string, string>();
  const re = /^\s*(?:(\w+)\s+)?"([^"]+)"\s*$/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(safe)) !== null) {
    const path = m[2]!;
    const alias = m[1] ?? path.slice(path.lastIndexOf('/') + 1);
    const segs = path.split('/').filter(Boolean);
    out.set(alias, segs.slice(-2).join('/'));
  }
  return out;
}

interface GroupBind { prefix: string; arg: string; }

/**
 * Find `X.Group("p", func(g *ghttp.RouterGroup) { BODY })` — nested groups
 * compose — and yield each top-level arg of every `.Bind(...)` call inside
 * the body with the composed prefix. A `Bind` directly on `s` (no group) has
 * no prefix and is ignored.
 */
function collectGroupBinds(safe: string): GroupBind[] {
  const out: GroupBind[] = [];
  const GROUP_RE = /\.\s*Group\s*\(\s*"([^"]*)"\s*,\s*func/g;
  let m: RegExpExecArray | null;
  const spans: { start: number; end: number; prefix: string }[] = [];
  while ((m = GROUP_RE.exec(safe)) !== null) {
    // `func(params) {` — find the opening brace of the literal body.
    const braceMatch = /\(\s*[^)]*\)\s*\{/.exec(safe.slice(m.index + m[0].length));
    if (!braceMatch) continue;
    const open = m.index + m[0].length + braceMatch.index + braceMatch[0].length - 1;
    const close = matchingBrace(safe, open);
    if (close < 0) continue;
    // Compose over the enclosing group span, if this literal is nested.
    let parent = '';
    for (const sp of spans) if (sp.start < m.index && m.index < sp.end) parent = sp.prefix;
    spans.push({ start: open, end: close, prefix: joinUrlPath(parent, m[1]!) });
  }
  if (spans.length === 0) return out;

  // Every `.Bind(` inside a group body belongs to that body's prefix — a
  // nested group's binds live in the deeper span and get the deeper prefix.
  const BIND_RE = /\.\s*Bind\s*\(/g;
  while ((m = BIND_RE.exec(safe)) !== null) {
    let inner: { start: number; end: number; prefix: string } | null = null;
    for (const sp of spans) {
      if (sp.start < m.index && m.index < sp.end && (!inner || sp.start > inner.start)) inner = sp;
    }
    if (!inner || !inner.prefix || inner.prefix === '/') continue;
    const openParen = m.index + m[0].length - 1;
    const closeParen = matchingParen(safe, openParen);
    if (closeParen < 0) continue;
    for (const arg of splitTopArgs(safe.slice(openParen + 1, closeParen))) {
      out.push({ prefix: inner.prefix, arg });
    }
  }
  return out;
}

/**
 * Map a `Bind` arg to its controller type name. Shapes: `&pkg.Type{...}`,
 * `pkg.Type{...}`, `new(pkg.Type)`, `pkg.NewX(...)` (constructor — resolved
 * through the graph to its `*T` return type), and unqualified `NewX(...)` /
 * `&T{...}` for same-package binds. `dir` scopes method lookup to the
 * controller package when the arg was package-qualified.
 */
function bindArgControllerType(
  arg: string,
  imports: Map<string, string>,
  context: ResolutionContext,
): { type: string; dir: string } | null {
  const a = arg.trim();
  let m: RegExpExecArray | null;
  // `&pkg.Type{` / `pkg.Type{` / `&Type{` / `Type{`
  if ((m = /^&?\s*(?:(\w+)\s*\.\s*)?(\w+)\s*\{/.exec(a))) {
    return { type: m[2]!, dir: (m[1] && imports.get(m[1])) ?? '' };
  }
  // `new(pkg.Type)` / `new(Type)`
  if ((m = /^new\s*\(\s*(?:(\w+)\s*\.\s*)?(\w+)\s*\)/.exec(a))) {
    return { type: m[2]!, dir: (m[1] && imports.get(m[1])) ?? '' };
  }
  // `pkg.NewX(...)` / `NewX(...)` — the constructor's `*T` return is the type.
  if ((m = /^(?:(\w+)\s*\.\s*)?(New\w*)\s*\(/.exec(a))) {
    const dir = (m[1] && imports.get(m[1])) ?? '';
    const ctor = context.getNodesByName(m[2]!).filter(
      (n) => n.language === 'go' && n.kind === 'function' && (!dir || n.filePath.includes(dir)),
    );
    if (ctor.length !== 1 || !ctor[0]!.signature) return null;
    // `func NewX() *T` / `func NewX() (*T, error)` / `func NewX() *pkg.T`
    const ret = /\)\s*\(?\s*\*?\s*(?:\w+\s*\.\s*)?(\w+)/.exec(ctor[0]!.signature);
    if (!ret) return null;
    return { type: ret[1]!, dir };
  }
  return null;
}

/**
 * The `pkg.Req` request types a controller's methods take — the same join
 * keys `extract` encodes into route qualifiedNames. Qualified form matches
 * exactly; the bare form covers same-package request types (unqualified
 * signature).
 */
function controllerRequestTypes(methods: Node[]): string[] {
  const out = new Set<string>();
  for (const method of methods) {
    const re = /\*\s*(?:(\w+)\.)?([A-Z]\w*)\b/g;
    let mm: RegExpExecArray | null;
    while ((mm = re.exec(method.signature!)) !== null) {
      if (mm[1]) out.add(`${mm[1]}.${mm[2]}`);
      out.add(mm[2]!);
    }
  }
  return [...out];
}

/** Index of the `}` matching the `{` at `open` (Go `"`/`'`/backtick strings), -1 if unbalanced. */
function matchingBrace(s: string, open: number): number {
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

/** Index of the `)` matching the `(` at `open`, -1 if unbalanced. */
function matchingParen(s: string, open: number): number {
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
    else if (c === '(') depth++;
    else if (c === ')' && --depth === 0) return i;
  }
  return -1;
}

/** Split a top-level comma list, respecting nested () [] {}. */
function splitTopArgs(s: string): string[] {
  const out: string[] = [];
  let depth = 0, cur = '';
  for (const c of s) {
    if (c === '(' || c === '[' || c === '{') { depth++; cur += c; }
    else if (c === ')' || c === ']' || c === '}') { depth--; cur += c; }
    else if (c === ',' && depth === 0) { out.push(cur); cur = ''; }
    else cur += c;
  }
  if (cur.trim()) out.push(cur);
  return out;
}
