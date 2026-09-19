/**
 * Drupal hook dispatch → hook implementation synthesis.
 *
 * Drupal dispatches hooks by a runtime STRING name through the module handler,
 * so the invocation site has no static edge to any implementation:
 *
 *   // a controller / service / .module helper — the dispatch
 *   $this->moduleHandler->invokeAll('cron');          // every hook_cron impl
 *   $this->moduleHandler->invoke('my_module', 'cron'); // my_module_cron() only
 *   $this->moduleHandler->alter(['form', 'user_login'], $data); // *_form_alter + *_user_login_alter
 *   module_invoke_all('cron');                        // legacy procedural shapes
 *   module_invoke('my_module', 'cron');
 *   drupal_alter('form', $data);
 *
 *   // the implementations — a DIFFERENT file/module, either era:
 *   function my_module_cron() { … }                        // procedural (.module/.install/.theme/.inc)
 *   #[Hook('cron')] public function run(): void { … }      // D11 attribute (src/Hook/*.php)
 *
 * The join key is the hook name, mapped to implementations by the SAME
 * conventions drupalResolver.resolve() applies to a `hook_X` ref — so a synth
 * candidate is exactly what the resolver would resolve `hook_X` to:
 *
 *   - procedural: `function` nodes in hook files (.module/.install/.theme/.inc)
 *     whose name ends `_{hook}` (registered under every `_` suffix, mirroring
 *     resolve()'s `endsWith('_'+suffix)` filter — `hook_alter` → `*_alter`
 *     candidates too), plus the full name itself;
 *   - attribute: every `#[Hook('{hook}')]` impl under `src/Hook/` via
 *     drupalHookAttributeImpls() (shares the resolver's per-context scan cache);
 *   - leftover `hook_{name}` unresolved refs: an impl whose name doesn't end
 *     with the hook suffix (docblock `Implements hook_X()` on a renamed
 *     function) is unreachable by the suffix rule — its ref row still keys the
 *     impl node.
 *
 * `invoke('mod','h')` is the one scoped shape: it calls `mod_h()` literally, so
 * candidates narrow to `function` nodes named `mod_h` plus `#[Hook('h')]` impls
 * under a `/{mod}/` path.
 *
 * Precision gates: literal string args only (`invokeAll($var)` never matches),
 * and a dispatch site emits nothing when the hook has zero in-index impls — a
 * non-Drupal `->invokeAll(`/`->alter(` collides on name only, never on target.
 * Edges are `kind:'calls'`, `provenance:'heuristic'`,
 * `metadata.synthesizedBy:'drupal-hook'`.
 */

import type { Edge, Node } from '../types';
import type { QueryBuilder } from '../db/queries';
import type { ResolutionContext } from './types';
import type { MaybeYield } from './cooperative-yield';
import { stripCommentsForRegex } from './strip-comments';
import { enclosingFn, makeLineAt } from './synth-utils';
import { drupalHookAttributeImpls, isDrupalHookFile } from './frameworks/drupal';

// All-implementations dispatch: `->invokeAll('h'` (also invokeAllDeprecated /
// invokeAllWith — same first-arg hook name) and legacy `module_invoke_all('h'`.
const DRUPAL_INVOKE_ALL_RE =
  /->\s*invokeAll(?:Deprecated|With)?\s*\(\s*['"](\w+)['"]|\bmodule_invoke_all\s*\(\s*['"](\w+)['"]/g;
// Module-scoped dispatch: `->invoke('m','h'` (also invokeDeprecated) and
// `module_invoke('m','h'` — two leading literals, so `->invoke($obj, 'a')`
// (ReflectionMethod::invoke et al.) never matches.
const DRUPAL_INVOKE_RE =
  /(?:->\s*invoke(?:Deprecated)?|\bmodule_invoke)\s*\(\s*['"](\w+)['"]\s*,\s*['"](\w+)['"]/g;
// Alter dispatch: `->alter('t'` / `->alter(['a','b']` / `->alterDeprecated(…` /
// `drupal_alter(…`. Arg is captured raw (`'t'` or `[…]` up to the first `]`)
// and literal-split by the caller; `[$var]`/dynamic content yields no key.
const DRUPAL_ALTER_RE =
  /(?:->\s*alter(?:Deprecated)?|\bdrupal_alter)\s*\(\s*(\[[^\]]*\]|'[^'\n]*'|"[^"\n]*")/g;
const DRUPAL_ALTER_LITERAL_RE = /['"](\w+)['"]/g;
// The cheap per-file prefilter: every dispatch verb contains one of these
// substrings (`invokeAllDeprecated` has `invoke`, `drupal_alter`/`alterDeprecated`
// have `alter`) — a plain includes() beats a regex here.
const drupalDispatchGate = (content: string): boolean =>
  content.includes('invoke') || content.includes('alter');
// Files a dispatch can live in: .php plus the procedural hook extensions
// (module code runs inside functions too — `mod_a()` calling `->alter(...)`).
const DRUPAL_PHP_FILE_RE = /\.(?:php|module|install|theme|inc)$/;
const DRUPAL_FANOUT_CAP = 200; // edges per file — backstop; real sites stay under

export async function drupalHookEdges(
  queries: QueryBuilder,
  ctx: ResolutionContext,
  onYield: MaybeYield
): Promise<Edge[]> {
  let scanned = 0;

  // ── Impl index: hook name → implementation nodes ──────────────────────────
  // Keyed by every `_`-suffix of each hook-file function's name (the resolver's
  // `endsWith('_'+suffix)` rule, so `x_a_b` registers `a_b` and `b`), plus the
  // full name itself for exact `mod_h` lookups.
  const impls = new Map<string, Node[]>();
  const addImpl = (key: string, node: Node): void => {
    let arr = impls.get(key);
    if (!arr) {
      arr = [];
      impls.set(key, arr);
    }
    if (!arr.some((n) => n.id === node.id)) arr.push(node);
  };
  for (const file of ctx.getAllFiles()) {
    if ((++scanned & 63) === 0) await onYield();
    if (!isDrupalHookFile(file)) continue;
    for (const n of ctx.getNodesInFile(file)) {
      if (n.kind !== 'function') continue;
      addImpl(n.name, n);
      let idx = n.name.indexOf('_');
      while (idx >= 0 && idx < n.name.length - 1) {
        addImpl(n.name.slice(idx + 1), n);
        idx = n.name.indexOf('_', idx + 1);
      }
    }
  }

  // Attribute-era impls: `#[Hook('h')]` under src/Hook/, keyed by `h`. Kept as
  // its own map too — `invoke('m','h')` needs just these (plus `m_h`), not the
  // combined suffix set (`my_module_form_node_form_alter` is an impl of
  // `form_node_form_alter`, never of `node_form_alter` — a suffix+path filter
  // would misroute it).
  const attrImpls = drupalHookAttributeImpls(ctx);
  for (const [hook, nodes] of attrImpls) {
    for (const n of nodes) addImpl(hook, n);
  }
  // No impls → no possible targets (also the non-Drupal early-out: a plain PHP
  // repo has no hook files and no src/Hook/, so this exits before any
  // dispatch-site file reads).
  if (!impls.size) return [];

  // Docblock-renamed impls the suffix rule can't reach still carry a
  // `hook_{name}` ref row when resolve() found no `*_{name}` candidate —
  // memoized per hook name (only dispatch-seen keys are ever queried).
  const refImplCache = new Map<string, Node[]>();
  const refImpls = (hook: string): Node[] => {
    let arr = refImplCache.get(hook);
    if (arr) return arr;
    arr = [];
    for (const ref of queries.getUnresolvedByName(`hook_${hook}`)) {
      const node = queries.getNodeById(ref.fromNodeId);
      if (node && !arr.some((n) => n.id === node.id)) arr.push(node);
    }
    refImplCache.set(hook, arr);
    return arr;
  };
  const implsFor = (hook: string): Node[] => {
    const base = impls.get(hook) ?? [];
    const extra = refImpls(hook);
    if (!extra.length) return base;
    const out = base.slice();
    for (const n of extra) if (!out.some((b) => b.id === n.id)) out.push(n);
    return out;
  };

  // ── Dispatch scan ─────────────────────────────────────────────────────────
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const emit = (disp: Node, target: Node, line: number, file: string, via: string): void => {
    if (target.id === disp.id) return;
    const key = `${disp.id}>${target.id}`;
    if (seen.has(key)) return;
    seen.add(key);
    edges.push({
      source: disp.id,
      target: target.id,
      kind: 'calls',
      line,
      provenance: 'heuristic',
      metadata: { synthesizedBy: 'drupal-hook', via, registeredAt: `${file}:${line}` },
    });
  };

  for (const file of ctx.getAllFiles()) {
    if ((++scanned & 63) === 0) await onYield();
    if (!DRUPAL_PHP_FILE_RE.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !drupalDispatchGate(content)) continue;
    const safe = stripCommentsForRegex(content, 'php');
    const lineAt = makeLineAt(safe, 1);
    const nodesInFile = ctx.getNodesInFile(file);
    let added = 0;
    let m: RegExpExecArray | null;

    // ->invokeAll('h' / ->invokeAllWith('h' / module_invoke_all('h' → all impls of h.
    DRUPAL_INVOKE_ALL_RE.lastIndex = 0;
    while ((m = DRUPAL_INVOKE_ALL_RE.exec(safe)) && added < DRUPAL_FANOUT_CAP) {
      const hook = m[1] ?? m[2]!;
      const targets = implsFor(hook);
      if (!targets.length) continue;
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      for (const t of targets) {
        emit(disp, t, line, file, hook);
        added++;
      }
    }

    // ->invoke('m','h' / module_invoke('m','h' → m_h() + #[Hook('h')] under /m/.
    DRUPAL_INVOKE_RE.lastIndex = 0;
    while ((m = DRUPAL_INVOKE_RE.exec(safe)) && added < DRUPAL_FANOUT_CAP) {
      const [mod, hook] = [m[1]!, m[2]!];
      const modDir = `/${mod}/`;
      // Drupal calls `{mod}_{hook}()` literally — exact-name functions only —
      // plus that module's #[Hook('{hook}')] impls (file path under /{mod}/).
      const targets: Node[] = ctx
        .getNodesByName(`${mod}_${hook}`)
        .filter((n) => n.kind === 'function');
      for (const n of attrImpls.get(hook) ?? []) {
        if (n.filePath.replace(/\\/g, '/').includes(modDir) && !targets.some((t) => t.id === n.id)) {
          targets.push(n);
        }
      }
      if (!targets.length) continue;
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      for (const t of targets) {
        emit(disp, t, line, file, `${mod}:${hook}`);
        added++;
      }
    }

    // ->alter('t' / ->alter(['a','b'] / drupal_alter('t' → all impls of each t_alter.
    DRUPAL_ALTER_RE.lastIndex = 0;
    while ((m = DRUPAL_ALTER_RE.exec(safe)) && added < DRUPAL_FANOUT_CAP) {
      const arg = m[1]!;
      const types: string[] = [];
      if (arg[0] === '[') {
        DRUPAL_ALTER_LITERAL_RE.lastIndex = 0;
        let lm: RegExpExecArray | null;
        while ((lm = DRUPAL_ALTER_LITERAL_RE.exec(arg))) types.push(lm[1]!);
      } else {
        // The capture IS the quoted literal — 'form' → form.
        const t = arg.slice(1, -1);
        if (/^\w+$/.test(t)) types.push(t);
      }
      if (!types.length) continue;
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      for (const type of types) {
        for (const t of implsFor(`${type}_alter`)) {
          emit(disp, t, line, file, `${type}_alter`);
          added++;
        }
      }
    }
  }
  return edges;
}
