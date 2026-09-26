/**
 * Callback / observer edge synthesis — Phase 1 + 2.
 *
 * Closes dynamic-dispatch holes where a dispatcher invokes callbacks registered
 * elsewhere. Two channel shapes:
 *
 *  (1) Field-backed observer (Phase 1):
 *      onUpdate(cb) { this.callbacks.add(cb); }            // registrar
 *      triggerUpdate() { for (cb of this.callbacks) cb(); } // dispatcher
 *      scene.onUpdate(this.triggerRender)                  // registration
 *      → synthesize triggerUpdate → triggerRender
 *
 *  (2) String-keyed EventEmitter (Phase 2):
 *      this.on('mount', function onmount(){...})           // registration
 *      fn.emit('mount', this)                              // dispatch
 *      → synthesize (method containing emit('mount')) → onmount
 *
 * Whole-graph pass after base resolution. High-precision/low-recall by design:
 * named callbacks only; field channels paired by file+field; EventEmitter
 * channels capped by event fan-out (generic names like 'error' skipped — they
 * need receiver-type matching, deferred to Phase 3). All synthesized edges are
 * tagged `provenance:'heuristic'`. See docs/design/callback-edge-synthesis.md.
 */
import type { Edge, Language, Node, NodeKind } from '../types';
import type { QueryBuilder } from '../db/queries';
import type { ResolutionContext } from './types';
import { isGeneratedFile } from '../extraction/generated-detection';
import { stripCommentsForRegex, withStripMemo } from './strip-comments';
import { cFnPointerDispatchEdges } from './c-fnptr-synthesizer';
import { drupalHookEdges } from './drupal-hook-synthesizer';
import { goframeRouteEdges } from './goframe-synthesizer';
import { expoRouterReturnEdges } from './expo-router-synthesizer';
import { nextLinkEdges } from './next-router-synthesizer';
import { reactRouterLinkEdges } from './react-router-synthesizer';
import { tanstackLinkEdges } from './tanstack-router-synthesizer';
import { vueRouterLinkEdges } from './vue-router-synthesizer';
import { svelteKitLinkEdges, svelteKitPageComponentEdges } from './sveltekit-synthesizer';
import { createYielder, type MaybeYield } from './cooperative-yield';
import { crossTierEdges, endpointNodesFor } from './tier-synthesizer';
import { enclosingFn, enclosingValue, makeLineAt, matchBalanced } from './synth-utils';
import { rnModuleMethods } from './frameworks/react-native';
import { resolveImportPath } from './import-resolver';
import { isDistinctiveIdentifier } from '../search/query-utils';

const REGISTRAR_NAME = /^(on[A-Z]\w*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$/;
const DISPATCHER_NAME = /(emit|trigger|notify|dispatch|fire|publish|flush)/i;
const MAX_CALLBACKS_PER_CHANNEL = 40;
const EVENT_FANOUT_CAP = 6; // skip events with more handlers/dispatchers than this (too generic without type info)

const ON_RE = /\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:(?:async\s+)?function\s+(\w+)|(?:this\.)?(\w+))/g;
const EMIT_RE = /\.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]/g;
// Inline listeners — `.on('x', (e) => {…})`, `.on('x', e => …)`,
// `.on('x', function () {…})` (incl. `async` forms). Anonymous handlers have
// no node (extraction deliberately attributes an inline closure's calls to
// its enclosing function), so the registration is attributed to that same
// enclosing function — where the event's work demonstrably happens — or to
// the smallest enclosing constant/variable for an object-literal API site
// (the rn-event-channel cross-language arm already does exactly this).
const ON_INLINE_RE =
  /\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:async\s+)?(?:\([^)]*\)\s*=>|[A-Za-z_$][\w$]*\s*=>|function\s*\()/g;
const SETSTATE_RE = /this\.setState\s*\(/;
const FLUTTER_SETSTATE_RE = /\bsetState\s*\(/; // Flutter: setState((){…}) / this.setState
const JS_FAMILY = ['typescript', 'javascript', 'tsx', 'jsx'];
const JSX_TAG_RE = /<([A-Z][A-Za-z0-9_]*)[\s/>]/g;
const MAX_JSX_CHILDREN = 30;
// Vue SFC templates: kebab-case child components (<el-button> → ElButton) and
// event bindings (@click="fn" / v-on:click="fn"). PascalCase children (<VPNav/>)
// are already caught by JSX_TAG_RE via the SFC component node.
const VUE_KEBAB_RE = /<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)[\s/>]/g;
// PascalCase component tags — `<MediaCard ...>`, `<NavBar/>`. HTML elements are
// lowercase, so an uppercase-initial tag is a component usage; built-ins
// (`<NuxtLink>`, `<Transition>`) simply resolve to nothing and emit no edge.
const VUE_PASCAL_RE = /<([A-Z][A-Za-z0-9]*)[\s/>]/g;
const VUE_HANDLER_RE = /(?:@|v-on:)([a-zA-Z][\w-]*)(?:\.[\w]+)*\s*=\s*"([^"]+)"/g;
// Composable/hook destructure: `const { close: closeSidebar } = useSidebarControl()`.
// Captures the destructure body + the called composable; only `use*` calls qualify.
const VUE_DESTRUCTURE_RE = /(?:const|let|var)\s*\{([^}]+)\}\s*=\s*(\w+)\s*\(/g;

// Vue component registrations — a template tag resolves through the REGISTERED
// name before the PascalCase guess: `app.component('fancy-widget', FancyThing)`
// (global, Vue 3 entry / `Vue.component` in Vue 2 / `nuxtApp.vueApp` in Nuxt
// plugins) and an SFC's own `components: { 'my-comp': MyComp }` (local,
// options API). Both map a kebab name onto a component whose own name may be
// spelled differently, which a kebab→Pascal lookup can never find.
const VUE_APP_GATE_RE = /\bcreateApp\s*\(|\bVue\.component\s*\(|\.vueApp\b/;
const VUE_APP_COMPONENT_RE =
  /(?:[\w$]+\s*\([^()]*\)|[\w$]+(?:\.[\w$]+)*)\s*\.\s*component\s*\(\s*['"]([^'"]+)['"]\s*,\s*(?:defineAsyncComponent\s*\(\s*)?(\(\s*\)\s*=>\s*import\s*\(\s*['"]([^'"]+)['"]|[A-Za-z_$][\w$]*)/g;
const VUE_COMPONENTS_BLOCK_RE = /\bcomponents\s*:\s*\{/g;
// Entries inside a `components: { … }` block: `'my-comp': Comp`, `myComp: Comp`,
// `Comp` shorthand, or a lazy `x: () => import('./X.vue')` /
// `x: defineAsyncComponent(() => import('./X.vue'))`.
const VUE_COMP_ENTRY_RE =
  /(['"])([\w$-]+)\1\s*:\s*(?:(?:defineAsyncComponent\s*\(\s*)?\(\s*\)\s*=>\s*import\s*\(\s*['"]([^'"]+)['"]|([A-Za-z_$][\w$]*))|\b([A-Za-z_$][\w$]*)\s*:\s*(?:(?:defineAsyncComponent\s*\(\s*)?\(\s*\)\s*=>\s*import\s*\(\s*['"]([^'"]+)['"]|([A-Za-z_$][\w$]*))|\b([A-Za-z_$][\w$]*)\s*(?=[,}])/g;

/** A tag's registration-key spellings — Vue's resolveAsset tries the raw name, its camelCase, its PascalCase, and its hyphenated form. */
function vueTagVariants(tag: string): string[] {
  const camel = tag.replace(/-([a-z0-9])/g, (_s, c: string) => c.toUpperCase());
  const pascal = camel.charAt(0).toUpperCase() + camel.slice(1);
  const kebab = camel.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();
  return [...new Set([tag, camel, pascal, kebab])];
}

// Closure-collection dynamic dispatch (language-agnostic, Swift-first). A method
// appends a closure to a collection property; another method iterates that
// property *invoking each element* (`coll.forEach { $0() }` / `{ it() }`). The
// element-invoke (`$0(` / `it(`) PROVES the collection holds closures, so pairing
// a dispatcher to same-named registrars (`.append`/`.add`/`.push`/`.insert`,
// incl. Swift `prop.write { $0.append }`) is high-precision. Cross-file/class by
// design: Alamofire appends in `DataRequest.validate` but iterates in the base
// `Request.didCompleteTask` — neither same-file nor same-class pairing reaches it.
const CC_DISPATCH_RE = /(\w+)\.forEach\s*\{\s*(?:\$0|it)\s*\(/g;
const CC_APPEND_WRITE_RE = /(\w+)\.write\s*\{\s*\$0(?:\.(\w+))?\.(?:append|add|push|insert)\s*\(/g;
const CC_APPEND_DIRECT_RE = /(\w+)\.(?:append|add|push|insert)\s*\(/g;
const CC_FANOUT_CAP = 8; // skip a field name with more dispatchers/registrars than this (too generic to pair confidently)
// The dispatcher gate — `{ $0( ` / `{ it( ` element-invocation — is Swift/Kotlin
// trailing-closure syntax, so ONLY those languages can ever contribute a
// dispatcher, and a cross-language registrar pairing (a JS `.push(` against a
// Swift dispatcher's field name) would be a wrong edge, not a missed one.
// Gating both sides here isn't just precision: `.push(`/`.add(` is everywhere
// in JS/PHP, so an ungated scan slices + regexes nearly every function on repos
// where the pass cannot emit a single edge — on a 12k-file PHP/JS app that was
// 20+ minutes of the "Resolving refs" tail and a #850 watchdog kill (#1235).
const CC_LANGUAGES = new Set(['swift', 'kotlin']);

function kebabToPascal(s: string): string {
  return s.split('-').map((p) => p.charAt(0).toUpperCase() + p.slice(1)).join('');
}

/**
 * Nuxt auto-import name for a component, derived from its path UNDER `components/`:
 * `components/media/Card.vue` → `MediaCard`, `components/base/foo/Bar.vue` →
 * `BaseFooBar`. Each directory segment and the filename is PascalCased and
 * concatenated; a directory whose PascalCase name prefixes the next segment is
 * collapsed (Nuxt's de-dup: `base/BaseButton.vue` → `BaseButton`, not
 * `BaseBaseButton`). Returns null for a flat component (`components/NavBar.vue`)
 * — its node is already named by basename, so a direct tag match finds it.
 */
function nuxtComponentName(filePath: string): string | null {
  const marker = filePath.lastIndexOf('components/');
  if (marker === -1) return null;
  const rel = filePath.slice(marker + 'components/'.length).replace(/\.(vue|ts|tsx|js|jsx)$/i, '');
  const segs = rel.split('/').filter(Boolean).map(kebabToPascal);
  if (segs.length < 2) return null;
  const out: string[] = [];
  for (const s of segs) {
    const prev = out[out.length - 1];
    if (prev && s.startsWith(prev)) out[out.length - 1] = s;
    else out.push(s);
  }
  return out.join('');
}

// Callers walk one file's nodes in a row, so the last file's lines are kept:
// splitting the whole file once per node was the JSX and render passes' top
// cost (466 ms of a 2.8 s pretix run).
let sliceLinesSource: string | null = null;
let sliceLinesSplit: string[] = [];
function sliceLines(content: string, startLine?: number, endLine?: number): string | null {
  if (!startLine || !endLine) return null;
  if (content !== sliceLinesSource) {
    sliceLinesSource = content;
    sliceLinesSplit = content.split('\n');
  }
  return sliceLinesSplit.slice(startLine - 1, endLine).join('\n');
}

// `src.slice(0, idx).split('\n').length` per match is quadratic on a file with
// many matches (the registry pass's top cost). The line index is built once
// per text, and the last text's index is kept.
let lineOfSource: string | null = null;
let lineOfIndex: (idx: number) => number = () => 1;
function lineOf(src: string, idx: number): number {
  if (src !== lineOfSource) {
    lineOfSource = src;
    lineOfIndex = makeLineAt(src, 1);
  }
  return lineOfIndex(idx);
}

function registrarField(src: string): string | null {
  const m = src.match(/this\.(\w+)\.(?:add|push|set)\(/);
  return m ? m[1]! : null;
}

function dispatcherField(src: string): string | null {
  const forOf = src.match(/\bof\s+(?:Array\.from\(\s*)?this\.(\w+)/);
  if (forOf && /\b\w+\s*\(/.test(src)) return forOf[1]!;
  const forEach = src.match(/this\.(\w+)\.forEach\(/);
  if (forEach) return forEach[1]!;
  return null;
}

/**
 * Stream method + function nodes lazily. The synthesizers only scan-and-filter
 * down to a tiny matched subset, so materializing every function/method (which
 * is gigabytes on a symbol-dense project) just to iterate it once is what OOM'd
 * #610. Iterating keeps memory O(1) in the node count.
 */
function* methodAndFunctionNodes(queries: QueryBuilder): IterableIterator<Node> {
  yield* queries.iterateNodesByKind('method');
  yield* queries.iterateNodesByKind('function');
}

/** Phase 1: field-backed observer channels (registrar/dispatcher share a store). */
async function fieldChannelEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  const registrars: Array<{ node: Node; field: string }> = [];
  const dispatchers: Array<{ node: Node; field: string }> = [];

  let scanned = 0;
  for (const m of methodAndFunctionNodes(queries)) {
    if ((++scanned & 255) === 0) await onYield(); // #1091: yield mid-scan on huge graphs
    const isReg = REGISTRAR_NAME.test(m.name);
    const isDisp = DISPATCHER_NAME.test(m.name);
    if (!isReg && !isDisp) continue;
    const content = ctx.readFile(m.filePath);
    const src = content && sliceLines(content, m.startLine, m.endLine);
    if (!src) continue;
    if (isReg) { const f = registrarField(src); if (f) registrars.push({ node: m, field: f }); }
    if (isDisp) { const f = dispatcherField(src); if (f) dispatchers.push({ node: m, field: f }); }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const reg of registrars) {
    const chDispatchers = dispatchers.filter(
      (d) => d.node.filePath === reg.node.filePath && d.field === reg.field
    );
    if (chDispatchers.length === 0) continue;
    const argRe = new RegExp(`${reg.node.name}\\s*\\(\\s*(?:this\\.)?(\\w+)`);
    let added = 0;
    for (const e of queries.getIncomingEdges(reg.node.id, ['calls'])) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      if (!e.line) continue;
      const caller = queries.getNodeById(e.source);
      if (!caller) continue;
      const line = ctx.readFile(caller.filePath)?.split('\n')[e.line - 1];
      const am = line?.match(argRe);
      if (!am) continue;
      const fn = ctx.getNodesByName(am[1]!).find((n) => n.kind === 'method' || n.kind === 'function');
      if (!fn) continue;
      for (const disp of chDispatchers) {
        if (disp.node.id === fn.id) continue;
        const key = `${disp.node.id}>${fn.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.node.id, target: fn.id, kind: 'calls', line: disp.node.startLine,
          provenance: 'heuristic',
          metadata: {
            synthesizedBy: 'callback', via: reg.node.name, field: reg.field,
            // Where the callback was wired up (`scene.onUpdate(this.triggerRender)`).
            // This is the #1 thing an agent reads/greps to explain the flow — surface
            // it so node/trace/context can show it without a callers() + Read round-trip.
            registeredAt: `${caller.filePath}:${e.line}`,
          },
        });
        added++;
      }
    }
  }
  return edges;
}

/**
 * Closure-collection dispatch: dispatcher iterates a closure-collection property
 * invoking each element; registrar appends a closure to the same-named property.
 * Emits dispatcher → registrar so a flow reaches the registration site (where the
 * appended closure's body — and its callers — live). High-precision: the
 * dispatcher's element-invoke is the gate (a `.forEach` that does NOT invoke its
 * element is ignored), so a repo with no closure-collection dispatch yields zero
 * edges regardless of how many `.append`/`.push` sites it has.
 *
 * Pairs globally by field name (cross-file/class is required — see Alamofire's
 * base-class `Request.didCompleteTask` iterating `validators` appended by the
 * subclass `DataRequest.validate`), bounded by a fan-out cap so a generic field
 * name shared across unrelated classes can't fan out into noise.
 */
async function closureCollectionEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  const dispatchers = new Map<string, Array<{ node: Node; line: number }>>(); // field → dispatcher methods + forEach line
  const registrars = new Map<string, Array<{ node: Node; line: number }>>();   // field → registrar methods + append line

  const addReg = (field: string | undefined, node: Node, absLine: number) => {
    if (!field || /^\d+$/.test(field)) return; // `$0.append` mis-captures the `0`; the write-RE owns that field
    const arr = registrars.get(field) ?? [];
    if (!arr.some((r) => r.node.id === node.id)) arr.push({ node, line: absLine });
    registrars.set(field, arr);
  };

  // Slices EVERY Swift/Kotlin method/function's source (no cheap name-gate), so
  // on a repo with a huge file this is the heaviest synthesis pass — yield
  // mid-scan (and mid-match-loop below: a single generated function dense with
  // matches must not starve the watchdog either) so it can't wedge the #850
  // watchdog on its own (#1091, #1235).
  let scanned = 0;
  let matchTick = 0;
  for (const m of methodAndFunctionNodes(queries)) {
    if ((++scanned & 127) === 0) await onYield();
    if (!CC_LANGUAGES.has(m.language)) continue;
    const content = ctx.readFile(m.filePath);
    const src = content && sliceLines(content, m.startLine, m.endLine);
    if (!src) continue;
    const hasForEach = src.includes('.forEach');
    const hasAppend = src.includes('.append(') || src.includes('.add(') || src.includes('.push(') || src.includes('.insert(');
    if (!hasForEach && !hasAppend) continue;
    const lineAt = makeLineAt(src, m.startLine ?? 1);

    if (hasForEach) {
      CC_DISPATCH_RE.lastIndex = 0;
      let d: RegExpExecArray | null;
      while ((d = CC_DISPATCH_RE.exec(src))) {
        if ((++matchTick & 255) === 0) await onYield();
        const arr = dispatchers.get(d[1]!) ?? [];
        if (!arr.some((n) => n.node.id === m.id)) arr.push({ node: m, line: lineAt(d.index) });
        dispatchers.set(d[1]!, arr);
      }
    }
    if (hasAppend) {
      CC_APPEND_WRITE_RE.lastIndex = 0;
      let w: RegExpExecArray | null;
      while ((w = CC_APPEND_WRITE_RE.exec(src))) {
        if ((++matchTick & 255) === 0) await onYield();
        addReg(w[2] || w[1], m, lineAt(w.index)); // nested `$0.streams` else the `.write` receiver
      }
      CC_APPEND_DIRECT_RE.lastIndex = 0;
      let a: RegExpExecArray | null;
      while ((a = CC_APPEND_DIRECT_RE.exec(src))) {
        if ((++matchTick & 255) === 0) await onYield();
        addReg(a[1], m, lineAt(a.index));
      }
    }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const [field, disps] of dispatchers) {
    const regs = registrars.get(field);
    if (!regs || regs.length === 0) continue;
    if (disps.length > CC_FANOUT_CAP || regs.length > CC_FANOUT_CAP) continue; // generic field — can't pair confidently
    for (const disp of disps) for (const reg of regs) {
      if (disp.node.id === reg.node.id) continue;
      const key = `${disp.node.id}>${reg.node.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.node.id, target: reg.node.id, kind: 'calls', line: disp.line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'closure-collection', field, registeredAt: `${reg.node.filePath}:${reg.line}` },
      });
    }
  }
  return edges;
}

/** Phase 2: string-keyed EventEmitter channels (on('e', fn) ↔ emit('e')). */
async function eventEmitterEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const emitsByEvent = new Map<string, Set<string>>();          // event → dispatcher node ids
  const handlersByEvent = new Map<string, Map<string, string>>(); // event → handler id → registration site (file:line)

  let scanned = 0;
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if ((++scanned & 255) === 0) await onYield(); // #1091: yield mid-scan on huge graphs
    const content = ctx.readFile(file);
    if (!content) continue;
    const hasEmit = content.includes('.emit(') || content.includes('.fire(') || content.includes('.dispatchEvent(');
    const hasOn = content.includes('.on(') || content.includes('.once(') || content.includes('.addListener(');
    if (!hasEmit && !hasOn) continue;
    const nodesInFile = ctx.getNodesInFile(file);
    const lineOf = makeLineAt(content, 1);

    if (hasEmit) {
      EMIT_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = EMIT_RE.exec(content))) {
        const disp = enclosingFn(nodesInFile, lineOf(m.index));
        if (!disp) continue;
        const set = emitsByEvent.get(m[1]!) ?? new Set<string>();
        set.add(disp.id); emitsByEvent.set(m[1]!, set);
      }
    }
    if (hasOn) {
      ON_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = ON_RE.exec(content))) {
        const handlerName = m[2] || m[3];
        if (!handlerName) continue;
        const handler = ctx.getNodesByName(handlerName).find((n) => n.kind === 'function' || n.kind === 'method');
        if (!handler) continue;
        const map = handlersByEvent.get(m[1]!) ?? new Map<string, string>();
        map.set(handler.id, `${file}:${lineOf(m.index)}`); handlersByEvent.set(m[1]!, map);
      }
      // Inline listeners: the anonymous arrow/function can't be looked up by
      // name (extraction attributes its calls to the enclosing function), so
      // the edge lands on the enclosing function — the same node the
      // listener's own calls attribute to — or on the smallest enclosing
      // constant/variable for an object-literal API's registration site.
      ON_INLINE_RE.lastIndex = 0;
      while ((m = ON_INLINE_RE.exec(content))) {
        const handler = enclosingFn(nodesInFile, lineOf(m.index)) ?? enclosingValue(nodesInFile, lineOf(m.index));
        if (!handler) continue;
        const map = handlersByEvent.get(m[1]!) ?? new Map<string, string>();
        if (!map.has(handler.id)) map.set(handler.id, `${file}:${lineOf(m.index)}`);
        handlersByEvent.set(m[1]!, map);
      }
    }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const [event, dispatchers] of emitsByEvent) {
    const handlers = handlersByEvent.get(event);
    if (!handlers) continue;
    // Precision guard: a generic event name with many handlers/dispatchers can't
    // be matched without receiver-type info (Phase 3) — skip rather than over-link.
    if (dispatchers.size > EVENT_FANOUT_CAP || handlers.size > EVENT_FANOUT_CAP) continue;
    for (const d of dispatchers) for (const [h, registeredAt] of handlers) {
      if (d === h) continue;
      const key = `${d}>${h}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({ source: d, target: h, kind: 'calls', provenance: 'heuristic', metadata: { synthesizedBy: 'event-emitter', event, registeredAt } });
    }
  }
  return edges;
}

/**
 * Window `postMessage` ↔ `addEventListener('message')` keyed on a payload
 * discriminator (`source: CONST` / `data.source === CONST`). The DOM event
 * name is always `message`, so pairing on that string would fan out. Skip
 * generic names and cap fan-out like the EventEmitter pass.
 */
const WINDOW_MSG_GENERIC = new Set(['message', 'data', 'event', 'source', 'type', 'error', 'ping', 'pong']);
const POSTMESSAGE_RE = /\.postMessage\s*\(/g;
const MESSAGE_LISTEN_RE = /\.addEventListener\s*\(\s*['"]message['"]\s*,\s*/g;
const SOURCE_FIELD_RE = /\bsource\s*:\s*(?:(['"])([^'"\n]+)\1|((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*))/g;
const SOURCE_CMP_RE = /\.source\s*(?:!==|===|!=|==)\s*(?:(['"])([^'"\n]+)\1|((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*))/g;
const SOURCE_CMP_REV_RE = /(?:(['"])([^'"\n]+)\1|((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*))\s*(?:!==|===|!=|==)\s*[\w.$]*\.source/g;
const NAMED_LISTEN_HANDLER_RE = /^(?:async\s+)?([A-Za-z_$][\w$]*)\s*[,)]/;

function windowMessageKey(lit: string | undefined, mem: string | undefined): string | null {
  if (lit) {
    if (lit.length < 4 || WINDOW_MSG_GENERIC.has(lit.toLowerCase())) return null;
    return `lit:${lit}`;
  }
  const name = mem?.split('.').pop();
  if (!name || WINDOW_MSG_GENERIC.has(name.toLowerCase()) || !isDistinctiveIdentifier(name)) return null;
  return `mem:${name}`;
}

function windowMessageKeys(chunk: string, field: boolean): string[] {
  const keys = new Set<string>();
  const add = (lit?: string, mem?: string) => {
    const key = windowMessageKey(lit, mem);
    if (key) keys.add(key);
  };
  if (field) {
    SOURCE_FIELD_RE.lastIndex = 0;
    for (let m; (m = SOURCE_FIELD_RE.exec(chunk)); ) add(m[2], m[3]);
  } else {
    SOURCE_CMP_RE.lastIndex = 0;
    for (let m; (m = SOURCE_CMP_RE.exec(chunk)); ) add(m[2], m[3]);
    SOURCE_CMP_REV_RE.lastIndex = 0;
    for (let m; (m = SOURCE_CMP_REV_RE.exec(chunk)); ) add(m[2], m[3]);
  }
  return [...keys];
}

function nodeSource(ctx: ResolutionContext, node: Node): string {
  const content = ctx.readFile(node.filePath);
  if (!content) return '';
  return content.split('\n').slice(node.startLine - 1, node.endLine).join('\n');
}

async function windowMessageEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  const posts = new Map<string, Set<string>>();
  const listeners = new Map<string, Map<string, string>>();
  let scannedFiles = 0;
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    const content = ctx.readFile(file);
    if (!content || (!content.includes('postMessage') && !content.includes('addEventListener'))) continue;
    const nodesInFile = ctx.getNodesInFile(file);
    if (!nodesInFile.some((n) => n.language && JS_FAMILY.includes(n.language))) continue;
    const lineOf = makeLineAt(content, 1);

    POSTMESSAGE_RE.lastIndex = 0;
    for (let m; (m = POSTMESSAGE_RE.exec(content)); ) {
      const disp = enclosingFn(nodesInFile, lineOf(m.index));
      if (!disp) continue;
      for (const key of windowMessageKeys(content.slice(m.index, m.index + 500), true)) {
        const set = posts.get(key) ?? new Set<string>();
        set.add(disp.id);
        posts.set(key, set);
      }
    }

    MESSAGE_LISTEN_RE.lastIndex = 0;
    for (let m; (m = MESSAGE_LISTEN_RE.exec(content)); ) {
      const rest = content.slice(m.index + m[0].length);
      const named = rest.match(NAMED_LISTEN_HANDLER_RE)?.[1];
      const line = lineOf(m.index);
      let handler = named
        ? ctx.getNodesByName(named).find((n) => n.kind === 'function' || n.kind === 'method')
        : null;
      if (!handler) handler = enclosingFn(nodesInFile, line);
      if (!handler) continue;
      for (const key of windowMessageKeys(nodeSource(ctx, handler), false)) {
        const map = listeners.get(key) ?? new Map<string, string>();
        map.set(handler.id, `${file}:${line}`);
        listeners.set(key, map);
      }
    }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const [key, dispatchers] of posts) {
    const handlers = listeners.get(key);
    if (!handlers) continue;
    if (dispatchers.size > EVENT_FANOUT_CAP || handlers.size > EVENT_FANOUT_CAP) continue;
    const event = key.slice(key.indexOf(':') + 1);
    for (const d of dispatchers) for (const [h, registeredAt] of handlers) {
      if (d === h) continue;
      const pair = `${d}>${h}`;
      if (seen.has(pair)) continue;
      seen.add(pair);
      edges.push({
        source: d, target: h, kind: 'calls', provenance: 'heuristic',
        metadata: { synthesizedBy: 'window-message', event, registeredAt },
      });
    }
  }
  return edges;
}

/**
 * Phase 4: React class-component re-render. `this.setState(...)` re-runs the
 * component's `render()`, but that hop is React-internal — no static edge — so a
 * flow like "mutation → setState → canvas repaint" dead-ends at setState even
 * though `render → getRenderableElements → …` is fully call-connected after it.
 * Bridge it: for each class that has a `render` method, link every sibling method
 * whose body calls `this.setState(` → `render`. The setState gate keeps this to
 * React class components (a non-React class with a `render` method won't call
 * `this.setState`). Over-approximation (all setState methods reach render) is
 * accepted — it's reachability-correct, like the callback channels.
 */
async function reactRenderEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  // A class can only emit here if it CONTAINS a method named `render` — so one
  // indexed name lookup bounds the candidate set up front, and the class scan
  // below skips everything else before its per-class edge/node queries. On a
  // repo with few/no render methods (any non-React codebase) this collapses
  // the pass from every-class fan-out to ~zero DB work, with identical output:
  // the skipped classes fail the same `render` check today, just after paying
  // for their children. (Not a language gate: `render` + `this.setState(` in
  // Java — e.g. Litho — legitimately matches today and still does.)
  const renderOwners = new Set<string>();
  for (const n of ctx.getNodesByName('render')) {
    if (n.kind !== 'method') continue;
    for (const e of queries.getIncomingEdges(n.id, ['contains'])) renderOwners.add(e.source);
  }
  if (renderOwners.size === 0) return edges;
  for (const cls of queries.iterateNodesByKind('class')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (!renderOwners.has(cls.id)) continue;
    const children = queries.getOutgoingEdges(cls.id, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n && n.kind === 'method');
    const render = children.find((n) => n.name === 'render');
    if (!render) continue;
    let added = 0;
    for (const m of children) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      if (m.id === render.id) continue;
      const content = ctx.readFile(m.filePath);
      const src = content && sliceLines(content, m.startLine, m.endLine);
      if (!src || !SETSTATE_RE.test(src)) continue;
      const key = `${m.id}>${render.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: m.id, target: render.id, kind: 'calls', line: m.startLine,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'react-render', via: 'setState', registeredAt: `${render.filePath}:${render.startLine}` },
      });
      added++;
    }
  }
  return edges;
}

/**
 * Phase 4b: Flutter setState → build (the Dart analog of react-render). In a
 * StatefulWidget's State class, `setState(() {…})` re-runs `build(context)`, but
 * that hop is framework-internal (Flutter calls build), so a flow like
 * "onPressed → _increment → setState → rebuilt UI" dead-ends at setState. Bridge
 * it: for each Dart class with a `build` method, link every sibling method whose
 * body calls `setState(` → `build`. The setState gate + `.dart` file keep this to
 * Flutter State classes. Over-approximation accepted (reachability-correct).
 *
 * Second phase (same pass): named navigation — `Navigator.pushNamed(context,
 * '/detail')` is a string-keyed dispatch whose destination lives in the app's
 * `routes:` table (`MaterialApp(routes: { '/detail': (ctx) => DetailPage() })`),
 * not at the callsite, so nothing links caller → page widget. Build a
 * route-name → widget-class map from every `routes:` map literal (only inside a
 * file that constructs a `MaterialApp`/`CupertinoApp`/`WidgetsApp`), then link
 * the enclosing method at each `Navigator.*Named(…, '/route')` site to the
 * widget class that route builds. `Navigator.push(ctx, MaterialPageRoute(
 * builder: (_) => X()))` needs nothing — the `X()` constructor is a literal
 * call in the enclosing method already. `GetPage(name:…, page:…)`/GetX's
 * `Get.toNamed` is a different registration shape — deferred.
 */
const FLUTTER_APP_RE = /\b(?:MaterialApp|CupertinoApp|WidgetsApp|GetMaterialApp)\s*\(/;
const FLUTTER_ROUTES_BLOCK_RE = /\broutes\s*:\s*\{/g;
const FLUTTER_ROUTE_ENTRY_RE =
  /['"]([^'"]+)['"]\s*:\s*(?:\([^)]*\)|[A-Za-z_]\w*)\s*=>\s*(?:const\s+)?([A-Z]\w*)\s*\(/g;
const FLUTTER_NAMED_NAV_RE =
  /\b[Nn]avigator\s*\.\s*(?:of\s*\([^()]*\)\s*\.\s*)?(?:pushNamed|pushReplacementNamed|pushNamedAndRemoveUntil|popAndPushNamed|restorablePushNamed|restorablePushReplacementNamed)\s*\(\s*(?:[\w$.!]+\s*,\s*)?['"]([^'"]+)['"]/g;

async function flutterBuildEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const cls of queries.iterateNodesByKind('class')) {
    if ((++scanned255 & 63) === 0) await onYield();
    const children = queries.getOutgoingEdges(cls.id, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n && n.kind === 'method');
    const build = children.find((n) => n.name === 'build');
    if (!build || !build.filePath.endsWith('.dart')) continue;
    let added = 0;
    for (const m of children) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      if (m.id === build.id) continue;
      const content = ctx.readFile(m.filePath);
      const src = content && sliceLines(content, m.startLine, m.endLine);
      if (!src || !FLUTTER_SETSTATE_RE.test(src)) continue;
      const key = `${m.id}>${build.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: m.id, target: build.id, kind: 'calls', line: m.startLine,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'flutter-build', via: 'setState', registeredAt: `${build.filePath}:${build.startLine}` },
      });
      added++;
    }
  }

  // ── named routes → page widgets ──────────────────────────────────────────
  // Pass 1: collect `routes: { '/x': (ctx) => XPage(), … }` entries — only
  // from a file that constructs an app widget, so a `routes:` map on an
  // unrelated object literal never registers.
  const routeWidgets = new Map<string, { widget: string; file: string }>();
  const routeAmbiguous = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if (!file.endsWith('.dart')) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('routes') || !FLUTTER_APP_RE.test(content)) continue;
    FLUTTER_ROUTES_BLOCK_RE.lastIndex = 0;
    let bm: RegExpExecArray | null;
    while ((bm = FLUTTER_ROUTES_BLOCK_RE.exec(content))) {
      const open = bm.index + bm[0].length - 1;
      const close = matchBalanced(content, open);
      if (close === -1) continue;
      const block = content.slice(open, close + 1);
      FLUTTER_ROUTE_ENTRY_RE.lastIndex = 0;
      let em: RegExpExecArray | null;
      while ((em = FLUTTER_ROUTE_ENTRY_RE.exec(block))) {
        const [, routeName, widget] = em;
        const prev = routeWidgets.get(routeName!);
        if (prev && (prev.widget !== widget! || prev.file !== file)) {
          // Two different widgets behind one route name — can't tell which a
          // pushNamed means, so drop the route rather than guess.
          routeAmbiguous.add(routeName!);
          routeWidgets.delete(routeName!);
        } else if (!routeAmbiguous.has(routeName!)) {
          routeWidgets.set(routeName!, { widget: widget!, file });
        }
      }
    }
  }
  if (routeWidgets.size === 0) return edges;

  // Pass 2: `Navigator.pushNamed(context, '/x')` sites. The registered-name
  // backstop — only a route string actually declared in a `routes:` map ever
  // links — is the precision gate; an unknown name yields nothing.
  let scannedFiles = 0;
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!file.endsWith('.dart')) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('pushNamed')) continue;
    const nodesInFile = ctx.getNodesInFile(file);
    const lineOf = makeLineAt(content, 1);
    FLUTTER_NAMED_NAV_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = FLUTTER_NAMED_NAV_RE.exec(content))) {
      const reg = routeWidgets.get(m[1]!);
      if (!reg) continue;
      const candidates = ctx
        .getNodesByName(reg.widget)
        .filter((n) => n.kind === 'class' && n.filePath.endsWith('.dart'));
      const widgetCls =
        candidates.find((n) => n.filePath === reg.file) ??
        (candidates.length === 1 ? candidates[0] : undefined);
      if (!widgetCls) continue;
      const disp =
        enclosingFn(nodesInFile, lineOf(m.index)) ?? enclosingValue(nodesInFile, lineOf(m.index));
      if (!disp || disp.id === widgetCls.id) continue;
      const key = `${disp.id}>${widgetCls.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: widgetCls.id,
        kind: 'navigates',
        line: lineOf(m.index),
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'flutter-nav', route: m[1], registeredAt: `${file}:${lineOf(m.index)}` },
      });
    }
  }
  return edges;
}

/**
 * Reactive ArkUI property decorators: assigning a property carrying one of
 * these re-runs the owning struct's `build()`. Covers both state models —
 * V1 (`@Component`: State/Prop/Link/Provide/Consume/Storage*) and V2
 * (`@ComponentV2`: Local/Provider/Consumer; `@Param` is read-only in V2 so
 * the assignment gate never fires on it, and `@Trace` lives on `@ObservedV2`
 * data classes, not struct properties).
 */
const ARKUI_REACTIVE_DECORATORS = new Set([
  'State', 'Prop', 'Link', 'Provide', 'Consume', 'StorageLink', 'StorageProp',
  'LocalStorageLink', 'LocalStorageProp', 'ObjectLink',
  'Local', 'Provider', 'Consumer',
]);

/** ArkUI-observed array mutators — `this.todos.push(x)` re-renders like an assignment. */
const ARKUI_ARRAY_MUTATORS = 'push|pop|shift|unshift|splice|sort|reverse|fill';

/**
 * Phase 4b-ets: ArkUI state → build (the ArkTS analog of react-render /
 * flutter-build). Assigning a reactive-decorated property (`@State count`,
 * `@Link selected`, …) re-runs the `@Component struct`'s `build()`, but that
 * hop is framework-internal — no static edge — so "onClick → markAllDone →
 * this.todos = […] → rebuilt list" dead-ends at the assignment. Bridge it:
 * for each arkts struct with a `build()` method and at least one reactive
 * property, link every sibling method whose body ASSIGNS (or array-mutates)
 * one of those properties → `build`. Assignment-gated on the struct's OWN
 * reactive property names — a method that merely reads state, or a struct
 * with no reactive properties, gets nothing (this is the precision line the
 * all-sibling-methods design would erase).
 */
async function arkuiStateBuildEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const struct of queries.iterateNodesByKind('struct')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (struct.language !== 'arkts') continue;
    const children = queries.getOutgoingEdges(struct.id, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n);
    const build = children.find((n) => n.kind === 'method' && n.name === 'build');
    if (!build) continue;
    const reactiveProps = children.filter(
      (n) => n.kind === 'property' && (n.decorators ?? []).some((d) => ARKUI_REACTIVE_DECORATORS.has(d))
    );
    if (reactiveProps.length === 0) continue;
    const propAlternation = reactiveProps
      .map((p) => p.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
      .join('|');
    // `this.count = …` / `+=` / `++` / `--` / `this.todos.push(…)`. The
    // `=(?!=)` keeps `this.done == x` comparisons out.
    const mutationRe = new RegExp(
      `this\\.(?:${propAlternation})\\s*(?:=(?!=)|\\+\\+|--|[+\\-*/%&|^]=|\\.(?:${ARKUI_ARRAY_MUTATORS})\\s*\\()`
    );
    let added = 0;
    for (const m of children) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      if (m.kind !== 'method' || m.id === build.id) continue;
      const content = ctx.readFile(m.filePath);
      const src = content && sliceLines(content, m.startLine, m.endLine);
      if (!src || !mutationRe.test(stripCommentsForRegex(src, 'typescript'))) continue;
      const key = `${m.id}>${build.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: m.id, target: build.id, kind: 'calls', line: m.startLine,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'arkui-state', via: 'state assignment', registeredAt: `${build.filePath}:${build.startLine}` },
      });
      added++;
    }
  }
  return edges;
}

/** Emit/subscribe call sites of HarmonyOS's `@ohos.events.emitter` bus. */
const ARKUI_EMITTER_CALL_RE = /\bemitter\s*\.\s*(emit|on|once)\s*\(\s*([A-Za-z_$][\w$.]*|\{[^)]{0,120}?\beventId\s*:\s*[^,}]+[^)]*?\})/g;

/** Cap per event bucket — a generic key with many parties is dynamic routing, not a static pair. */
const ARKUI_EMITTER_FANOUT_CAP = 8;

/**
 * Phase 4b-ets2: HarmonyOS `@ohos.events.emitter` bridge. The cross-component
 * bus — `emitter.emit(eventId)` fires `emitter.on(eventId, cb)` — is
 * framework-internal, so an order flow riding it (OrangeShopping's
 * add-to-cart) dead-ends at the emit. Link emit-site enclosing
 * function/method → on/once-site enclosing function/method when both
 * reference the SAME statically-recoverable event key.
 *
 * Key recovery, per call site (comment-stripped enclosing-file source): the
 * first argument is an `{ eventId: K }` literal, a `Names.Dotted` constant, or
 * a local whose same-file declaration is `new EventsId(K)` / `= K` — chase one
 * level. Precision scoping learned from the samples monorepo (thousands of
 * unrelated samples, most using eventId 1): NUMERIC keys pair within the same
 * FILE only; NAMED keys pair within the same workspace module directory (or
 * the whole project when it declares no modules — the single-app case), both
 * behind a fan-out cap. Inline `on(id, (e) => {…})` arrows need no special
 * handling — their bodies' calls already attribute to the registering method,
 * so targeting that method keeps the chain connected.
 */
async function arkuiEmitterEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  interface Site { nodeId: string; file: string; line: number }
  // bucket key -> emit sites / handler sites
  const emits = new Map<string, Site[]>();
  const handlers = new Map<string, Site[]>();

  const moduleDirs = (() => {
    const ws = ctx.getWorkspacePackages?.();
    return ws ? [...new Set(ws.byName.values())].sort((a, b) => b.length - a.length) : [];
  })();
  const moduleScopeOf = (file: string): string => {
    for (const dir of moduleDirs) {
      if (file === dir || file.startsWith(dir + '/')) return dir;
    }
    return '';
  };

  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!file.endsWith('.ets')) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('emitter.')) continue;
    const safe = stripCommentsForRegex(content, 'typescript');
    const nodes = ctx.getNodesInFile(file)
      .filter((n) => n.kind === 'method' || n.kind === 'function');

    ARKUI_EMITTER_CALL_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = ARKUI_EMITTER_CALL_RE.exec(safe))) {
      const verb = m[1]!;
      const arg = m[2]!.trim();
      const line = lineOf(safe, m.index);
      const encl = nodes
        .filter((n) => n.startLine <= line && n.endLine >= line)
        .sort((a, b) => (a.endLine - a.startLine) - (b.endLine - b.startLine))[0];
      if (!encl) continue;

      // Recover the event key from the first argument.
      let key: string | null = null;
      const idLit = arg.startsWith('{') ? arg.match(/\beventId\s*:\s*([\w$.]+)/)?.[1] : undefined;
      const token = idLit ?? arg;
      if (token !== undefined) {
        if (/^\d+$/.test(token)) {
          key = `num:${file}:${token}`; // numeric: same-file only
        } else if (token.includes('.')) {
          key = `name:${moduleScopeOf(file)}:${token}`;
        } else {
          // Local variable — chase its same-file declaration one level:
          // `let x = new EventsId(K)` / `const x = K`.
          const declRe = new RegExp(
            `\\b${token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b\\s*(?::[^=\\n]+)?=\\s*(?:new\\s+[\\w$.]+\\(\\s*([^)\\n]+?)\\s*\\)|([\\w$.]+))`
          );
          const decl = safe.match(declRe);
          const inner = (decl?.[1] ?? decl?.[2])?.trim();
          if (inner && /^\d+$/.test(inner)) key = `num:${file}:${inner}`;
          else if (inner && /^[\w$.]+$/.test(inner)) key = `name:${moduleScopeOf(file)}:${inner}`;
        }
      }
      if (!key) continue;

      const site: Site = { nodeId: encl.id, file, line };
      if (verb === 'emit') {
        (emits.get(key) ?? emits.set(key, []).get(key)!).push(site);
      } else {
        (handlers.get(key) ?? handlers.set(key, []).get(key)!).push(site);
      }
    }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const [key, emitSites] of emits) {
    const handlerSites = handlers.get(key);
    if (!handlerSites) continue;
    if (emitSites.length > ARKUI_EMITTER_FANOUT_CAP || handlerSites.length > ARKUI_EMITTER_FANOUT_CAP) continue;
    const eventLabel = key.slice(key.lastIndexOf(':') + 1);
    for (const e of emitSites) for (const h of handlerSites) {
      if (e.nodeId === h.nodeId) continue;
      const dedupe = `${e.nodeId}>${h.nodeId}`;
      if (seen.has(dedupe)) continue;
      seen.add(dedupe);
      edges.push({
        source: e.nodeId, target: h.nodeId, kind: 'calls', line: e.line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'arkui-emitter', event: eventLabel, registeredAt: `${h.file}:${h.line}` },
      });
    }
  }
  return edges;
}

/** `router.pushUrl({ url: 'pages/Detail' })` / replaceUrl — literal urls only. */
const ARKUI_ROUTER_RE = /\brouter\s*\.\s*(?:pushUrl|replaceUrl)\s*\(\s*\{[^)]{0,200}?\burl\s*:\s*['"]([\w\-./]+)['"]/g;

/**
 * Phase 4b-ets3: HarmonyOS page navigation. `router.pushUrl({ url:
 * 'pages/Detail' })` reaches the `@Entry struct` of
 * `<module>/src/main/ets/pages/Detail.ets`, but the hop is a string — no
 * static edge — so "tap → openDetail → ???" ends at the router call. Bridge
 * literal urls to the page struct: the url resolves against the standard
 * `src/main/ets/` layout (what main_pages.json entries name); candidates
 * prefer the caller's own workspace module (routes are module-scoped), and
 * anything still ambiguous is dropped rather than guessed. Only `@Entry`
 * structs qualify as targets — the decorator is what makes a file a page.
 */
async function arkuiRouterEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  const edges: Edge[] = [];
  const seen = new Set<string>();

  const allFiles = ctx.getAllFiles();
  const moduleDirs = (() => {
    const ws = ctx.getWorkspacePackages?.();
    return ws ? [...new Set(ws.byName.values())].sort((a, b) => b.length - a.length) : [];
  })();
  const moduleScopeOf = (file: string): string => {
    for (const dir of moduleDirs) {
      if (file === dir || file.startsWith(dir + '/')) return dir;
    }
    return '';
  };

  let scannedFiles = 0;
  for (const file of allFiles) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!file.endsWith('.ets')) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('router.')) continue;
    const safe = stripCommentsForRegex(content, 'typescript');
    const nodes = ctx.getNodesInFile(file)
      .filter((n) => n.kind === 'method' || n.kind === 'function');

    ARKUI_ROUTER_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = ARKUI_ROUTER_RE.exec(safe))) {
      const url = m[1]!;
      const line = lineOf(safe, m.index);
      const encl = nodes
        .filter((n) => n.startLine <= line && n.endLine >= line)
        .sort((a, b) => (a.endLine - a.startLine) - (b.endLine - b.startLine))[0];
      if (!encl) continue;

      const suffix = `/src/main/ets/${url}.ets`;
      let candidates = allFiles.filter((f) => f.endsWith(suffix));
      if (candidates.length > 1) {
        const scope = moduleScopeOf(file);
        const sameModule = candidates.filter((f) => moduleScopeOf(f) === scope);
        if (sameModule.length > 0) candidates = sameModule;
      }
      if (candidates.length !== 1) continue; // ambiguous or unresolved — never guess

      const page = ctx.getNodesInFile(candidates[0]!).find(
        (n) => n.kind === 'struct' && (n.decorators ?? []).includes('Entry')
      );
      if (!page) continue;

      const key = `${encl.id}>${page.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: encl.id, target: page.id, kind: 'calls', line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'arkui-route', event: url, registeredAt: `${candidates[0]}:${page.startLine}` },
      });
    }
  }
  return edges;
}

/**
 * Phase 4c: C++ virtual override. A call through a base/interface pointer
 * (`db->Get(...)`, `iter->Next()`) dispatches at runtime to a subclass override,
 * but that hop is a vtable indirection — no static call edge — so a flow stops at
 * the abstract base method. Bridge it like react-render: for each C++ class that
 * `extends` a base, link each base method → the subclass method of the same name
 * (the override), so trace/callees from the interface method reach the
 * implementation(s). Over-approximation accepted (reachability-correct); capped
 * per class and gated to C++ to avoid touching other languages' dispatch.
 */
async function cppOverrideEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const methodsOf = (classId: string): Node[] =>
    queries
      .getOutgoingEdges(classId, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n && n.kind === 'method');
  for (const cls of queries.iterateNodesByKind('class')) {
    if ((++scanned255 & 63) === 0) await onYield();
    const subMethods = methodsOf(cls.id).filter((n) => n.language === 'cpp');
    if (subMethods.length === 0) continue;
    for (const ext of queries.getOutgoingEdges(cls.id, ['extends'])) {
      const base = queries.getNodeById(ext.target);
      if (!base || base.language !== 'cpp' || base.id === cls.id) continue;
      const baseMethods = new Map(methodsOf(base.id).map((m) => [m.name, m]));
      let added = 0;
      for (const m of subMethods) {
        if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
        const bm = baseMethods.get(m.name);
        if (!bm || bm.id === m.id) continue;
        const key = `${bm.id}>${m.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: bm.id,
          target: m.id,
          kind: 'calls',
          line: bm.startLine,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'cpp-override', via: m.name, registeredAt: `${m.filePath}:${m.startLine}` },
        });
        added++;
      }
    }
  }
  return edges;
}

/**
 * Phase 5.5: interface / abstract dispatch (Java, Kotlin). A call through an
 * injected interface (`@Autowired FooService svc; svc.list()`) or an abstract
 * base dispatches at runtime to the implementing class's override — a vtable
 * indirection with no static call edge — so a request→service flow stops at the
 * interface method. Bridge it like cpp-override: for each class that
 * `implements` an interface (or `extends` an abstract base), link each
 * base/interface method → the class's same-name method (the override) so
 * trace/callees reach the implementation. Over-approximation accepted
 * (reachability-correct); capped per class, gated to JVM languages.
 */
// Languages whose static `implements`/`extends` edges should bridge an
// interface (or abstract base) method to the matching concrete-class method.
// The set is "languages with explicit nominal subtyping and a single class
// kind that holds methods" — i.e. the shape this loop expects. Swift and
// Scala fit shape-wise (Swift `protocol`/`class`, Scala `trait`/`class`)
// and are added below; their concrete-side nodes can be a `struct` (Swift)
// or an `object` (Scala) so the loop also iterates those kinds.
const IFACE_OVERRIDE_LANGS = new Set([
  'java', 'kotlin', 'csharp', 'typescript', 'javascript', 'swift', 'scala', 'go', 'rust',
  'arkts',
]);
/** Depth cap for transitive-supertype BFS in interfaceOverrideEdges — real
 *  extends/implements chains are ≤~6; the cap is a graph-corruption backstop. */
const MAX_SUPERTYPE_DEPTH = 8;
/**
 * Go implicit interface satisfaction (#584). Go has no `implements` keyword — a
 * struct satisfies an interface structurally when its method set covers the
 * interface's. Synthesize the missing `implements` edge (struct → interface) by
 * matching method-NAME sets, so impl-navigation works and the interface-dispatch
 * bridge ({@link interfaceOverrideEdges}, now 'go'-enabled) can link an interface
 * method call to the concrete overrides.
 *
 * Name-only matching (signatures ignored) — over-approximation accepted, in line
 * with the other dispatch synthesizers; capped per interface. Empty interfaces
 * (`any`) are skipped so they don't match every struct.
 */
async function goImplementsEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();

  const methodNameSet = (id: string): Set<string> =>
    new Set(
      queries
        .getOutgoingEdges(id, ['contains'])
        .map((e) => queries.getNodeById(e.target))
        .filter((n): n is Node => !!n && n.kind === 'method')
        .map((n) => n.name),
    );

  // Materializes GO structs only (the pass is language-gated by the caller),
  // never the whole struct kind — that array is O(nodes) on struct-heavy
  // repos like the Linux kernel (#1212).
  const goStructs: Node[] = [];
  for (const s of queries.iterateNodesByKind('struct')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (s.language === 'go') goStructs.push(s);
  }
  const structMethods = new Map<string, Set<string>>();
  for (const s of goStructs) structMethods.set(s.id, methodNameSet(s.id));

  for (const iface of queries.iterateNodesByKind('interface')) {
    if ((++scanned255 & 63) === 0) await onYield();

    if ((++scanned255 & 63) === 0) await onYield();
    if (iface.language !== 'go') continue;
    const want = methodNameSet(iface.id);
    if (want.size === 0) continue; // empty interface (`any`) — would match everything
    let added = 0;
    for (const s of goStructs) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      const have = structMethods.get(s.id);
      if (!have || have.size < want.size) continue;
      let all = true;
      for (const m of want) {
        if (!have.has(m)) { all = false; break; }
      }
      if (!all) continue;
      const key = `${s.id}>${iface.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: s.id,
        target: iface.id,
        kind: 'implements',
        line: s.startLine,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'go-implements', via: iface.name, registeredAt: `${s.filePath}:${s.startLine}` },
      });
      added++;
    }
  }
  return edges;
}

/**
 * Cross-file Go method → receiver-type `contains` edges. In Go a type's methods
 * are commonly declared in a different file from the `type` declaration itself
 * (`type User struct{…}` in `user.go`, `func (u *User) Save()` in
 * `user_store.go`). Extraction attaches the struct→method `contains` edge only
 * when the receiver type is in the SAME file — the owner lookup in
 * `tree-sitter.ts` is scoped to the file being parsed — so a cross-file method
 * is left orphaned from its type (it's still `contains`ed by its file, just not
 * its struct). That breaks `codegraph_node` member outlines, any
 * callers/callees/impact traversal that goes through the type's `contains`
 * edges, and the {@link goImplementsEdges} method-set computation (which derives
 * a struct's method set from those same edges, so it under-counts interfaces a
 * cross-file struct satisfies).
 *
 * Go guarantees a method's receiver type is declared in the SAME PACKAGE as the
 * method, and a Go package is a single directory — so this is a deterministic
 * structural link, not a heuristic: find the same-named type in the method's own
 * directory and add the missing `contains` edge (no `provenance: 'heuristic'`,
 * matching the same-file edges extraction already emits). Skips methods that
 * already have a type parent (the same-file case). (#583, cross-file half)
 */
async function goCrossFileMethodContainsEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const TYPE_KINDS = new Set<NodeKind>(['struct', 'class', 'interface', 'enum', 'type_alias']);
  const dirOf = (p: string): string => {
    const i = p.replace(/\\/g, '/').lastIndexOf('/');
    return i >= 0 ? p.slice(0, i) : '';
  };

  for (const method of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();

    if ((++scanned255 & 63) === 0) await onYield();
    if (method.language !== 'go') continue;
    // The receiver type is encoded in the method's qualifiedName as `Recv::name`
    // (extraction sets `${receiverType}::${name}` for receiver methods).
    const qn = method.qualifiedName;
    if (!qn) continue;
    const sep = qn.lastIndexOf('::');
    if (sep <= 0) continue;
    const receiver = qn.slice(0, sep);
    if (!receiver) continue;

    // Already attached to its type (same-file case handled at extraction)?
    const hasTypeParent = queries
      .getIncomingEdges(method.id, ['contains'])
      .some((e) => {
        const src = queries.getNodeById(e.source);
        return src != null && TYPE_KINDS.has(src.kind);
      });
    if (hasTypeParent) continue;

    // Find the receiver type in the SAME directory (= same Go package). Go forbids
    // duplicate type names within a package, so a same-name same-dir match is
    // unambiguous; scoping to the directory avoids linking to a same-named type
    // in another package.
    const dir = dirOf(method.filePath);
    const owner = queries
      .getNodesByName(receiver)
      .find((n) => n.language === 'go' && TYPE_KINDS.has(n.kind) && dirOf(n.filePath) === dir);
    if (!owner) continue;

    const key = `${owner.id}>${method.id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push({ source: owner.id, target: method.id, kind: 'contains', line: method.startLine });
  }
  return edges;
}

/**
 * Kotlin Multiplatform `expect`/`actual` linking. A `common` source set declares
 * `expect fun foo()` / `expect class Bar`; each platform source set (jvm, native,
 * js, …) provides an `actual` implementation with the IDENTICAL fully-qualified
 * name in a different file. Callers in common code resolve to the `expect`
 * declaration, so every `actual` impl ends up with zero dependents — invisible to
 * impact/affected even though editing it can break every caller of the API.
 *
 * Synthesize a `calls` edge from the common declaration to each platform `actual`
 * (mirroring the interface-impl bridge: abstract → concrete), so editing a
 * platform impl surfaces the common `expect` and its callers, and the impl file
 * participates in the graph.
 *
 * `expect`/`actual` are captured onto the node's `decorators` list at extraction
 * (kotlin.ts `extractModifiers`). Members of an `expect class` are NOT themselves
 * keyword-marked, so the declaration side is matched as the same-FQN, same-kind
 * node that is NOT marked `actual`. Requiring an `actual`-marked counterpart also
 * gates out plain cross-file overloads (neither side is marked).
 */
// Kinds that an `expect`/`actual` pair may legitimately straddle. `expect class`
// is routinely fulfilled by an `actual typealias` (e.g. `actual typealias
// CancellationException = …`, `actual typealias SchedulerTask = Task`), so a
// strict kind match would miss those one-line alias files. Same-FQN + the
// `actual` marker already gates out unrelated symbols, so widening to the
// type-like kinds is safe.
const KMP_TYPE_KINDS = new Set(['class', 'interface', 'struct', 'enum', 'type_alias']);
function kmpKindsCompatible(a: string, b: string): boolean {
  return a === b || (KMP_TYPE_KINDS.has(a) && KMP_TYPE_KINDS.has(b));
}

async function kotlinExpectActualEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  // SQL-side language+decorator pre-filter, streamed. The old
  // `getAllNodes().filter(...)` hydrated the ENTIRE node table into one array
  // just to find kotlin `actual` declarations — on a 2M-node graph that alone
  // exceeded Node's default heap and killed the index (#1212). The LIKE
  // pre-filter can over-match (substring), so the exact decorator check stays.
  for (const act of queries.iterateNodesByLanguageWithDecorator('kotlin', 'actual')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (!act.decorators?.includes('actual')) continue;
    let added = 0;
    for (const cand of queries.getNodesByQualifiedNameExact(act.qualifiedName)) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      // The declaration side: same FQN + compatible kind, a different file, NOT
      // itself an `actual` (that would be a sibling platform impl, not the decl).
      if (cand.language !== 'kotlin' || cand.id === act.id) continue;
      if (!kmpKindsCompatible(cand.kind, act.kind) || cand.filePath === act.filePath) continue;
      if (cand.decorators?.includes('actual')) continue;
      const key = `${cand.id}>${act.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: cand.id,
        target: act.id,
        kind: 'calls',
        line: cand.startLine,
        provenance: 'heuristic',
        metadata: {
          synthesizedBy: 'kotlin-expect-actual',
          via: act.name,
          registeredAt: `${act.filePath}:${act.startLine}`,
        },
      });
      added++;
    }
  }
  return edges;
}

async function interfaceOverrideEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  // Memoized: a popular base interface's method list is otherwise re-fetched
  // once per implementer (dubbo-style hub interfaces have hundreds), and the
  // memo only ever serves reads. Same rows, same order — byte-identical.
  const methodsMemo = new Map<string, Node[]>();
  const methodsOf = (classId: string): Node[] => {
    const hit = methodsMemo.get(classId);
    if (hit) return hit;
    const methods = queries
      .getOutgoingEdges(classId, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n && n.kind === 'method');
    methodsMemo.set(classId, methods);
    return methods;
  };
  // Concrete-side kinds vary by language: `class` covers Java / Kotlin /
  // C# / TS / Swift-classes / Scala-classes; `struct` covers Swift value
  // types that conform to protocols. Iterate both.
  const concreteKinds = ['class', 'struct', 'union'] as const;
  for (const kind of concreteKinds) {
  for (const cls of queries.iterateNodesByKind(kind)) {
    if ((++scanned255 & 63) === 0) await onYield();
    // A class can only emit here if it HAS a supertype edge — check that
    // (one edge query) before materializing its methods: most classes in a
    // typical graph extend/implement nothing and skip in one hop.
    const sups = queries.getOutgoingEdges(cls.id, ['implements', 'extends']);
    if (sups.length === 0) continue;
    const implMethods = methodsOf(cls.id).filter((n) => IFACE_OVERRIDE_LANGS.has(n.language));
    if (implMethods.length === 0) continue;
    // Group impl methods by name to handle OVERLOADS: an interface `list()` and
    // `list(params)` are distinct nodes and a call may resolve to either, so
    // link every base overload → every same-name impl overload (keying by name
    // alone would drop all but one and miss the resolved overload).
    const implByName = new Map<string, Node[]>();
    for (const m of implMethods) {
      const arr = implByName.get(m.name);
      if (arr) arr.push(m); else implByName.set(m.name, [m]);
    }
    // TRANSITIVE supertypes: `A implements B`, `B extends C` → A also overrides
    // C's members (a call typed to C dispatches to A's impl). BFS over the
    // implements/extends edges, visited-guarded for diamonds/cycles and
    // depth-capped — real hierarchies are shallow, and a mid-chain node that
    // fails the language check is still traversed THROUGH (its own supertypes
    // may be valid bases).
    const supertypes: Node[] = [];
    const visited = new Set<string>([cls.id]);
    const queue: Array<{ id: string; depth: number }> = sups.map((s) => ({ id: s.target, depth: 1 }));
    while (queue.length > 0) {
      const cur = queue.shift()!;
      if (visited.has(cur.id)) continue;
      visited.add(cur.id);
      const base = queries.getNodeById(cur.id);
      if (base && base.id !== cls.id && IFACE_OVERRIDE_LANGS.has(base.language)) supertypes.push(base);
      if (cur.depth >= MAX_SUPERTYPE_DEPTH) continue;
      for (const e of queries.getOutgoingEdges(cur.id, ['implements', 'extends'])) {
        if (!visited.has(e.target)) queue.push({ id: e.target, depth: cur.depth + 1 });
      }
    }
    for (const base of supertypes) {
      let added = 0;
      for (const bm of methodsOf(base.id)) {
        if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
        for (const m of implByName.get(bm.name) ?? []) {
          if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
          if (bm.id === m.id) continue;
          const key = `${bm.id}>${m.id}`;
          if (seen.has(key)) continue;
          seen.add(key);
          edges.push({
            source: bm.id,
            target: m.id,
            kind: 'calls',
            line: bm.startLine,
            provenance: 'heuristic',
            metadata: { synthesizedBy: 'interface-impl', via: m.name, registeredAt: `${m.filePath}:${m.startLine}` },
          });
          added++;
        }
      }
    }
  }
  }
  return edges;
}

/**
 * Go gRPC stub → impl bridge. The protoc-gen-go-grpc codegen emits an
 * `UnimplementedXxxServer` struct in `*_grpc.pb.go` carrying one method
 * per service RPC; the real handler is a hand-written struct in another
 * file (`x/bank/keeper/msg_server.go::msgServer.Send` in cosmos-sdk).
 * Go's structural typing means no `implements` edge exists for our
 * resolver to follow, so `trace("Send","SendCoins")` lands on the
 * empty stub and reports "no path" (validated empirically — the cosmos
 * Q1 r1 trace failure that drove this work).
 *
 * Bridge: for each `UnimplementedXxxServer` whose RPC-method names are
 * a SUBSET of some other Go struct's method names, emit `calls` edges
 * `stub.method → impl.method` (paired by name). Excludes the gRPC
 * internal markers `mustEmbedUnimplementedXxxServer` and
 * `testEmbeddedByValue`, and skips candidate impls that themselves
 * live in a generated file (their `xxxClient` / sibling stubs would
 * otherwise look like impls).
 *
 * Multiple candidates is allowed and capped at MAX_CALLBACKS_PER_CHANNEL —
 * a service often has both a production impl and one or more test
 * mocks; linking to all preserves trace utility without false-favoring.
 *
 * Provenance: `heuristic`, `synthesizedBy: 'go-grpc-stub-impl'`. The
 * stub's source line is the wiring site shown in the trace trail.
 */
async function goGrpcStubImplEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();

  const STUB_RE = /^Unimplemented.*Server$/;
  // gRPC internal-helper methods that appear on every Unimplemented*Server;
  // not part of the service contract, so exclude when computing the RPC-method
  // signature used to match impls.
  const isInternalMarker = (n: string) => n.startsWith('mustEmbed') || n === 'testEmbeddedByValue';

  // Methods directly contained by each Go struct, name-only. Built once.
  const methodNamesByStruct = new Map<string, Set<string>>();
  const methodNodesByStruct = new Map<string, Node[]>();
  const goStructs: Node[] = [];
  for (const s of queries.iterateNodesByKind('struct')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (s.language !== 'go') continue;
    goStructs.push(s);
    const ms = queries
      .getOutgoingEdges(s.id, ['contains'])
      .map((e) => queries.getNodeById(e.target))
      .filter((n): n is Node => !!n && n.kind === 'method');
    methodNodesByStruct.set(s.id, ms);
    methodNamesByStruct.set(s.id, new Set(ms.map((m) => m.name)));
  }

  for (const stub of goStructs) {
    if (!STUB_RE.test(stub.name)) continue;
    // The stub MUST live in a generated file — that's what tells us this is
    // a protoc-emitted scaffold rather than someone naming a struct
    // `UnimplementedXxxServer` by hand. Without this gate we'd also bridge
    // such hand-written structs and create misleading edges.
    if (!isGeneratedFile(stub.filePath)) continue;

    const stubMethods = (methodNodesByStruct.get(stub.id) ?? []).filter(
      (m) => !isInternalMarker(m.name),
    );
    if (stubMethods.length === 0) continue;
    const stubMethodNames = stubMethods.map((m) => m.name);

    for (const cand of goStructs) {
      if (cand.id === stub.id) continue;
      // Skip generated-file candidates — they're siblings (msgClient,
      // UnsafeMsgServer, …) whose method sets coincidentally match.
      if (isGeneratedFile(cand.filePath)) continue;

      const candNames = methodNamesByStruct.get(cand.id);
      if (!candNames) continue;
      // Subset: every RPC method must exist on the candidate by name.
      // Signature-level match would tighten this further, but name-match
      // alone already gives one-to-one pairing in real codebases because
      // gRPC method-name sets are highly distinctive (Send + MultiSend +
      // UpdateParams + SetSendEnabled is unique to bank's MsgServer).
      if (!stubMethodNames.every((n) => candNames.has(n))) continue;

      const candMethods = methodNodesByStruct.get(cand.id) ?? [];
      let added = 0;
      for (const sm of stubMethods) {
        if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
        for (const cm of candMethods) {
          if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
          if (cm.name !== sm.name) continue;
          const key = `${sm.id}>${cm.id}`;
          if (seen.has(key)) continue;
          seen.add(key);
          edges.push({
            source: sm.id,
            target: cm.id,
            kind: 'calls',
            line: sm.startLine,
            provenance: 'heuristic',
            metadata: {
              synthesizedBy: 'go-grpc-stub-impl',
              via: cm.name,
              registeredAt: `${cm.filePath}:${cm.startLine}`,
            },
          });
          added++;
        }
      }
    }
  }
  return edges;
}

/** Kinds a JSX tag can name. A tag that resolves only to a type is markup we drop. */
const JSX_CHILD_KINDS = new Set<NodeKind>(['component', 'function', 'class']);

/**
 * The languages a JSX tag can plausibly name a component in. Preferred over a
 * same-named symbol in another language, never required — a React Native tag
 * whose only match is the native class it bridges to (`requireNativeComponent`)
 * still links there.
 */
const JSX_CHILD_LANGUAGES = [...JS_FAMILY, 'vue', 'svelte'];

/** `localName` → the project file it is imported from, for one file's imports. */
function importedFrom(ctx: ResolutionContext, file: string, language: Language): Map<string, string> {
  const out = new Map<string, string>();
  for (const im of ctx.getImportMappings(file, language)) {
    // The mappings name the module as written; the file it is comes from the
    // same resolution the import resolver uses (aliases, extensions, index files).
    const resolved = im.resolvedPath ?? resolveImportPath(im.source, file, language, ctx);
    if (resolved) out.set(im.localName, resolved);
  }
  return out;
}

/**
 * The component a JSX tag names, among every node that shares the name.
 *
 * A tag is written in one file, and that file already says which `FrameCard` it
 * means: the one it declares, or the one it imports. Taking the FIRST node of
 * that name — which is what this did — is a coin flip once a name repeats, and
 * it costs twice over. The parent gets an edge to a component it never renders,
 * and the component it does render is left with no caller at all, so every walk
 * back from that subtree dead-ends: on an Expo app whose `<FrameCard/>` was one
 * of two, the folder sheet's `openBackgroundNoiseDetail` stood alone on Screens
 * with no screen behind it, while the edge pointed at an unrelated card in
 * another sheet.
 *
 * Same file first — a small component declared beside its use is the commonest
 * shape, and the one an import can never disambiguate. Then the file the name
 * is imported from. Then the language, which only decides a tie: a `.tsx` tag
 * naming both a TS component and a same-named Swift class means the TS one.
 */
function jsxChild(
  ctx: ResolutionContext,
  name: string,
  file: string,
  importsOf: () => Map<string, string>
): Node | undefined {
  const candidates = ctx.getNodesByName(name).filter((n) => JSX_CHILD_KINDS.has(n.kind));
  if (candidates.length <= 1) return candidates[0];
  const local = candidates.find((n) => n.filePath === file);
  if (local) return local;
  const from = importsOf().get(name);
  if (from) {
    const imported = candidates.find((n) => n.filePath === from);
    if (imported) return imported;
  }
  return candidates.find((n) => JSX_CHILD_LANGUAGES.includes(n.language)) ?? candidates[0];
}

/**
 * Phase 5: React JSX child rendering. A component that returns `<Child .../>`
 * mounts Child — React calls it — but JSX instantiation isn't a static call edge,
 * so a render tree (App.render → StaticCanvas → renderStaticScene) breaks at the
 * JSX hop. Link parent → each capitalized JSX child it renders. File-oriented
 * (read each JSX file once). Precision gate: the child name must resolve to a
 * component/function/class node — TS generics like `Array<Foo>` resolve to a type
 * (or nothing) and are dropped.
 */
async function reactJsxChildEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const PARENT_KINDS = new Set(['method', 'function', 'component']);
  let scanned = 0;
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if ((++scanned & 255) === 0) await onYield(); // #1091: yield mid-scan on huge graphs
    const content = ctx.readFile(file);
    if (!content || (!content.includes('</') && !content.includes('/>'))) continue; // JSX-file gate
    // File-level language gate, not merely a project-level one: mixed C/JS
    // monorepos must not interpret `"<Foo/>"` inside C as JSX (#1560).
    const parents = ctx.getNodesInFile(file).filter(
      (n) => PARENT_KINDS.has(n.kind) && JS_FAMILY.includes(n.language)
    );
    if (parents.length === 0) continue;
    // Read once per file, and only when a name actually turns out ambiguous.
    let imports: Map<string, string> | null = null;
    const importsOf = () =>
      (imports ??= importedFrom(ctx, file, parents[0]!.language));
    for (const parent of parents) {
      const src = sliceLines(content, parent.startLine, parent.endLine);
      if (!src || (!src.includes('</') && !src.includes('/>'))) continue;
      const names = new Set<string>();
      JSX_TAG_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = JSX_TAG_RE.exec(src))) names.add(m[1]!);
      let added = 0;
      for (const name of names) {
        if (added >= MAX_JSX_CHILDREN) break;
        const child = jsxChild(ctx, name, file, importsOf);
        if (!child || child.id === parent.id) continue;
        const key = `${parent.id}>${child.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: parent.id, target: child.id, kind: 'calls', line: parent.startLine,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'jsx-render', via: name },
        });
        added++;
      }
    }
  }
  return edges;
}

/**
 * Phase 6: Vue SFC templates. The `.vue` extractor only parses `<script>`, so
 * template usage is invisible — child components and event handlers used ONLY in
 * the template have no edge to them. PascalCase children (`<VPNav/>`) are already
 * caught by reactJsxChildEdges (which scans the SFC component node), so this adds
 * the two Vue-specific shapes:
 *   - kebab-case children: `<el-button>` → `ElButton` component (renders).
 *   - event bindings: `@click="onClick"` / `v-on:submit="save"` → handler method.
 * Scoped to the `<template>` block of `.vue` files; resolution gate (kebab→
 * component, handler→function/method) keeps precision; inline arrows / `$emit`
 * skipped.
 */
async function vueTemplateEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const COMPONENT_KINDS = new Set(['component', 'function', 'class']);
  const HANDLER_KINDS = new Set(['method', 'function']);
  // A composable's returned member may be a fn (`function close(){}`) or an
  // arrow assigned to a const (`const close = () => {}`).
  const RETURN_KINDS = new Set(['method', 'function', 'variable', 'constant']);
  // Nuxt auto-imports nested components by a DIRECTORY-PREFIXED name —
  // `components/media/Card.vue` is used as `<MediaCard/>`, not `<Card/>` — but
  // the component node is named by basename (`Card`), so a direct tag match
  // misses it (flat components match by basename and don't need this). Map each
  // nested component's Nuxt name → node so those template usages resolve.
  const nuxtComponents = new Map<string, Node>();
  for (const c of (ctx.iterateNodesByKind?.('component') ?? ctx.getNodesByKind('component'))) {
    if ((++scanned255 & 63) === 0) await onYield();
    const nn = nuxtComponentName(c.filePath);
    if (nn && !nuxtComponents.has(nn)) nuxtComponents.set(nn, c);
  }

  // Global `app.component('name', Comp)` registrations, collected once. The
  // createApp/vueApp/Vue.component gate keeps a `.component(` call on an
  // unrelated object from registering; the value must resolve to a real
  // component (or a lazy `import()` of a `.vue` file) or the entry is inert.
  const globalRegistrations = new Map<string, Node>();
  const regComponent = (value: string, lazyPath: string | undefined, fromFile: string): Node | undefined => {
    if (lazyPath) {
      const target = resolveImportPath(lazyPath, fromFile, 'typescript', ctx);
      return target ? ctx.getNodesInFile(target).find((n) => n.kind === 'component') : undefined;
    }
    const candidates = ctx.getNodesByName(value).filter((n) => COMPONENT_KINDS.has(n.kind));
    return candidates.length === 1 ? candidates[0] : undefined;
  };
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!/\.(?:vue|[cm]?[jt]s)$/.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('.component(') || !VUE_APP_GATE_RE.test(content)) continue;
    VUE_APP_COMPONENT_RE.lastIndex = 0;
    let am: RegExpExecArray | null;
    while ((am = VUE_APP_COMPONENT_RE.exec(content))) {
      const target = regComponent(am[2]!, am[3], file);
      if (target && !globalRegistrations.has(am[1]!)) globalRegistrations.set(am[1]!, target);
    }
  }

  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!file.endsWith('.vue')) continue;
    const content = ctx.readFile(file);
    const tpl = content && content.match(/<template[^>]*>([\s\S]*)<\/template>/i)?.[1];
    if (!tpl) continue;
    const comp = ctx.getNodesInFile(file).find((n) => n.kind === 'component');
    if (!comp) continue;

    // Composable-destructure map: alias → { composable, key }. Lets us resolve a
    // template handler that isn't a local function but a destructured composable
    // return (`@click="closeSidebar"` ← `const { close: closeSidebar } = useSidebarControl()`).
    const script = content.match(/<script[^>]*>([\s\S]*?)<\/script>/i)?.[1] ?? '';
    const destructured = new Map<string, { composable: string; key: string }>();
    VUE_DESTRUCTURE_RE.lastIndex = 0;
    let dm: RegExpExecArray | null;
    while ((dm = VUE_DESTRUCTURE_RE.exec(script))) {
      if (!/^use[A-Z]/.test(dm[2]!)) continue; // composables / hooks only
      for (const part of dm[1]!.split(',')) {
        const pm = part.trim().match(/^(\w+)\s*(?::\s*(\w+))?$/); // key | key: alias
        if (pm) destructured.set(pm[2] || pm[1]!, { composable: dm[2]!, key: pm[1]! });
      }
    }

    let added = 0;
    const addEdge = (target: Node | undefined, meta: Record<string, unknown>) => {
      if (added >= MAX_JSX_CHILDREN || !target || target.id === comp.id) return;
      const k = `${comp.id}>${target.id}>${meta.synthesizedBy}`;
      if (seen.has(k)) return;
      seen.add(k);
      edges.push({ source: comp.id, target: target.id, kind: 'calls', line: comp.startLine, provenance: 'heuristic', metadata: meta });
      added++;
    };
    // Prefer a target in THIS SFC (handlers live in the same file's script) —
    // avoids cross-file mis-match when a name repeats across a monorepo.
    const resolve = (name: string, kinds: Set<string>): Node | undefined => {
      const matches = ctx.getNodesByName(name).filter((n) => kinds.has(n.kind));
      return matches.find((n) => n.filePath === file) ?? matches[0];
    };

    // This SFC's own `components: { 'my-comp': MyComp, Other }` registrations
    // (options API). The registered KEY is the tag's real binding — the value
    // may be any resolvable component or a lazy `() => import('./X.vue')`.
    const localRegistrations = new Map<string, Node>();
    VUE_COMPONENTS_BLOCK_RE.lastIndex = 0;
    let cb: RegExpExecArray | null;
    while ((cb = VUE_COMPONENTS_BLOCK_RE.exec(script))) {
      const open = cb.index + cb[0].length - 1;
      const close = matchBalanced(script, open);
      if (close === -1) continue;
      const block = script.slice(open, close + 1);
      VUE_COMP_ENTRY_RE.lastIndex = 0;
      let rm: RegExpExecArray | null;
      while ((rm = VUE_COMP_ENTRY_RE.exec(block))) {
        const key = rm[2] ?? rm[5] ?? rm[8];
        const lazyPath = rm[3] ?? rm[6];
        const value = rm[4] ?? rm[7] ?? rm[8];
        if (!key) continue;
        const target = regComponent(value!, lazyPath, file);
        if (target && !localRegistrations.has(key)) localRegistrations.set(key, target);
      }
    }
    // The registered name binds before a PascalCase guess — Vue resolves a tag
    // through its registry (raw / camel / Pascal / kebab spellings), so
    // `components: { 'my-comp': Special }` makes `<my-comp>` Special even when
    // a `MyComp` component exists.
    const registered = (tag: string): Node | undefined => {
      for (const v of vueTagVariants(tag)) {
        const t = localRegistrations.get(v) ?? globalRegistrations.get(v);
        if (t) return t;
      }
      return undefined;
    };

    let m: RegExpExecArray | null;
    VUE_KEBAB_RE.lastIndex = 0;
    while ((m = VUE_KEBAB_RE.exec(tpl))) {
      const tag = kebabToPascal(m[1]!);
      addEdge(
        registered(m[1]!) ?? resolve(tag, COMPONENT_KINDS) ?? nuxtComponents.get(tag),
        { synthesizedBy: 'jsx-render', via: m[1] }
      );
    }
    // PascalCase component tags. Try a direct name match first (flat components
    // and explicit registrations), then the Nuxt dir-prefixed auto-import name
    // (`<MediaCard>` → components/media/Card.vue). Built-ins match neither → no edge.
    VUE_PASCAL_RE.lastIndex = 0;
    while ((m = VUE_PASCAL_RE.exec(tpl))) {
      const tag = m[1]!;
      addEdge(
        registered(tag) ?? resolve(tag, COMPONENT_KINDS) ?? nuxtComponents.get(tag),
        { synthesizedBy: 'jsx-render', via: tag }
      );
    }
    VUE_HANDLER_RE.lastIndex = 0;
    while ((m = VUE_HANDLER_RE.exec(tpl))) {
      const event = m[1]!;
      const expr = m[2]!.trim();
      if (expr.includes('=>') || expr.startsWith('$')) continue; // inline arrow / $emit
      const name = expr.match(/^([A-Za-z_]\w*)/)?.[1];
      if (!name) continue;
      const direct = resolve(name, HANDLER_KINDS);
      if (direct) { addEdge(direct, { synthesizedBy: 'vue-handler', event }); continue; }
      // Composable-destructure handler → resolve to the composable's returned fn.
      const d = destructured.get(name);
      if (!d) continue;
      const composable = resolve(d.composable, HANDLER_KINDS);
      // Resolve to the SPECIFIC returned member (e.g. `close`) defined in the
      // composable's file. No fallback to the composable itself — the component
      // already has a static `useX()` call edge, so that would just be redundant
      // and less precise.
      const keyFn = composable
        ? ctx.getNodesByName(d.key).find((n) => RETURN_KINDS.has(n.kind) && n.filePath === composable.filePath)
        : undefined;
      if (keyFn) addEdge(keyFn, { synthesizedBy: 'vue-handler', event, via: d.composable });
    }
  }
  return edges;
}

/**
 * React Native cross-language event channel (Phase 3 of the mixed-iOS/RN
 * bridging effort). Same shape as `eventEmitterEdges` but cross-language:
 *
 *   Native (ObjC, on RCTEventEmitter subclass):
 *     [self sendEventWithName:@"locationUpdate" body:@{...}];
 *
 *   Native (Java/Kotlin, via the JS module dispatcher):
 *     emitter.emit("locationUpdate", body);
 *     reactContext.getJSModule(RCTDeviceEventEmitter.class).emit("locationUpdate", body);
 *
 *   JS (subscriber):
 *     new NativeEventEmitter(NativeModules.Geo).addListener("locationUpdate", handler);
 *     DeviceEventEmitter.addListener("locationUpdate", handler);
 *
 * Synthesize: native dispatch site → JS handler, keyed by the literal
 * event name. A NAMED handler (`addListener('x', handleX)`) is the target
 * when it is a node; an unnamed one — a parameter passed through, or an
 * inline `(data) => {…}` written in a `useEffect` — is attributed to the
 * enclosing function, where the event demonstrably lands. (The in-language
 * synthesizer stays named-only; this channel pairs across a language
 * boundary on a literal, which is the evidence that makes the wider
 * attribution safe.)
 *
 * Provenance `'heuristic'`, synthesizedBy `'rn-event-channel'`.
 */
// ObjC's `[self sendEventWithName:@"X" body:...]` shape (bracket syntax,
// `@` string literals).
const RN_OBJC_SEND_RE = /\bsendEventWithName\s*:\s*@"([^"]+)"/g;
// Swift's `sendEvent(withName: "X", body: ...)` shape — same RCTEventEmitter
// method, different call syntax. Both Objective-C and Swift subclass
// RCTEventEmitter so this catches the Swift-side equivalent emission sites
// (e.g. RNFusedLocation.swift's `sendEvent(withName: "geolocationDidChange",
// body: locationData)`).
const RN_SWIFT_SEND_RE = /\bsendEvent\s*\(\s*withName\s*:\s*"([^"]+)"/g;
// JVM-side emitter calls: `emitter.emit("X", body)`. Matches both Java
// and Kotlin syntax because the call form is identical. Restricted to
// JVM source files in the consumer so we don't re-process JS emits
// (which `eventEmitterEdges` already handles).
const RN_JVM_EMIT_RE = /\.emit\s*\(\s*"([^"]+)"\s*,/g;
// Custom `sendEvent(reactContext, "X", body)` wrapper — extremely common
// (react-native-device-info and many libs wrap `DeviceEventManagerModule…emit`
// behind a helper whose `.emit(eventName, …)` uses a VARIABLE, so RN_JVM_EMIT_RE
// misses it; the literal lives in the wrapper CALL instead). Captures the first
// string literal inside a `sendEvent(...)` call. `[^;{}]*?` keeps it on one
// statement and stops at a block boundary, so the wrapper DEFINITION (whose `(`
// is followed by `… ) {`) never matches. Multi-line tolerant. (java/kotlin/swift)
const RN_NATIVE_SENDEVENT_RE = /\bsendEvent\s*\([^;{}]*?"([^"]+)"/g;

async function rnEventEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // Native dispatchers (source = the native method whose body sends the
  // event) and JS handlers (target = the function/method registered as
  // the listener) keyed by event name.
  const nativeDispatchersByEvent = new Map<string, Set<string>>();
  const jsHandlersByEvent = new Map<string, Map<string, string>>();

  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    const content = ctx.readFile(file);
    if (!content) continue;

    const nodesInFile = ctx.getNodesInFile(file);
    const lineOf = makeLineAt(content, 1);
    const addDispatcher = (event: string, line: number) => {
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) return;
      const set = nativeDispatchersByEvent.get(event) ?? new Set<string>();
      set.add(disp.id);
      nativeDispatchersByEvent.set(event, set);
    };

    // ObjC side: `sendEventWithName:@"X"` only fires inside `.m`/`.mm`
    // files (RCTEventEmitter subclasses).
    if (file.endsWith('.m') || file.endsWith('.mm')) {
      RN_OBJC_SEND_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = RN_OBJC_SEND_RE.exec(content))) {
        if (m[1]) addDispatcher(m[1], lineOf(m.index));
      }
    }

    // Swift side: same RCTEventEmitter method, parens/named-args syntax.
    if (file.endsWith('.swift')) {
      RN_SWIFT_SEND_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = RN_SWIFT_SEND_RE.exec(content))) {
        if (m[1]) addDispatcher(m[1], lineOf(m.index));
      }
      RN_NATIVE_SENDEVENT_RE.lastIndex = 0;
      while ((m = RN_NATIVE_SENDEVENT_RE.exec(content))) {
        if (m[1]) addDispatcher(m[1], lineOf(m.index));
      }
    }

    // JVM side: `.emit("X", …)` in Java/Kotlin, plus the common
    // `sendEvent(ctx, "X", body)` wrapper. (We pattern-match anywhere in the
    // file; the JS in-language path uses a separate emitter object pattern and
    // is already handled by eventEmitterEdges.)
    if (file.endsWith('.java') || file.endsWith('.kt')) {
      let m: RegExpExecArray | null;
      RN_JVM_EMIT_RE.lastIndex = 0;
      while ((m = RN_JVM_EMIT_RE.exec(content))) {
        if (m[1]) addDispatcher(m[1], lineOf(m.index));
      }
      RN_NATIVE_SENDEVENT_RE.lastIndex = 0;
      while ((m = RN_NATIVE_SENDEVENT_RE.exec(content))) {
        if (m[1]) addDispatcher(m[1], lineOf(m.index));
      }
    }

    // JS subscribers (.addListener("X", handler)). Restrict to JS-family
    // files so a native file's `addListener:` (the ObjC method) doesn't
    // get mistaken for a JS subscription — they're entirely different
    // things despite sharing a name.
    if (
      file.endsWith('.js') ||
      file.endsWith('.jsx') ||
      file.endsWith('.ts') ||
      file.endsWith('.tsx') ||
      file.endsWith('.mjs') ||
      file.endsWith('.cjs')
    ) {
      // Match BOTH the named-handler form (`.addListener('x', fn)`) and
      // an unnamed-handler form (`.addListener('x', listener)` where
      // `listener` is a parameter — common in RN wrapper APIs like
      // RNFirebase's `messaging().onMessageReceived(listener)`). For the
      // unnamed case we attribute the subscription to the ENCLOSING JS
      // function (the abstraction layer), giving a reachability-correct
      // hop even when the actual user-side handler lives one call up.
      const ADDLISTENER_ANY = /\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*([A-Za-z_][\w.]*)/g;
      // The inline form: `.addListener('x', (data) => {…})` / `function () {…}`.
      const ADDLISTENER_INLINE =
        /\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:async\s*)?(?:\([^)]*\)\s*=>|[A-Za-z_$][\w$]*\s*=>|function\s*\()/g;
      ADDLISTENER_ANY.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = ADDLISTENER_ANY.exec(content))) {
        const event = m[1];
        const arg = m[2];
        if (!event || !arg) continue;
        const bareName = arg.includes('.') ? arg.slice(arg.lastIndexOf('.') + 1) : arg;
        // Try a named-symbol match first (matches the in-language semantic).
        const namedHandler = ctx
          .getNodesByName(bareName)
          .find((n) => n.kind === 'function' || n.kind === 'method');
        let targetId: string | null = namedHandler?.id ?? null;
        if (!targetId) {
          // Fall back to the enclosing function — the subscribe-wrapper
          // pattern means the event fires THROUGH this function on its
          // way to user code. Reachability-correct attribution.
          const enclosing = enclosingFn(nodesInFile, lineOf(m.index));
          targetId = enclosing?.id ?? null;
        }
        if (!targetId) {
          // Broader fallback for JS object-literal API shape
          // (`const Foo = { watchX(...) { … addListener(...) … } }`):
          // method shorthand inside an object literal isn't extracted
          // as a method node, so enclosingFn returns null. Attribute to
          // the smallest enclosing `constant` / `variable` node — that's
          // the API surface a downstream caller would `import` and
          // invoke. Reachability-correct.
          const line = lineOf(m.index);
          let smallest: typeof nodesInFile[number] | null = null;
          for (const n of nodesInFile) {
            if (n.kind !== 'constant' && n.kind !== 'variable') continue;
            const end = n.endLine ?? n.startLine;
            if (n.startLine <= line && end >= line) {
              if (!smallest || n.startLine >= smallest.startLine) smallest = n;
            }
          }
          targetId = smallest?.id ?? null;
        }
        if (!targetId) continue;
        const map = jsHandlersByEvent.get(event) ?? new Map<string, string>();
        map.set(targetId, `${file}:${lineOf(m.index)}`);
        jsHandlersByEvent.set(event, map);
      }
      // Inline listeners — the shape a React component registers inside a
      // `useEffect`: `nativeEmitter.addListener('onCaptureComplete', (data) =>
      // { … router.push('/review') })`. There is no handler symbol to name,
      // so the subscription is attributed to the enclosing function exactly
      // as the unnamed-argument form above is: the native event lands in that
      // component, and its body is where a reader looks next. This channel
      // only — the in-language synthesizer keeps its named-handler policy.
      ADDLISTENER_INLINE.lastIndex = 0;
      while ((m = ADDLISTENER_INLINE.exec(content))) {
        const event = m[1];
        if (!event) continue;
        const enclosing = enclosingFn(nodesInFile, lineOf(m.index));
        if (!enclosing) continue;
        const map = jsHandlersByEvent.get(event) ?? new Map<string, string>();
        if (!map.has(enclosing.id)) map.set(enclosing.id, `${file}:${lineOf(m.index)}`);
        jsHandlersByEvent.set(event, map);
      }
    }
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const [event, dispatchers] of nativeDispatchersByEvent) {
    const handlers = jsHandlersByEvent.get(event);
    if (!handlers) continue;
    // Same fan-out guard as the in-language channel: generic event names
    // (e.g. 'change', 'error', 'data') with many handlers/dispatchers
    // can't be matched precisely without receiver-type info.
    if (dispatchers.size > EVENT_FANOUT_CAP || handlers.size > EVENT_FANOUT_CAP) continue;
    for (const d of dispatchers) {
      for (const [h, registeredAt] of handlers) {
        if (d === h) continue;
        const key = `${d}>${h}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: d,
          target: h,
          kind: 'calls',
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'rn-event-channel', event, registeredAt },
        });
      }
    }
  }
  return edges;
}

/**
 * React Native dynamic module-key callsites: `NativeModules[key].method()`
 * where `key` is statically known — a string literal
 * (`NativeModules['Capture'].start()`) or a same-file constant initialized
 * to a string literal (`const KEY = 'Capture'; NativeModules[KEY].start()`).
 * A subscript receiver emits no call reference, so these callsites never
 * reach the bridge resolver — this pass scans source for the pattern and
 * links the enclosing function to the module's native implementations
 * (every platform's impl is a real bridge target).
 *
 * `NativeModules[opaqueExpr]` stays uncovered: an unresolvable key could
 * name ANY module — no statically verifiable anchor, so we skip rather
 * than fan out to every bridged method of that name.
 */
const RN_DYNAMIC_MODULE_RE =
  /\bNativeModules\s*\[\s*(?:['"]([A-Za-z_$][\w$]*)['"]|([A-Za-z_$][\w$]*))\s*\]\s*\.\s*([A-Za-z_$][\w$]*)\s*\(/g;
const RN_MODULE_NAME_RE = /^[A-Za-z_$][\w$]*$/;

async function rnDynamicModuleEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!/\.(?:[cm]?[jt]sx?)$/.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('NativeModules[')) continue;
    const nodesInFile = ctx.getNodesInFile(file);
    const lineOf = makeLineAt(content, 1);
    RN_DYNAMIC_MODULE_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = RN_DYNAMIC_MODULE_RE.exec(content))) {
      let moduleName: string | null = m[1] ?? null;
      const keyIdent = m[2];
      const method = m[3]!;
      if (!moduleName && keyIdent) {
        // Same-file `const KEY = 'Module'` — collect every literal binding of
        // the identifier in this file; two distinct values mean the key is
        // not statically known (conditional rebinding) → skip.
        const decl = new RegExp(
          `\\b(?:const|let|var)\\s+${keyIdent.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*(?::[^=;]*)?=\\s*['"]([^'"]+)['"]`,
          'g'
        );
        const values = new Set<string>();
        let dm: RegExpExecArray | null;
        while ((dm = decl.exec(content))) values.add(dm[1]!);
        if (values.size === 1) moduleName = [...values][0]!;
      }
      if (!moduleName || !RN_MODULE_NAME_RE.test(moduleName)) continue;
      const disp =
        enclosingFn(nodesInFile, lineOf(m.index)) ?? enclosingValue(nodesInFile, lineOf(m.index));
      if (!disp) continue;
      for (const target of rnModuleMethods(ctx, moduleName, method)) {
        if (target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'rn-dynamic-module', module: moduleName, via: method },
        });
      }
    }
  }
  return edges;
}

/**
 * Phase 6 — React Native Fabric/Codegen view component bridge.
 *
 * The Fabric framework extractor (`frameworks/fabric.ts`) emits
 * `component` nodes named after the JS-visible component (e.g.
 * `RNSScreenStack`) from each `codegenNativeComponent<Props>('Name')`
 * spec declaration. The native implementation lives in an ObjC++/.mm or
 * Kotlin/Java class whose name follows one of RN's conventions:
 *
 *   - Exact: `RNSScreenStack`
 *   - With suffix: `RNSScreenStackView`, `RNSScreenStackViewManager`,
 *     `RNSScreenStackComponentView`, `RNSScreenStackManager`
 *
 * This synthesizer walks every Fabric component node and looks for a
 * native class matching one of those names; when found, emits a
 * `calls` edge `component → native class` (provenance `'heuristic'`,
 * `synthesizedBy:'fabric-native-impl'`) so trace from JSX usage of the
 * component continues into native.
 *
 * The convention-based suffix lookup is precise: there's no name
 * collision in RN view-manager codebases by design (Codegen output would
 * conflict otherwise).
 */
const FABRIC_NATIVE_SUFFIXES = ['', 'View', 'ViewManager', 'ComponentView', 'Manager'];

/**
 * Expo Modules cross-platform pairing. An Expo Module exposes the SAME
 * JS-visible method (`AsyncFunction("getBatteryLevelAsync")`) from BOTH an iOS
 * (Swift) and an Android (Kotlin) implementation. A JS callsite name-resolves to
 * only ONE of them, so the other platform's impl looked like nothing called it
 * (and editing it showed no blast radius). Link the iOS and Android impls of the
 * same `<module>.<method>` to each other (both directions), so a JS call that
 * reaches one platform reaches the other, and editing either surfaces the JS
 * caller. The Expo method nodes are id-prefixed `expo-module:` and qualified
 * `<file>::<module>.<method>` by the framework extractor.
 */
async function expoCrossPlatformEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const byKey = new Map<string, Node[]>();
  for (const m of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (!m.id.startsWith('expo-module:')) continue;
    const key = m.qualifiedName.split('::').pop(); // `<module>.<method>`
    if (!key) continue;
    const arr = byKey.get(key);
    if (arr) arr.push(m);
    else byKey.set(key, [m]);
  }
  for (const group of byKey.values()) {
    if (group.length < 2) continue;
    for (const a of group) {
      for (const b of group) {
        if (a.id === b.id || a.language === b.language) continue; // cross-platform only
        const key = `${a.id}>${b.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: a.id,
          target: b.id,
          kind: 'calls',
          line: a.startLine,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'expo-cross-platform', via: a.name },
        });
      }
    }
  }
  return edges;
}

/**
 * Classic React Native NativeModules cross-platform pairing. A native module
 * method (`@ReactMethod` on Android, `RCT_EXPORT_METHOD` on iOS) is implemented
 * on BOTH platforms, but a JS callsite name-resolves to only ONE — so the other
 * platform's impl looked like nothing called it. A native method that HAS a JS
 * caller is a confirmed bridge method; link it to the same-named native method
 * in another language (the other platform's impl) so a JS call reaching one
 * platform reaches the other, and editing either surfaces the JS caller.
 *
 * Names are normalized to the first selector keyword (`getFreeDiskStorage:` →
 * `getFreeDiskStorage`) — that's the JS-visible name, and how the iOS selector
 * lines up with the bare Android method name.
 */
async function rnCrossPlatformEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const NATIVE = new Set(['java', 'kotlin', 'objc', 'cpp']);
  const JS = new Set(['typescript', 'tsx', 'javascript', 'jsx']);
  // RN module INFRASTRUCTURE methods exist on every native module (called by the
  // RN runtime, not user JS), so pairing them by name would cross-link unrelated
  // modules in a multi-module repo. Skip them — they aren't user-facing methods.
  const RN_INFRA = new Set([
    'addListener', 'removeListeners', 'getConstants', 'constantsToExport', 'getName',
    'invalidate', 'initialize', 'getDefaultEventTypes', 'supportedEvents',
    'requiresMainQueueSetup', 'methodQueue',
  ]);
  const norm = (name: string): string => {
    const i = name.indexOf(':');
    return i >= 0 ? name.slice(0, i) : name;
  };

  // Index native methods by their JS-visible (normalized) name. Only names with
  // impls in ≥2 native languages can pair, so the per-method JS-caller check
  // below only runs for genuine cross-platform candidates.
  const byName = new Map<string, Node[]>();
  for (const m of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (!NATIVE.has(m.language)) continue;
    const key = norm(m.name);
    const arr = byName.get(key);
    if (arr) arr.push(m);
    else byName.set(key, [m]);
  }

  for (const [groupName, group] of byName) {
    if (RN_INFRA.has(groupName)) continue;
    const langs = new Set(group.map((m) => m.language));
    if (langs.size < 2) continue; // single-platform — nothing to pair
    for (const m of group) {
      // Is m a bridge method? (a JS-language `calls` edge points at it)
      const incoming = queries.getIncomingEdges(m.id, ['calls']);
      if (incoming.length === 0) continue;
      const sources = queries.getNodesByIds(incoming.map((e) => e.source));
      const isBridge = incoming.some((e) => {
        const s = sources.get(e.source);
        return !!s && JS.has(s.language);
      });
      if (!isBridge) continue;
      // Link to the other-platform impls (both directions).
      for (const sib of group) {
        if (sib.id === m.id || sib.language === m.language) continue;
        for (const [a, b] of [[m, sib], [sib, m]] as const) {
          const key = `${a.id}>${b.id}`;
          if (seen.has(key)) continue;
          seen.add(key);
          edges.push({
            source: a.id,
            target: b.id,
            kind: 'calls',
            line: a.startLine,
            provenance: 'heuristic',
            metadata: { synthesizedBy: 'rn-cross-platform', via: norm(m.name) },
          });
        }
      }
    }
  }
  return edges;
}

async function fabricNativeImplEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();

  // The Fabric extractor IDs are prefixed `fabric-component:` so we can
  // filter to just those while streaming — never materializing the whole
  // `component` kind (#1212).
  const components: Node[] = [];
  for (const n of (ctx.iterateNodesByKind?.('component') ?? ctx.getNodesByKind('component'))) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (n.id.startsWith('fabric-component:')) components.push(n);
  }
  if (components.length === 0) return edges;

  // Pre-index native classes by name for O(1) lookup.
  const nativeClassesByName = new Map<string, Node[]>();
  for (const n of (ctx.iterateNodesByKind?.('class') ?? ctx.getNodesByKind('class'))) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (n.language !== 'objc' && n.language !== 'kotlin' && n.language !== 'java' && n.language !== 'cpp') continue;
    const arr = nativeClassesByName.get(n.name);
    if (arr) arr.push(n);
    else nativeClassesByName.set(n.name, [n]);
  }

  for (const component of components) {
    for (const suffix of FABRIC_NATIVE_SUFFIXES) {
      const candidate = component.name + suffix;
      const matches = nativeClassesByName.get(candidate);
      if (!matches || matches.length === 0) continue;
      // Link the component node to every matching native class (iOS +
      // Android each have one).
      for (const native of matches) {
        const key = `${component.id}>${native.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: component.id,
          target: native.id,
          kind: 'calls',
          provenance: 'heuristic',
          metadata: {
            synthesizedBy: 'fabric-native-impl',
            viaSuffix: suffix || '(exact)',
            componentName: component.name,
          },
        });
      }
    }
  }

  return edges;
}

/**
 * MyBatis: link a Java mapper interface method to the XML statement that holds
 * its SQL. The XML extractor (`src/extraction/mybatis-extractor.ts`) qualifies
 * each `<select|insert|update|delete|sql id="X">` as `<namespace>::<id>` where
 * `<namespace>` is the Java FQN of the mapper interface. A Java method's
 * qualifiedName ends with `<ClassName>::<methodName>`, so we suffix-match the
 * last two segments of the XML qualified name to find a unique Java method by
 * `<ClassName>::<methodName>` (`ClassName` = last dotted segment of the XML
 * namespace). Cross-mapper `<include refid="other.X">` references go through
 * the normal qualified-name resolver — only the Java↔XML bridge is synthetic.
 *
 * Precision over recall: ambiguous mappers (multiple Java classes with the
 * same simple name) are dropped. We need-not bridge by package because Java
 * mapper interfaces are typically uniquely named within a project.
 */
async function mybatisJavaXmlEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  // Collect the XML side FIRST (mapper `<select id=…>` statements extracted as
  // xml-language method nodes): if the project has none — every Java+XML repo
  // that doesn't use MyBatis — return before paying the full java-method
  // stream below. Same rowid stream order as matching inline, so the edge
  // output is byte-identical when mappers do exist.
  const xmlMethods: Node[] = [];
  for (const m of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (m.language === 'xml') xmlMethods.push(m);
  }
  if (xmlMethods.length === 0) return edges;

  // Index Java methods by `<ClassName>::<methodName>` for O(1) lookup.
  const javaIndex = new Map<string, Node[]>();
  for (const m of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (m.language !== 'java' && m.language !== 'kotlin') continue;
    const parts = m.qualifiedName.split('::');
    const last = parts[parts.length - 1];
    const cls = parts[parts.length - 2];
    if (!last || !cls) continue;
    const key = `${cls}::${last}`;
    const arr = javaIndex.get(key);
    if (arr) arr.push(m); else javaIndex.set(key, [m]);
  }

  for (const xml of xmlMethods) {
    if ((++scanned255 & 63) === 0) await onYield();
    // Qualified name: `<namespace>::<id>`. Extract the simple class name.
    const colonIdx = xml.qualifiedName.lastIndexOf('::');
    if (colonIdx < 0) continue;
    const namespace = xml.qualifiedName.slice(0, colonIdx);
    const id = xml.qualifiedName.slice(colonIdx + 2);
    if (!namespace || !id) continue;
    const dotIdx = namespace.lastIndexOf('.');
    const className = dotIdx >= 0 ? namespace.slice(dotIdx + 1) : namespace;
    const candidates = javaIndex.get(`${className}::${id}`);
    if (!candidates || candidates.length === 0) continue;
    // Drop ambiguous matches (multiple same-name classes); the user can
    // disambiguate by adding the package-suffix match in a future enhancement.
    if (candidates.length > 1) continue;
    const java = candidates[0]!;
    const key = `${java.id}>${xml.id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push({
      source: java.id,
      target: xml.id,
      kind: 'calls',
      line: java.startLine,
      provenance: 'heuristic',
      metadata: {
        synthesizedBy: 'mybatis-java-xml',
        via: `${className}.${id}`,
        registeredAt: `${xml.filePath}:${xml.startLine}`,
      },
    });
  }
  return edges;
}

/**
 * Gin middleware chain. Gin runs its entire handler chain through one dynamic
 * line in `(*Context).Next`:
 *     for c.index < len(c.handlers) { c.handlers[c.index](c); c.index++ }
 * `c.handlers` is a `HandlersChain` (`[]HandlerFunc`) assembled at registration
 * time by `combineHandlers` from the funcs passed to `r.Use(...)` /
 * `r.GET("/path", h...)` / `r.Handle(...)`. Because the call is a computed index
 * into a runtime-built slice, tree-sitter resolves `c.handlers[c.index](c)` to
 * NOTHING — so `callees(Next)` is just the `len()` helper and the flow
 * `ServeHTTP → handleHTTPRequest → Next` dead-ends at the exact symbol the
 * "how do requests flow through the middleware chain" question is about. The
 * agent then re-queries Next and falls back to Read/grep (validated: the gin
 * WITH-arm rabbit-holed on precisely this dead-end).
 *
 * Bridge it: find the chain DISPATCHER (a Go method whose body invokes a
 * `handlers` slice by index) and link it → every HandlerFunc registered via a
 * gin registration call, so `callees(Next)` and `trace(ServeHTTP, <handler>)`
 * connect end-to-end. Named handlers only (`gin.Logger()` → `Logger`,
 * `authMiddleware`); inline closures are anonymous and skipped. Like
 * react-render / interface-impl this is a deliberate over-approximation —
 * reachability-correct (any registered handler CAN run for some route), capped,
 * and gated on the dispatcher existing so it never runs on non-gin Go repos.
 * Provenance `heuristic`, `synthesizedBy:'gin-middleware-chain'`; `registeredAt`
 * is the `.Use`/`.GET` site an agent would otherwise grep for.
 */
const GIN_DISPATCH_RE = /\.handlers\s*\[[^\]]*\]\s*\(/;                 // c.handlers[c.index](c)
const GIN_REG_RE = /\.(?:Use|GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\s*\(/g;

/** Balanced `(...)` body starting at the '(' index; null if unbalanced. */
function goBalancedArgs(s: string, openIdx: number): string | null {
  let depth = 0;
  for (let i = openIdx; i < s.length; i++) {
    const c = s[i];
    if (c === '(') depth++;
    else if (c === ')') { depth--; if (depth === 0) return s.slice(openIdx + 1, i); }
  }
  return null;
}
/** Split a top-level comma list, respecting nested () [] {}. */
function goSplitArgs(args: string): string[] {
  const out: string[] = [];
  let depth = 0, cur = '';
  for (const c of args) {
    if (c === '(' || c === '[' || c === '{') { depth++; cur += c; }
    else if (c === ')' || c === ']' || c === '}') { depth--; cur += c; }
    else if (c === ',' && depth === 0) { out.push(cur); cur = ''; }
    else cur += c;
  }
  if (cur.trim()) out.push(cur);
  return out;
}
/** Tail ident of a handler arg: `gin.Logger()`→`Logger`, `mw`→`mw`; null for string paths / closures. */
function goHandlerIdent(expr: string): string | null {
  const cleaned = expr.trim().replace(/\(\s*\)$/, '');                  // drop a trailing call ()
  if (!cleaned || cleaned.startsWith('"') || cleaned.startsWith('`') || cleaned.startsWith('func')) return null;
  const m = cleaned.match(/(?:\.|^)([A-Za-z_]\w*)$/);
  return m ? m[1]! : null;
}

async function ginMiddlewareChainEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  let scannedFiles = 0;
  // 1. Find the chain dispatcher(s): a Go method that invokes a `handlers` slice by index.
  const dispatchers: Node[] = [];
  for (const n of queries.iterateNodesByKind('method')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (n.language !== 'go') continue;
    const content = ctx.readFile(n.filePath);
    const src = content && sliceLines(content, n.startLine, n.endLine);
    if (src && GIN_DISPATCH_RE.test(src)) dispatchers.push(n);
  }
  if (dispatchers.length === 0) return [];                              // not a gin repo — bail

  // 2. Collect handler identifiers registered via gin registration calls
  //    (.Use / .GET / … / .Handle). String args (paths/methods) and inline
  //    closures are dropped by goHandlerIdent; the rest are HandlerFuncs.
  const registered = new Map<string, string>();                         // name → registeredAt (file:line)
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!file.endsWith('.go')) continue;
    const content = ctx.readFile(file);
    if (!content || (!content.includes('.Use(') && !/\.(?:GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\(/.test(content))) continue;
    const safe = stripCommentsForRegex(content, 'go');
    GIN_REG_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = GIN_REG_RE.exec(safe))) {
      const parenIdx = m.index + m[0].length - 1;
      const argStr = goBalancedArgs(safe, parenIdx);
      if (!argStr) continue;
      const line = lineOf(safe, m.index);
      for (const arg of goSplitArgs(argStr)) {
        const name = goHandlerIdent(arg);
        if (name && !registered.has(name)) registered.set(name, `${file}:${line}`);
      }
    }
  }
  if (registered.size === 0) return [];

  // 3. Link each dispatcher → each registered handler node (dedup, capped).
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const disp of dispatchers) {
    let added = 0;
    for (const [name, registeredAt] of registered) {
      if (added >= MAX_CALLBACKS_PER_CHANNEL) break;
      const handler = ctx.getNodesByName(name).find(
        (n) => (n.kind === 'function' || n.kind === 'method') && n.language === 'go'
      );
      if (!handler || handler.id === disp.id) continue;
      const key = `${disp.id}>${handler.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id, target: handler.id, kind: 'calls', line: disp.startLine,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'gin-middleware-chain', via: name, registeredAt },
      });
      added++;
    }
  }
  return edges;
}

/**
 * Delphi form code-behind: a form unit `UFRMAbout.pas` owns its visual form
 * definition `UFRMAbout.dfm` (VCL) / `.fmx` (FireMonkey) — paired by basename in
 * the same directory, wired by the `{$R *.dfm}` directive rather than a `uses`
 * clause. Link the unit → its form so a `.dfm`/`.fmx` used only as a form
 * definition isn't orphaned, and editing the form surfaces its code-behind unit.
 */
async function pascalFormEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const edges: Edge[] = [];
  const allFiles = new Set(ctx.getAllFiles());
  for (const file of allFiles) {
    if ((++scannedFiles & 255) === 0) await onYield();
    if (!/\.(dfm|fmx)$/i.test(file)) continue;
    const pasFile = file.replace(/\.(dfm|fmx)$/i, '.pas');
    if (!allFiles.has(pasFile)) continue;
    const formNode = ctx.getNodesInFile(file).find((n) => n.kind === 'file');
    const unitNode = ctx.getNodesInFile(pasFile).find((n) => n.kind === 'file');
    if (!formNode || !unitNode) continue;
    edges.push({
      source: unitNode.id,
      target: formNode.id,
      kind: 'references',
      line: unitNode.startLine,
      provenance: 'heuristic',
      metadata: { synthesizedBy: 'pascal-form', registeredAt: pasFile },
    });
  }
  return edges;
}

/**
 * SvelteKit file-convention data flow. A route directory's `+page.svelte` (a
 * `component` node) receives its `data` from the sibling `+page.server.{ts,js}`
 * / `+page.{ts,js}` `load` function and posts forms to its `actions` — wired by
 * the framework BY FILE PATH, with no static import between them. So editing a
 * `load` shows no impact on the page it feeds, and the page looks like it has no
 * server-side dependency. Link the page component to its sibling loader's
 * `load` / `actions` (same for `+layout`). The pairing is path-deterministic
 * (same directory, matching `+page`/`+layout` prefix), so it's precise — but
 * it's a framework-convention edge, so provenance stays `heuristic`.
 *
 * Direction: page → load, so `getImpactRadius(load)` surfaces the page (editing
 * a loader's data shows the page it feeds) and the page's dependencies include
 * its loader.
 */
async function svelteKitLoadEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const edges: Edge[] = [];
  const allFiles = new Set(ctx.getAllFiles());
  const HOOKS = new Set(['load', 'actions']);
  const HOOK_KINDS = new Set(['function', 'method', 'constant', 'variable']);
  for (const file of allFiles) {
    if ((++scannedFiles & 255) === 0) await onYield();
    const m = file.match(/(.*\/)(\+(?:page|layout))\.svelte$/);
    if (!m) continue;
    const dir = m[1]!;
    const prefix = m[2]!;
    const page = ctx.getNodesInFile(file).find((n) => n.kind === 'component');
    if (!page) continue;
    for (const ext of ['.server.ts', '.server.js', '.ts', '.js']) {
      const loaderFile = `${dir}${prefix}${ext}`;
      if (!allFiles.has(loaderFile)) continue;
      for (const hook of ctx.getNodesInFile(loaderFile)) {
        // `load` and `actions` by name, and every function the loader file
        // declares — a form action is an arrow inside `actions`, and it is a
        // node of its own (`default`, `logout`), where the redirect that ends
        // the submission is actually written.
        const named = HOOK_KINDS.has(hook.kind) && HOOKS.has(hook.name);
        if (!named && hook.kind !== 'function' && hook.kind !== 'method') continue;
        edges.push({
          source: page.id,
          target: hook.id,
          kind: 'calls',
          line: page.startLine,
          provenance: 'heuristic',
          metadata: {
            synthesizedBy: 'sveltekit-load',
            via: hook.name,
            registeredAt: `${loaderFile}:${hook.startLine ?? 0}`,
          },
        });
      }
    }
  }
  return edges;
}

/**
 * Redux-thunk dispatch chain. `export const X = createAsyncThunk(prefix, async (a, api) => {...})`
 * (or a wrapper like trezor's `createThunk(...)`) passes the async body as an ARGUMENT, so
 * tree-sitter never extracts it as a function node: `X` is a `constant` whose body's calls are
 * ORPHANED. The `dispatch(nextThunk(...))` calls that drive a thunk chain forward therefore produce
 * no edges, so `callees(X)` is empty and a flow `dispatch(X(...)) → X → nextThunk` dead-ends at the
 * constant (validated on trezor-suite: the signXxxThunk constants had ZERO outgoing edges). Bridge
 * it: body-scan each thunk constant for `dispatch(Y(...))` and link `X → Y`, so the dispatch chain
 * connects. High-precision — the `dispatch(` keyword plus `Y` must resolve to a function/constant/
 * method node; capped; gated on thunk constants existing so it never runs on non-RTK repos.
 * Cross-file by design (a suite thunk dispatches a wallet-core thunk). Provenance `heuristic`,
 * `synthesizedBy:'redux-thunk'`; `registeredAt` is the dispatch site.
 */
const THUNK_DECL_RE = /create(?:Async)?Thunk/;
const THUNK_DISPATCH_RE = /\bdispatch\s*\(\s*([A-Za-z_]\w*)\s*[(),]/g;
const THUNK_FANOUT_CAP = 24;

async function reduxThunkEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const node of queries.iterateNodesByKind('constant')) {
    if ((++scanned255 & 63) === 0) await onYield();
    // Cheap gate: the initializer (captured in `signature`) must be a create(Async)Thunk call —
    // avoids reading every constant's body on a large repo.
    if (!node.signature || !THUNK_DECL_RE.test(node.signature)) continue;
    const content = ctx.readFile(node.filePath);
    const src = content && sliceLines(content, node.startLine, node.endLine);
    if (!src) continue;
    // Thunks are TS/JS-family (same // and /* */ comment syntax); map to a CommentLang.
    const safe = stripCommentsForRegex(src, node.language === 'javascript' || node.language === 'jsx' ? 'javascript' : 'typescript');
    THUNK_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    let added = 0;
    while ((m = THUNK_DISPATCH_RE.exec(safe)) && added < THUNK_FANOUT_CAP) {
      const name = m[1]!;
      if (name === node.name) continue; // self-dispatch (recursive thunk) — skip
      // Resolve the dispatched name, PREFERRING the thunk/action-creator over a same-named
      // service function. `dispatch(X(...))` dispatches a thunk or an action-creator (both
      // `constant`s) — never an unrelated helper that merely shares the name. On octo-call,
      // `leaveCall` is BOTH a `createAsyncThunk` const AND a service function, and the bare
      // `.find()` picked the function (wrong). Order: thunk const > other const > same-file
      // callable > first match. A single candidate (no collision) is unaffected.
      const cands = ctx
        .getNodesByName(name)
        .filter((n) => n.kind === 'constant' || n.kind === 'function' || n.kind === 'method');
      const target =
        cands.find((n) => !!n.signature && THUNK_DECL_RE.test(n.signature)) ??
        cands.find((n) => n.kind === 'constant') ??
        cands.find((n) => n.filePath === node.filePath) ??
        cands[0];
      if (!target || target.id === node.id) continue;
      const key = `${node.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const line = node.startLine + lineOf(safe, m.index) - 1;
      edges.push({
        source: node.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'redux-thunk', via: name, registeredAt: `${node.filePath}:${line}` },
      });
      added++;
    }
  }
  return edges;
}

// ── Object-literal registry dispatch ─────────────────────────────────────────
// A command/handler registry maps string keys → handler class/function symbols in an
// object literal, then dispatches by a RUNTIME key static parsing can't follow:
//   this.commands = { [Cmd.ADD]: AddObjectCommand, ... }    // registration
//   new this.commands[command](args).execute()              // dynamic dispatch
// Bridge it like gin-middleware-chain: link each dispatching function → each registered
// handler's callable entry (a class's execute/run/handle/… method — preferring the method
// chained at the dispatch site — or the function value). Scoped to a registry + dispatch in
// the SAME file (the cross-file barrel-namespace variant, e.g. trezor's getMethod, is
// deferred). Gated on a real object literal with ≥2 entries that RESOLVE to callables (a
// `{ width: 5 }` literal resolves to nothing → no edges); fan-out capped.
const REGISTRY_ASSIGN_RE = /(?:(?:const|let|var)\s+([A-Za-z_$][\w$]*)|((?:this\.)?[A-Za-z_$][\w$]*))\s*=\s*\{/g;
const REGISTRY_DISPATCH_RE = /(?:\bnew\s+)?((?:this\.)?[A-Za-z_$][\w$]*)\s*\[\s*([A-Za-z_$][\w$.]*)\s*\]\s*(?:\(|\.[A-Za-z_$])/g;
// `const r = registry` — an alias the dispatch ref may be written through.
// RHS must be a BARE ref (a `reg[k]` element grab is a different shape).
const REGISTRY_ALIAS_RE = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*((?:this\.)?[A-Za-z_$][\w$]*)\s*(?:;|\n|$)/g;
// `registry['k'] = fn` / `registry[KEY] = fn` / `registry.k = fn` — entries
// added after the literal. RHS is a bare/qualified ident (`fn`, `ns.fn`,
// `this.fn`); a call/arrow/object RHS fails the trailing `;` gate.
const REGISTRY_AUGMENT_RE =
  /((?:this\.)?[A-Za-z_$][\w$]*)\s*(?:\[\s*(['"])([\w$]+)\2\s*\]|\[\s*([A-Za-z_$][\w$.]*)\s*\]|\.\s*([A-Za-z_$][\w$]*))\s*=(?!=)\s*([A-Za-z_$][\w$.]*)\s*;/g;
// `r['k'](` — a LITERAL-key access through an alias: statically resolvable
// when 'k' is a known registry key (declared or augmented), and missed by the
// main dispatch regex on purpose (a quoted key is a static access).
const REGISTRY_ALIAS_LITERAL_RE =
  /(?:\bnew\s+)?((?:this\.)?[A-Za-z_$][\w$]*)\s*\[\s*(['"])([\w$]+)\2\s*\]\s*(?:\(|\.[A-Za-z_$])/g;
// `const v = reg[keyExpr]` — assign-then-call: `v` binds a COMPUTED registry
// lookup and `v(…)` later is the dispatch call (warp-drive's
// `const cmd = COMMANDS[cmdString]; await cmd(args)`). `const` only — a
// `let`/`var` may be reassigned between lookup and call. The statement must
// END at the access (`;`/newline): `v = reg[k].load()` binds a member of the
// entry, a different shape. A literal `reg['k']` RHS resolves to that one key's
// handler instead of fanning out.
const REGISTRY_COMPUTED_ALIAS_RE =
  /\bconst\s+([A-Za-z_$][\w$]*)\s*=\s*(?:await\s+)?((?:this\.)?[A-Za-z_$][\w$]*)\s*\[\s*(?:([A-Za-z_$][\w$]*)|(['"])([\w$]+)\4)\s*\]\s*(?:;|\n|$)/g;
const REGISTRY_MIN_ENTRIES = 2;
const REGISTRY_FANOUT_CAP = 40;
const REGISTRY_CLASS_ENTRY = new Set(['execute', 'run', 'handle', 'perform', 'process', 'call', 'apply', 'dispatch']);
const REGISTRY_JS_EXT = /\.(?:ts|tsx|js|jsx|mjs|cjs)$/;

/** From the index of an opening `{`, return the brace-balanced body up to its matching `}`. */
function braceBody(src: string, openIdx: number): string | null {
  let depth = 0;
  for (let i = openIdx; i < src.length; i++) {
    if (src[i] === '{') depth++;
    else if (src[i] === '}' && --depth === 0) return src.slice(openIdx + 1, i);
  }
  return null;
}

/** Top-level `key: Identifier` entries of an object-literal body. DEPTH-AWARE: only depth-0
 *  segments are considered, so method-shorthand bodies (`number(a,b){…}`), arrow values
 *  (`x: () => …`), and nested objects (`x: { … }`) don't leak their inner `k: v` pairs as
 *  bogus handlers. The per-segment anchor (`^… key: Ident …$`) keeps only pure identifier
 *  values — a data value (`x: 5`), call, or arrow fails to match. */
function registryEntryNames(body: string): { names: string[]; literalKeys: Map<string, string> } {
  const segs: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c === '{' || c === '(' || c === '[') depth++;
    else if (c === '}' || c === ')' || c === ']') depth--;
    else if (c === ',' && depth === 0) { segs.push(body.slice(start, i)); start = i + 1; }
  }
  segs.push(body.slice(start));
  const names: string[] = [];
  const literalKeys = new Map<string, string>();
  for (const seg of segs) {
    const m = /^\s*(?:\[[^\]]+\]|(['"]?)([\w$]+)\1)\s*:\s*([A-Za-z_$][\w$]*)\s*$/.exec(seg);
    if (m && m[3]!.length >= 3 && !names.includes(m[3]!)) {
      names.push(m[3]!);
      // `[COMPUTED]` keys hold a runtime value we can't spell, so only plain
      // `k:` / `'k':` / `"k":` entries become literal-key lookup targets.
      if (!seg.trimStart().startsWith('[')) literalKeys.set(m[2]!, m[3]!);
    }
  }
  return { names, literalKeys };
}

/** Resolve a registered handler name to its callable entry: a function value, or a class's
 *  `execute`-like method (preferring the method chained at the dispatch site), else the class. */
function resolveRegistryHandler(ctx: ResolutionContext, name: string, chained: string | null): Node | null {
  const cands = ctx.getNodesByName(name);
  const fn = cands.find((n) => n.kind === 'function');
  if (fn) return fn;
  const cls = cands.find((n) => n.kind === 'class' || n.kind === 'struct');
  if (cls) {
    const methods = ctx
      .getNodesInFile(cls.filePath)
      .filter((n) => n.kind === 'method' && n.startLine >= cls.startLine && n.startLine <= (cls.endLine ?? cls.startLine));
    const want = chained && REGISTRY_CLASS_ENTRY.has(chained) ? chained : null;
    const entry =
      (want && methods.find((m) => m.name === want)) ||
      methods.find((m) => REGISTRY_CLASS_ENTRY.has(m.name)) ||
      methods.find((m) => m.name === 'constructor');
    return entry ?? cls;
  }
  // Require a CALLABLE target — a registry dispatched as `reg[k](…)` invokes a function/
  // method, never a data `constant` (dropping it removes false positives like a `{ x: URL }`
  // entry resolving to the global URL constant).
  return cands.find((n) => n.kind === 'method') ?? null;
}

async function objectRegistryEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  let scanned = 0;
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if ((++scanned & 255) === 0) await onYield(); // #1091: yield mid-scan on huge graphs
    if (!REGISTRY_JS_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    // Cheap pre-filter: a member access BY NAME (`ident[ident` or `ident['…'`)
    // — the var-dispatch and alias-literal shapes respectively.
    if (!content || !/[\w$]\s*\[\s*[A-Za-z_$'"]/.test(content)) continue;
    // Skip minified/generated bundles (draco, three.min, base64…): their pervasive `h[x](...)`
    // calls + single-letter `{a:b}` literals are a false-positive minefield. Average line
    // length is the reliable tell — real source ~30–80, minified in the hundreds/thousands.
    const newlines = (content.match(/\n/g)?.length ?? 0) + 1;
    if (content.length / newlines > 200) continue;
    const safe = stripCommentsForRegex(content, /\.(?:jsx?|mjs|cjs)$/.test(file) ? 'javascript' : 'typescript');

    // 1. Dispatch sites: `(new )?<ref>[<ident-key>]` followed by a call or a chained method.
    //    A quoted-string key (`['save']`) does NOT match — that's a static access, not dispatch.
    REGISTRY_DISPATCH_RE.lastIndex = 0;
    const dispatches: Array<{ ref: string; line: number; chained: string | null }> = [];
    let dm: RegExpExecArray | null;
    while ((dm = REGISTRY_DISPATCH_RE.exec(safe))) {
      const win = safe.slice(dm.index, dm.index + 160);
      const cm = /\]\s*\([^)]*\)\s*\.\s*([A-Za-z_$][\w$]*)/.exec(win) || /\]\s*\.\s*([A-Za-z_$][\w$]*)/.exec(win);
      dispatches.push({ ref: dm[1]!, line: lineOf(safe, dm.index), chained: cm ? cm[1]! : null });
    }
    // Literal-key accesses through an alias (`r['k'](…)`) — collected up front
    // so a file with ONLY these (no `r[k]` var dispatch) still proceeds.
    const literalAccesses: Array<{ ref: string; key: string; pos: number }> = [];
    REGISTRY_ALIAS_LITERAL_RE.lastIndex = 0;
    let lm: RegExpExecArray | null;
    while ((lm = REGISTRY_ALIAS_LITERAL_RE.exec(safe))) {
      literalAccesses.push({ ref: lm[1]!, key: lm[3]!, pos: lm.index });
    }
    // Assign-then-call: `const v = reg[key]` binds a computed lookup; `v(…)`
    // later is the dispatch. The ref joins the dispatched set so its literal
    // registers below; `v(` call sites are emitted after registries exist.
    const computedAliases: Array<{ var: string; ref: string; litKey: string | null; pos: number }> = [];
    REGISTRY_COMPUTED_ALIAS_RE.lastIndex = 0;
    let cm2: RegExpExecArray | null;
    while ((cm2 = REGISTRY_COMPUTED_ALIAS_RE.exec(safe))) {
      computedAliases.push({ var: cm2[1]!, ref: cm2[2]!, litKey: cm2[5] ?? null, pos: cm2.index });
    }
    if (!dispatches.length && !literalAccesses.length && !computedAliases.length) continue;
    // Normalize a leading `this.` so a class FIELD-INITIALIZER registry (`commands = {…}`)
    // matches a `this.commands[k]` dispatch, not just the constructor form `this.commands = {…}`.
    const norm = (r: string) => r.replace(/^this\./, '');
    // Aliases: `const r = registry` — the dispatch may be written through the
    // alias (`r[k]`). Collected unconditionally (the RHS needn't be a known
    // registry — an unresolvable alias simply never matches), then resolved
    // transitively (`const b = a; const a = reg`).
    const aliasOf = new Map<string, string>();
    REGISTRY_ALIAS_RE.lastIndex = 0;
    let al: RegExpExecArray | null;
    while ((al = REGISTRY_ALIAS_RE.exec(safe))) aliasOf.set(al[1]!, norm(al[2]!));
    const canon = (r: string): string => {
      let x = r;
      const seen2 = new Set<string>();
      while (aliasOf.has(x) && !seen2.has(x)) { seen2.add(x); x = aliasOf.get(x)!; }
      return x;
    };
    const refs = new Set([
      ...computedAliases.map((a) => canon(norm(a.ref))),
      ...dispatches.map((d) => canon(norm(d.ref))),
      ...literalAccesses.map((l) => canon(norm(l.ref))),
    ]);

    // 2. Registries: an object literal assigned to a dispatched ref, ≥2 entries resolving to callables.
    REGISTRY_ASSIGN_RE.lastIndex = 0;
    const registries = new Map<string, { names: string[]; literalKeys: Map<string, string>; line: number }>();
    let am: RegExpExecArray | null;
    while ((am = REGISTRY_ASSIGN_RE.exec(safe))) {
      const lhs = norm(am[1] ?? am[2]!);
      if (!refs.has(lhs) || registries.has(lhs)) continue;
      const body = braceBody(safe, am.index + am[0].length - 1);
      if (!body) continue;
      const { names, literalKeys } = registryEntryNames(body); // depth-0 `key: Identifier` entries only
      if (names.length >= REGISTRY_MIN_ENTRIES) {
        registries.set(lhs, { names, literalKeys, line: lineOf(safe, am.index) });
      }
    }
    if (!registries.size) continue;

    // 2b. Post-declaration augmentation: `registry['k'] = fn` / `registry[KEY] = fn`
    //     / `registry.k = fn` — entries appended after the literal (Prebid's
    //     single-entry `modules['x'] = fn` pattern). Only augments a registry
    //     the file already dispatches through.
    REGISTRY_AUGMENT_RE.lastIndex = 0;
    let ug: RegExpExecArray | null;
    while ((ug = REGISTRY_AUGMENT_RE.exec(safe))) {
      const reg = registries.get(canon(norm(ug[1]!)));
      if (!reg) continue;
      const rhsTail = ug[6]!.split('.').pop()!;
      if (rhsTail.length >= 3 && !reg.names.includes(rhsTail)) reg.names.push(rhsTail);
      const litKey = ug[3] ?? ug[5];
      if (litKey) reg.literalKeys.set(litKey, rhsTail);
    }

    // 3. Link each dispatcher → each registered handler's callable entry.
    const nodesInFile = ctx.getNodesInFile(file);
    for (const d of dispatches) {
      const reg = registries.get(canon(norm(d.ref)));
      if (!reg) continue;
      const disp = enclosingFn(nodesInFile, d.line);
      if (!disp) continue;
      let added = 0;
      for (const name of reg.names) {
        if (added >= REGISTRY_FANOUT_CAP) break;
        const target = resolveRegistryHandler(ctx, name, d.chained);
        if (!target || target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line: d.line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'object-registry', via: name, registeredAt: `${file}:${reg.line}` },
        });
        added++;
      }
    }

    // 4. Literal-key access THROUGH AN ALIAS: `const r = registry; r['k'](…)`
    //    — the main regex skips quoted keys (they're static accesses) and the
    //    resolver can't see through the const alias, so this is a genuinely
    //    missing edge. Only fires when 'k' is a KNOWN key of the resolved
    //    registry (declared literal or augmented) — an unknown key is skipped.
    for (const la of literalAccesses) {
      const refName = norm(la.ref);
      if (!aliasOf.has(refName)) continue; // aliases only — reg['k'] is the resolver's job
      const reg = registries.get(canon(refName));
      if (!reg) continue;
      const handler = reg.literalKeys.get(la.key);
      if (!handler) continue;
      const line = lineOf(safe, la.pos);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const win = safe.slice(la.pos, la.pos + 160);
      const cm = /\]\s*\([^)]*\)\s*\.\s*([A-Za-z_$][\w$]*)/.exec(win) || /\]\s*\.\s*([A-Za-z_$][\w$]*)/.exec(win);
      const target = resolveRegistryHandler(ctx, handler, cm ? cm[1]! : null);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: {
          synthesizedBy: 'object-registry',
          via: handler,
          key: la.key,
          registeredAt: `${file}:${reg.line}`,
        },
      });
    }

    // 5. Assign-then-call: `const v = reg[key]` binds the lookup, `v(…)` later
    //    dispatches. A literal `reg['k']` RHS resolves to that one handler; a
    //    dynamic `reg[ident]` fans out to all entries (same as `reg[k](…)`).
    for (const ca of computedAliases) {
      const reg = registries.get(canon(norm(ca.ref)));
      if (!reg) continue;
      // The const's own scope: a fn-scoped alias bridges only calls in the same
      // function after the assignment (a `v(` in another fn is a shadow, not
      // the lookup); a module-level alias stays open to any later call.
      const assignLine = lineOf(safe, ca.pos);
      const aliasScope = enclosingFn(nodesInFile, assignLine);
      const callRe = new RegExp(`(^|[^\\w$])${ca.var.replace(/\$/g, '\\$')}\\s*\\(`, 'g');
      let cl: RegExpExecArray | null;
      while ((cl = callRe.exec(safe))) {
        const varStart = cl.index + cl[1]!.length;
        if (varStart <= ca.pos) continue; // uses can't precede the const (TDZ)
        // Skip `function v(` declarations — only a bare `v(` use is a dispatch.
        if (/function\s*$/.test(safe.slice(Math.max(0, varStart - 12), varStart))) continue;
        const line = lineOf(safe, varStart);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp || (aliasScope && disp.id !== aliasScope.id)) continue;
        const names = ca.litKey !== null ? [reg.literalKeys.get(ca.litKey)].filter((x): x is string => !!x) : reg.names;
        for (const name of names.slice(0, REGISTRY_FANOUT_CAP)) {
          const target = resolveRegistryHandler(ctx, name, null);
          if (!target || target.id === disp.id) continue;
          const key = `${disp.id}>${target.id}`;
          if (seen.has(key)) continue;
          seen.add(key);
          edges.push({
            source: disp.id,
            target: target.id,
            kind: 'calls',
            line,
            provenance: 'heuristic',
            metadata: {
              synthesizedBy: 'object-registry',
              via: name,
              ...(ca.litKey !== null ? { key: ca.litKey } : {}),
              registeredAt: `${file}:${reg.line}`,
            },
          });
        }
      }
    }
  }
  return edges;
}

// ── RTK Query generated-hook → endpoint ──────────────────────────────────────
// RTK Query generates one `useGetXQuery`/`useUpdateYMutation` hook per endpoint
// (`createApi({ endpoints: b => ({ getX: b.query(...) }) })`). Components call the
// hook; the fetch logic lives in the endpoint's queryFn. The hook↔endpoint link is
// pure NAMING CONVENTION (no static edge): strip `use` + the optional `Lazy`
// variant + the `Query|Mutation` suffix, lowercase the head → the endpoint key.
// Both are extracted as function nodes (the hook from its `export const {…}=api`
// binding, carrying a sentinel signature; the endpoint from the createApi object),
// so bridging hook→endpoint connects `component → useGetXQuery → getX → queryFn`.
// Gated on the extraction sentinel so it only ever fires on genuinely-generated
// hooks (never a hand-written `useFooQuery`), and on a SAME-FILE endpoint (RTK
// colocates the hooks and their api in one module) — 0 on any non-RTK repo.
const RTK_HOOK_DERIVE_RE = /^use([A-Z][A-Za-z0-9]*?)(?:Query|Mutation)$/;
// MUST match the signature set in tree-sitter.ts `extractRtkHookBindings`.
const RTK_GENERATED_HOOK_SIGNATURE = '= RTK Query generated hook';

/** Derive the endpoint key from a generated-hook name (`useLazyGetRecordsQuery`
 *  → `getRecords`), or null if it doesn't fit the convention. */
function rtkEndpointNameFromHook(hook: string): string | null {
  const m = RTK_HOOK_DERIVE_RE.exec(hook);
  if (!m) return null;
  let mid = m[1]!;
  if (mid.startsWith('Lazy')) mid = mid.slice(4); // useLazyGetX → getX (same endpoint)
  if (!mid) return null;
  return mid.charAt(0).toLowerCase() + mid.slice(1);
}

async function rtkQueryEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const hook of queries.iterateNodesByKind('function')) {
    if ((++scanned255 & 63) === 0) await onYield();
    // Only our extracted generated-hook bindings (sentinel) — not a real hook fn.
    if (hook.signature !== RTK_GENERATED_HOOK_SIGNATURE) continue;
    const endpointName = rtkEndpointNameFromHook(hook.name);
    if (!endpointName) continue;
    // The endpoint is a same-file function by the derived name (RTK colocates the
    // api definition and its generated-hook exports in one module).
    const target = ctx
      .getNodesByName(endpointName)
      .find((n) => n.kind === 'function' && n.filePath === hook.filePath);
    if (!target || target.id === hook.id) continue;
    const key = `${hook.id}>${target.id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push({
      source: hook.id,
      target: target.id,
      kind: 'calls',
      line: hook.startLine,
      provenance: 'heuristic',
      metadata: { synthesizedBy: 'rtk-query', via: endpointName, registeredAt: `${hook.filePath}:${hook.startLine}` },
    });
  }
  return edges;
}

// ── Pinia useStore().action() dispatch bridge ────────────────────────────────
// A Pinia store factory `export const useXStore = defineStore(...)` exposes its
// actions as methods on the store instance; a consumer does `const s = useXStore()`
// then `s.action()`. The call is a method-on-instance with no static edge to the
// action (which lives in the store's module). Bridge it: map each factory → its
// file, bind `const <var> = useXStore()` per consumer file, and link the enclosing
// function → the `<var>.method()` action node IN THE STORE'S FILE. The same-store-
// file gate keeps it precise (a Pinia built-in like `$patch` or an unrelated
// same-named method resolves to nothing). Covers both the options and setup store
// forms uniformly (the action is a function node in the store file either way).
const PINIA_CONSUMER_EXT = /\.(?:ts|tsx|js|jsx|mjs|cjs|vue)$/;
const PINIA_FACTORY_RE = /\b(?:export\s+)?const\s+(\w+)\s*=\s*defineStore\s*\(/g;
const PINIA_BIND_RE = /\bconst\s+(\w+)\s*=\s*(?:await\s+)?(\w+)\s*\(/g;
const PINIA_CALL_RE = /(\w+)\s*\.\s*(\w+)\s*\(/g;
// Unbound store calls: `useXStore().action()` — the factory name itself is the
// static anchor, so no `const s = useXStore()` binding is needed. Only callee
// names in `factoryFile` qualify, so `useAnything().method()` stays silent.
const PINIA_DIRECT_CALL_RE = /\b(\w+)\s*\([^()]*\)\s*\.\s*(\w+)\s*\(/g;
const PINIA_FANOUT_CAP = 80;

async function piniaStoreEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // 1. Map each `const useXStore = defineStore(...)` factory → its store file.
  const factoryFile = new Map<string, string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!PINIA_CONSUMER_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('defineStore')) continue;
    PINIA_FACTORY_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = PINIA_FACTORY_RE.exec(content))) factoryFile.set(m[1]!, file);
  }
  if (!factoryFile.size) return [];

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!PINIA_CONSUMER_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('Store')) continue;
    const safe = stripCommentsForRegex(content, /\.(?:jsx?|mjs|cjs)$/.test(file) ? 'javascript' : 'typescript');

    // 2. Bind store vars in this file: `const <var> = <known-factory>(...)`.
    const varStore = new Map<string, string>();
    PINIA_BIND_RE.lastIndex = 0;
    let bm: RegExpExecArray | null;
    while ((bm = PINIA_BIND_RE.exec(safe))) {
      const sf = factoryFile.get(bm[2]!);
      if (sf) varStore.set(bm[1]!, sf);
    }

    // 3. Link `<store>.<method>(` → the action function node in the store's
    //    file. Two receiver shapes share the same emit:
    //      a) a bound variable — `authStore.getMenu()`;
    //      b) an unbound factory call — `useAuthStore().getMenu()`.
    const nodesInFile = ctx.getNodesInFile(file);
    const fallbackDispatcher = nodesInFile.find((n) => n.kind === 'component'); // .vue top-level setup
    let added = 0;
    const linkCall = (storeFile: string, method: string, matchIndex: number): void => {
      if (added >= PINIA_FANOUT_CAP) return;
      const line = lineOf(safe, matchIndex);
      const disp = enclosingFn(nodesInFile, line) ?? fallbackDispatcher;
      if (!disp) return;
      const target = ctx
        .getNodesByName(method)
        .find((n) => n.kind === 'function' && n.filePath === storeFile);
      if (!target || target.id === disp.id) return;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) return;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'pinia-store', via: method, registeredAt: `${file}:${line}` },
      });
      added++;
    };

    PINIA_CALL_RE.lastIndex = 0;
    let cm: RegExpExecArray | null;
    while ((cm = PINIA_CALL_RE.exec(safe)) && added < PINIA_FANOUT_CAP) {
      const storeFile = varStore.get(cm[1]!);
      if (!storeFile) continue;
      linkCall(storeFile, cm[2]!, cm.index);
    }
    PINIA_DIRECT_CALL_RE.lastIndex = 0;
    while ((cm = PINIA_DIRECT_CALL_RE.exec(safe)) && added < PINIA_FANOUT_CAP) {
      const storeFile = factoryFile.get(cm[1]!);
      if (!storeFile) continue;
      linkCall(storeFile, cm[2]!, cm.index);
    }
  }
  return edges;
}

// ── Vuex string-keyed dispatch / commit bridge ───────────────────────────────
// Vuex dispatches actions/mutations by a runtime STRING key: `dispatch('user/login')`
// / `commit('SET_TOKEN')` / `this.$store.dispatch('app/toggleDevice')`. The action
// & mutation definitions are object-literal methods in store module files (now
// extracted as function nodes). Bridge the string key to its node: the LAST `/`
// segment is the action/mutation name; the preceding segment is the namespace
// (≈ the store module's file). Resolve the name to a function node IN A STORE FILE
// (the store-file gate excludes a same-named `api/` helper — `getInfo`/`login`
// commonly collide), disambiguated by the namespace appearing in the path (or, for
// a root key, the same file — Vuex's local-module `commit('M')` inside an action).
const VUEX_DISPATCH_RE = /\b(?:dispatch|commit)\s*\(\s*['"]([A-Za-z][\w/]*)['"]/g;
const VUEX_STORE_SIGNAL = /\bdefineStore\b|\bcreateStore\b|\bVuex\b|\bmutations\b|\bactions\b|\bgetters\b|\bnamespaced\b/g;
const VUEX_FANOUT_CAP = 120;

/** A path segment (dir or filename stem) equals `seg` — `…/modules/user.js` has
 *  the segment `user` for namespace `user`. */
function pathHasSegment(filePath: string, seg: string): boolean {
  return new RegExp('[\\\\/]' + seg.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '[\\\\/.]').test(filePath);
}

async function vuexDispatchEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const storeFileCache = new Map<string, boolean>();
  const isStoreFile = (file: string): boolean => {
    let v = storeFileCache.get(file);
    if (v === undefined) {
      const c = ctx.readFile(file);
      const seen = new Set<string>();
      if (c) {
        VUEX_STORE_SIGNAL.lastIndex = 0;
        let sm: RegExpExecArray | null;
        while ((sm = VUEX_STORE_SIGNAL.exec(c))) { seen.add(sm[0]); if (seen.size >= 2) break; }
      }
      v = seen.size >= 2;
      storeFileCache.set(file, v);
    }
    return v;
  };

  const resolve = (key: string, dispatchFile: string): Node | null => {
    const segs = key.split('/');
    const action = segs[segs.length - 1]!;
    const cands = ctx.getNodesByName(action).filter((n) => n.kind === 'function' && isStoreFile(n.filePath));
    if (!cands.length) return null;
    if (segs.length > 1) {
      const mod = segs[segs.length - 2]!; // immediate namespace ≈ the module file
      return cands.find((c) => pathHasSegment(c.filePath, mod)) ?? (cands.length === 1 ? cands[0]! : null);
    }
    // Root key: a local `commit('M')` inside an action targets the same module file;
    // otherwise accept only an unambiguous single store-wide match.
    return cands.find((c) => c.filePath === dispatchFile) ?? (cands.length === 1 ? cands[0]! : null);
  };

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!PINIA_CONSUMER_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || (!content.includes('dispatch(') && !content.includes('commit('))) continue;
    const safe = stripCommentsForRegex(content, /\.(?:jsx?|mjs|cjs)$/.test(file) ? 'javascript' : 'typescript');
    const nodesInFile = ctx.getNodesInFile(file);
    const fallback = nodesInFile.find((n) => n.kind === 'component'); // .vue top-level
    VUEX_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    let added = 0;
    while ((m = VUEX_DISPATCH_RE.exec(safe)) && added < VUEX_FANOUT_CAP) {
      const key = m[1]!;
      const line = lineOf(safe, m.index);
      const disp = enclosingFn(nodesInFile, line) ?? fallback;
      if (!disp) continue;
      const target = resolve(key, file);
      if (!target || target.id === disp.id) continue;
      const edgeKey = `${disp.id}>${target.id}`;
      if (seen.has(edgeKey)) continue;
      seen.add(edgeKey);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'vuex-dispatch', via: key, registeredAt: `${file}:${line}` },
      });
      added++;
    }
  }
  return edges;
}

// ── Celery task dispatch (Python) ─────────────────────────────────────────────
// Celery decouples a task's call site from its body through async dispatch:
//   # tasks.py
//   @shared_task                       # also @app.task / @celery_app.task / @<app>.task / @task
//   def process(account_ids): ...
//   # views.py — a DIFFERENT module
//   process.apply_async(kwargs={...})  # or process.delay(...) — dynamic, no static edge
// Bridge it: link the enclosing function/method at each `.delay(`/`.apply_async(` site → the
// task function body. Precision rests on the DECORATOR gate — the dispatched name must resolve
// to a Python function carrying a celery task decorator (read from the source lines above its
// `def`, since the def's own startLine excludes the decorator). A `.delay()` on a non-task
// object resolves to no task node → no edge, so a Celery-free repo yields 0. Same-file /
// unique-candidate disambiguation like vuex.
// The receiver chain before `.delay`/`.apply_async` — `task`, `mod.task`, or
// `pkg.mod.task` (the last segment is the task name; a resolvable module prefix
// scopes the lookup to that module). Canvas forms (`group(t).delay()`,
// `t.s().delay()`) can't match — `(`/`]` break the chain — but the SIGNATURE
// constructor itself is a dispatch-binding site: `task.s(…)`/`task.si(…)`
// (aliases `.signature(…)`/`.subtask(…)`) binds the task wherever it appears —
// `link=[t.si(x)]`, `body=t.s(…)`, `chord(h).on_error(cb.s(…))`, or a
// list-comprehension element later fed to `chain(*sigs)`/`group(sigs)`. The
// container call adds nothing those sites don't already produce (same
// enclosing fn → dedup), so matching `.s`/`.si` covers the canvas surface.
const CELERY_DISPATCH_RE = /\b([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)*)\s*\.\s*(?:delay|apply_async)\s*\(/g;
const CELERY_SIG_RE = /\b([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)*)\s*\.\s*(?:s|si|signature|subtask)\s*\(/g;
// File quick-gate hint for the signature forms (the dispatch gate below uses
// cheap substring checks; these four share no common substring).
const CELERY_SIG_HINT_RE = /\.(?:s|si|signature|subtask)\(/;
// `recv.send_task('dotted.name')` — celery's string-name dispatch. Any receiver
// (`app.send_task`, `current_app.send_task`); the file-celery-import gate and the
// registered-name match keep non-celery `send_task`s out.
const CELERY_SEND_TASK_RE = /\b[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*\.\s*send_task\s*\(\s*['"]([^'"]+)['"]/g;
// A task decorator: bare `@shared_task`/`@task` or attribute `@app.task`/`@celery_app.task`,
// each optionally called with args. `\b`-bounded and `@`-anchored so `@mytask`, or a symbol
// merely named `task`, can't match. No `/g`, so `.test()` is stateless across reuse.
const CELERY_TASK_DECORATOR_RE = /@\s*(?:[A-Za-z_][\w.]*\.)?(?:shared_task|task)\b/;
// `@alias` — a decorator whose name is bound to a celery task decorator by a
// `from …celery… import shared_task as alias` line or an `alias = app.task`
// module-level assignment (see celeryFileInfo).
const CELERY_ALIAS_DECORATOR_RE = /^@\s*([A-Za-z_]\w*)\b/;
// A file "has celery imports" when an import line's module path mentions celery
// (`from celery import …`, `import celery`, `from myapp.celery import app`).
const CELERY_IMPORT_RE = /(?:^|\n)\s*(?:from\s+[\w.]*celery[\w.]*\s+import\b|import\s+[\w.]*celery[\w.]*)/;
// `from <mod> import a, b as c` — captures the module and the item list.
const CELERY_FROM_IMPORT_RE = /^[ \t]*from\s+([\w.]+)\s+import\s+([^#\n]+)/gm;
// `alias = <dotted>.task` / `= shared_task` (no call — the alias IS the decorator;
// an empty `()` is the decorator-factory form — `t = app.task()` then `@t`).
const CELERY_DECORATOR_ALIAS_RE = /^[ \t]*([A-Za-z_]\w*)\s*=\s*(?:[A-Za-z_]\w*\.)*(?:shared_task|task)\s*(?:\(\s*\))?\s*(?:#.*)?$/gm;
// `x = <dotted>.task(fn)` / `x = shared_task(fn)` — a task OBJECT bound to a fn.
const CELERY_TASK_BIND_RE = /^[ \t]*([A-Za-z_]\w*)\s*=\s*(?:[A-Za-z_]\w*\.)*(?:shared_task|task)\s*\(\s*([A-Za-z_]\w*)\s*[,)]/gm;
// `x = SomeClass(` — a module-level instance binding (a `Task` subclass instance).
const CELERY_CLASS_BIND_RE = /^[ \t]*([A-Za-z_]\w*)\s*=\s*([A-Z]\w*)\s*\(/gm;
// `class X(<bases>)` — a base list containing a `Task`-named class is a celery
// `Task` subclass when the file imports celery (`class X(app.Task)`/`(Task)`).
const CELERY_CLASS_BASES_RE = /\bclass\s+[A-Za-z_]\w*\s*\(([^)]*)\)/;
// `name='custom.name'` inside a task decorator's args — the registered task name.
const CELERY_NAME_ARG_RE = /\bname\s*=\s*['"]([^'"]+)['"]/;
const CELERY_PY_EXT = /\.py$/;
const CELERY_FANOUT_CAP = 80;
const CELERY_DECORATOR_LOOKBACK = 12; // max lines above a `def` to scan for its decorators

interface CeleryFileInfo {
  celeryImports: boolean;
  /** Names usable as `@x` task decorators (`shared_task as x`, `x = app.task`). */
  decoratorAliases: Set<string>;
  /** `from <mod> import <fn> as <alias>` → alias → { module, orig }. */
  importAliases: Map<string, { module: string; orig: string }>;
  /** `x = app.task(fn)` → x → fn (the task object is `x`, the body is `fn`). */
  taskFnBindings: Map<string, string>;
  /** `x = SomeClass(` → x → SomeClass (a `Task` subclass instance). */
  valueBindings: Map<string, string>;
}

async function celeryDispatchEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  const fileInfoCache = new Map<string, CeleryFileInfo>();
  const fileInfo = (file: string): CeleryFileInfo => {
    let info = fileInfoCache.get(file);
    if (info) return info;
    info = { celeryImports: false, decoratorAliases: new Set(), importAliases: new Map(), taskFnBindings: new Map(), valueBindings: new Map() };
    const content = ctx.readFile(file);
    if (content) {
      info.celeryImports = CELERY_IMPORT_RE.test(content);
      // Decorator aliases and task-object bindings only exist in a celery
      // context — a `st = app.task`-looking line in a celery-free file is not
      // celery evidence (the alias/decorator extensions stay inert there).
      if (info.celeryImports) {
        CELERY_DECORATOR_ALIAS_RE.lastIndex = 0;
        let am: RegExpExecArray | null;
        while ((am = CELERY_DECORATOR_ALIAS_RE.exec(content))) info.decoratorAliases.add(am[1]!);
        CELERY_TASK_BIND_RE.lastIndex = 0;
        let bm: RegExpExecArray | null;
        while ((bm = CELERY_TASK_BIND_RE.exec(content))) info.taskFnBindings.set(bm[1]!, bm[2]!);
      }
      // `from <mod> import <fn> [as <alias>]` — needed even in celery-free
      // dispatch files (the alias binds the task by its real name). A plain
      // `from tasks import bound` records `bound` too: the import may name a
      // task-object binding (`bound = app.task(fn)`) or a `Task` subclass —
      // the alias resolution follows it into the home module.
      CELERY_FROM_IMPORT_RE.lastIndex = 0;
      let im: RegExpExecArray | null;
      while ((im = CELERY_FROM_IMPORT_RE.exec(content))) {
        const mod = im[1]!;
        for (const item of im[2]!.split(',')) {
          const it = /^[ \t]*(\w+)(?:\s+as\s+(\w+))?/.exec(item);
          if (!it) continue;
          const orig = it[1]!;
          const alias = it[2];
          info.importAliases.set(alias ?? orig, { module: mod, orig });
          if (/celery/.test(mod) && (orig === 'shared_task' || orig === 'task') && alias) info.decoratorAliases.add(alias);
        }
      }
      // `x = SomeClass(` bindings are safe to record unconditionally — they only
      // resolve through taskClassRun, which itself requires the CLASS's file to
      // import celery (the gate travels with the target, not the binding site).
      CELERY_CLASS_BIND_RE.lastIndex = 0;
      let cm: RegExpExecArray | null;
      while ((cm = CELERY_CLASS_BIND_RE.exec(content))) info.valueBindings.set(cm[1]!, cm[2]!);
    }
    fileInfoCache.set(file, info);
    return info;
  };

  // The source lines ABOVE a def that hold its decorators (the def's own
  // startLine excludes them). Shared by the task-decorator check and the
  // `name='…'` custom-name lookup so both see the same decorator window.
  const decoratorWindow = (node: Node): string[] => {
    const content = ctx.readFile(node.filePath);
    if (!content) return [];
    const lines = content.split('\n');
    const stop = Math.max(0, node.startLine - 1 - CELERY_DECORATOR_LOOKBACK);
    const out: string[] = [];
    for (let i = node.startLine - 2; i >= stop; i--) {
      const t = (lines[i] ?? '').trim();
      if (/^(?:async\s+def|def|class)\b/.test(t)) break; // previous decl → stop
      out.push(t);
    }
    return out;
  };

  // Memoize the decorator check per task-candidate node: it reads the file and scans a few
  // lines above the def. Only called on names that are actually `.delay`/`.apply_async`
  // receivers, so the candidate set stays small.
  const taskCache = new Map<string, boolean>();
  const isCeleryTask = (node: Node): boolean => {
    let v = taskCache.get(node.id);
    if (v !== undefined) return v;
    v = false;
    if (node.kind === 'function' && CELERY_PY_EXT.test(node.filePath)) {
      const info = fileInfo(node.filePath);
      for (const t of decoratorWindow(node)) {
        if (CELERY_TASK_DECORATOR_RE.test(t)) { v = true; break; }
        // An aliased decorator (`@st` for `shared_task as st`, `@t` for `t = app.task`)
        // — only when the file actually imports celery.
        const alias = info.celeryImports ? CELERY_ALIAS_DECORATOR_RE.exec(t)?.[1] : undefined;
        if (alias && info.decoratorAliases.has(alias)) { v = true; break; }
      }
      // Module-level decoration: `send_email = app.task(send_email)` rebinds the
      // def name to a task object — the def is the task body.
      if (!v && info.celeryImports && info.taskFnBindings.get(node.name) === node.name) v = true;
    }
    taskCache.set(node.id, v);
    return v;
  };

  // A `class X(…Task…)` in a celery-importing file is a class-based task; the
  // dispatched work is its `run` method.
  const taskClassCache = new Map<string, Node | null>();
  const taskClassRun = (cls: Node): Node | null => {
    let v = taskClassCache.get(cls.id);
    if (v !== undefined) return v;
    v = null;
    if (cls.kind === 'class' && fileInfo(cls.filePath).celeryImports) {
      const content = ctx.readFile(cls.filePath);
      const line = content?.split('\n')[cls.startLine - 1] ?? '';
      const bases = CELERY_CLASS_BASES_RE.exec(line)?.[1];
      if (bases && bases.split(',').some((b) => b.trim().split('.').pop() === 'Task')) {
        v = ctx.getNodesInFile(cls.filePath).find(
          (n) => n.kind === 'method' && n.name === 'run' && n.startLine >= cls.startLine && n.startLine <= (cls.endLine ?? cls.startLine)
        ) ?? null;
      }
    }
    taskClassCache.set(cls.id, v);
    return v;
  };

  // The module FILE a dotted path names — `tasks` → `tasks.py`, `myapp.tasks` →
  // `myapp/tasks.py` or the `__init__.py` of the package. Suffix-matched so a
  // source root (`src/myapp/tasks.py`) still resolves; a multi-match (two
  // `tasks.py`s) is ambiguous → null. Memoized — getAllFiles is per-call.
  const moduleFileCache = new Map<string, string | null>();
  const moduleFile = (mod: string): string | null => {
    let v = moduleFileCache.get(mod);
    if (v !== undefined) return v;
    v = null;
    const rel = mod.replace(/^\.+/, '').replace(/\./g, '/');
    if (rel) {
      const hits = ctx
        .getAllFiles()
        .filter((f) => f === `${rel}.py` || f.endsWith(`/${rel}.py`) || f.endsWith(`/${rel}/__init__.py`));
      if (hits.length === 1) v = hits[0]!;
    }
    moduleFileCache.set(mod, v);
    return v;
  };

  // `x = app.task(fn)` / `x = TaskSubclass()` in `file` → the dispatched body
  // (`fn`, or the subclass's `run`). Bindings live at module level — the file
  // here is the DEFINING file (dispatch file for a local `x`, home module for
  // an imported one).
  const bindingTarget = (info: CeleryFileInfo, name: string, file: string): Node | null => {
    const boundFn = info.taskFnBindings.get(name);
    if (boundFn) {
      const bcands = ctx.getNodesInFile(file).filter((n) => n.kind === 'function' && n.name === boundFn);
      return bcands.length === 1 ? bcands[0]! : null;
    }
    const boundCls = info.valueBindings.get(name);
    if (boundCls) {
      const ccands = ctx.getNodesByName(boundCls).filter((n) => n.kind === 'class' && taskClassRun(n));
      const cls = ccands.length === 1 ? ccands[0]! : ccands.find((c) => c.filePath === file) ?? null;
      return cls ? taskClassRun(cls) : null;
    }
    return null;
  };

  // `name` as an attribute of module `modFile` — an `x = app.task(fn)` /
  // `x = TaskSubclass()` binding, a `Task` subclass, or a decorated function.
  // For a package (`…/__init__.py`) the sibling files are searched too
  // (re-exports); >1 hit across the package is ambiguous → null.
  const moduleAttrTarget = (modFile: string, name: string): Node | null => {
    const dir = modFile.endsWith('/__init__.py') ? modFile.slice(0, modFile.length - '__init__.py'.length) : null;
    const files = dir
      ? ctx.getAllFiles().filter((f) => f.startsWith(dir) && CELERY_PY_EXT.test(f))
      : [modFile];
    const hits: Node[] = [];
    for (const f of files) {
      const t =
        bindingTarget(fileInfo(f), name, f) ??
        (() => {
          const cls = ctx.getNodesInFile(f).find((n) => n.kind === 'class' && n.name === name && taskClassRun(n));
          return cls ? taskClassRun(cls) : null;
        })() ??
        ctx.getNodesInFile(f).find((n) => n.kind === 'function' && n.name === name && isCeleryTask(n)) ??
        null;
      if (t) hits.push(t);
    }
    return hits.length === 1 ? hits[0]! : null;
  };

  const resolve = (recv: string, dispatchFile: string): Node | null => {
    const info = fileInfo(dispatchFile);
    // `mod.name.delay()` — when `mod` resolves to a module file (`import
    // tasks`, `import myapp.tasks`, or `from pkg import tasks`), the attribute
    // is resolved INSIDE that module (the module binding is authoritative —
    // no global-name fallback for a resolved module).
    const dot = recv.lastIndexOf('.');
    if (dot > 0) {
      const modRef = recv.slice(0, dot);
      const leaf = recv.slice(dot + 1);
      const modAlias = info.importAliases.get(modRef);
      const mf = moduleFile(modAlias ? `${modAlias.module}.${modAlias.orig}` : modRef);
      if (mf) return moduleAttrTarget(mf, leaf);
      recv = leaf; // `x.y.delay()` with unresolvable `x` — resolve `y` globally
    }

    // `from m import f [as a]` — an explicit import binding wins over global
    // name lookup (the imported object is what `.delay` binds at runtime).
    // Exclusive: resolves only through the import, so an imported non-task is
    // never bridged to a same-named task elsewhere.
    const alias = info.importAliases.get(recv);
    if (alias) {
      const mf = moduleFile(alias.module);
      if (mf) return moduleAttrTarget(mf, alias.orig);
      // Module unplaceable in-repo — fall back to a unique global task under
      // the original name (a re-exported or renamed module we can't locate).
      const acands = ctx.getNodesByName(alias.orig).filter((n) => n.kind === 'function' && isCeleryTask(n));
      return acands.length === 1 ? acands[0]! : null;
    }

    // Direct name → decorated task function.
    const cands = ctx.getNodesByName(recv).filter((n) => n.kind === 'function' && isCeleryTask(n));
    if (cands.length) {
      if (cands.length === 1) return cands[0]!;
      // Cross-module name collision: prefer a task defined in the dispatching file, else bail
      // (ambiguous — precision over recall, like vuex's root-key resolution).
      return cands.find((c) => c.filePath === dispatchFile) ?? null;
    }
    // Class-based task: `MyTask.delay()` → its `run` (a `Task` subclass).
    const clsCands = ctx.getNodesByName(recv).filter((n) => n.kind === 'class' && taskClassRun(n));
    if (clsCands.length) {
      const cls = clsCands.length === 1 ? clsCands[0]! : clsCands.find((c) => c.filePath === dispatchFile) ?? null;
      return cls ? taskClassRun(cls) : null;
    }
    // Dispatch-file bindings — `x = app.task(fn)` → `x.delay()` runs fn;
    // `x = MyTask()` → `x.delay()` runs MyTask.run.
    return bindingTarget(info, recv, dispatchFile);
  };

  // Registered task name → task fn, for `send_task('dotted.name')` dispatch.
  // Built lazily on the first send_task site (most celery repos have none).
  // Default name = `<module dotted path>.<fn>`; custom `name='…'` decorator args
  // register under their literal value.
  let taskNames: Map<string, Node[]> | null = null;
  const taskNameIndex = (): Map<string, Node[]> => {
    if (taskNames) return taskNames;
    taskNames = new Map();
    const register = (key: string, n: Node) => {
      const arr = taskNames!.get(key);
      if (arr) arr.push(n); else taskNames!.set(key, [n]);
    };
    for (const file of ctx.getAllFiles()) {
      if (!CELERY_PY_EXT.test(file)) continue;
      const info = fileInfo(file);
      if (!info.celeryImports) continue;
      const mod = file.replace(/\.py$/, '').replace(/[\\/]/g, '.');
      for (const n of ctx.getNodesInFile(file)) {
        if (n.kind === 'function' && isCeleryTask(n)) {
          register(`${mod}.${n.name}`, n);
          // A `name='x'` arg anywhere in the decorator window is the custom name.
          for (const t of decoratorWindow(n)) {
            const nm = CELERY_NAME_ARG_RE.exec(t);
            if (nm) register(nm[1]!, n);
          }
        }
      }
      // `x = app.task(fn)` registers `fn` under the module's dotted path.
      for (const fnName of info.taskFnBindings.values()) {
        const fn = ctx.getNodesInFile(file).find((n) => n.kind === 'function' && n.name === fnName);
        if (fn) register(`${mod}.${fnName}`, fn);
      }
    }
    return taskNames;
  };
  const resolveTaskName = (taskName: string): Node | null => {
    const index = taskNameIndex();
    const direct = index.get(taskName);
    if (direct) return direct.length === 1 ? direct[0]! : null;
    // Package-prefix tolerance: `myapp.tasks.f` vs file `tasks.py` (`tasks.f`),
    // or the reverse — match when either dotted form suffixes the other.
    const matches = new Map<string, Node>();
    for (const [key, nodes] of index) {
      if (key.endsWith(`.${taskName}`) || taskName.endsWith(`.${key}`)) for (const n of nodes) matches.set(n.id, n);
    }
    return matches.size === 1 ? [...matches.values()][0]! : null;
  };

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!CELERY_PY_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (
      !content ||
      (!content.includes('.delay(') &&
        !content.includes('.apply_async(') &&
        !content.includes('.send_task(') &&
        !CELERY_SIG_HINT_RE.test(content))
    )
      continue;
    const safe = stripCommentsForRegex(content, 'python');
    const nodesInFile = ctx.getNodesInFile(file);
    const lineAt = makeLineAt(safe, 1);
    let added = 0;
    CELERY_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = CELERY_DISPATCH_RE.exec(safe)) && added < CELERY_FANOUT_CAP) {
      const recv = m[1]!.replace(/\s+/g, ''); // `x . y.delay()` → `x.y`
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue; // module-level dispatch — no source symbol to attribute
      const target = resolve(recv, file);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'celery-dispatch', via: recv.slice(recv.lastIndexOf('.') + 1), registeredAt: `${file}:${line}` },
      });
      added++;
    }
    // `recv.send_task('dotted.name')` — string-name dispatch. The gate is the
    // registered-name match itself: the task-name index is built only from
    // celery-importing files, so in a celery-free project nothing registers
    // and `send_task` produces nothing. (The common call shape is
    // `from myapp import app; app.send_task(…)` — no celery import in the
    // dispatch file — so a per-file import gate would lose real dispatches.)
    CELERY_SEND_TASK_RE.lastIndex = 0;
    while ((m = CELERY_SEND_TASK_RE.exec(safe)) && added < CELERY_FANOUT_CAP) {
      const taskName = m[1]!;
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const target = resolveTaskName(taskName);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'celery-dispatch', via: taskName, registeredAt: `${file}:${line}` },
      });
      added++;
    }
    // Canvas signature construction — `task.s(…)`/`task.si(…)`/`.signature(…)`/
    // `.subtask(…)` binds a task for deferred dispatch inside group/chain/chord
    // args, `link=[t.si(x)]`, `body=t.s(…)`, `on_error(cb.s(…))`, or list
    // elements later fed to `chain(*sigs)`/`group(sigs)`. Same resolve() +
    // dedup as `.delay()` — the container's own `.delay()` produces nothing
    // these sites don't already (same enclosing fn → same edge pair).
    CELERY_SIG_RE.lastIndex = 0;
    while ((m = CELERY_SIG_RE.exec(safe)) && added < CELERY_FANOUT_CAP) {
      const recv = m[1]!.replace(/\s+/g, '');
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const target = resolve(recv, file);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'celery-dispatch', via: recv.slice(recv.lastIndexOf('.') + 1), registeredAt: `${file}:${line}` },
      });
      added++;
    }
  }
  return edges;
}

// ── Spring application events (Java) ──────────────────────────────────────────
// Spring decouples an event PUBLISHER from its LISTENER(s) through the application
// event bus, linked by the EVENT TYPE (not a name):
//   // SomeService.java
//   eventPublisher.publishEvent(new PasswordChangedEvent(this, username));   // publish
//   // RememberMeTokenRevoker.java — a DIFFERENT file
//   @EventListener(PasswordChangedEvent.class)                              // listen
//   public void onPasswordChanged(PasswordChangedEvent event) { ... }
// Bridge it: link the enclosing method at each `publishEvent(new XEvent(...))` site →
// every listener method of XEvent. Listeners are `@EventListener` / `@TransactionalEventListener`
// methods (event type = the first param type, or the `@EventListener(X.class)` value form) and
// the older `class … implements ApplicationListener<X> { void onApplicationEvent(X e) }`. Keyed
// by exact type name, usually cross-file. A repo with no `@EventListener`/`publishEvent` yields 0.
// (Java method nodes INCLUDE their leading annotations in the range — startLine is the first
// `@…` line — so the annotation block is scanned DOWNWARD from startLine, bounded to consecutive
// `@`-lines so it can't bleed into an adjacent method.)
// `recv.publishEvent(<arg>)` — arg is an inline `new XEvent(…)`, a bare
// identifier (resolved within the enclosing method), or inside a listener body
// a call expression (`event.getDelegate()`) whose erased type fans out to all
// identifier whose type is inferred within the enclosing method (shared with
// MediatR's arg resolver: a `X arg` param/local decl or an `arg = new X(…)`
// assignment wins; untyped/ambiguous args yield nothing).
const SPRING_PUBLISH_RE = /\.publishEvent\s*\(\s*(new\s+[A-Z][A-Za-z0-9_]*(?=[\s(])|[A-Za-z_]\w*(?=[\s,)]))/g;
// `recv.publishEvent(<expr>.<m>(…))` — a call-expression arg, matched separately
// because the concrete event type is erased behind the call (halo's
// SharedEventDispatcher re-publishes `event.getDelegate()` where the delegate
// field is base `ApplicationEvent`). Only bridged when the enclosing method is
// itself a registered listener (an event→event re-dispatch), fanned out to
// every known listener — the runtime bound is genuinely "any event".
const SPRING_REPUBLISH_RE = /\.publishEvent\s*\(\s*[A-Za-z_]\w*\s*\.\s*\w+\s*\(/g;
// `registerEvent(<arg>)` — Spring Data's `AbstractAggregateRoot.registerEvent`
// domain-event hook (published on save). Gated on the file referencing
// `AbstractAggregateRoot`, so a same-named helper in a non-DDD file is skipped.
const SPRING_REGISTER_RE = /\bregisterEvent\s*\(\s*(new\s+[A-Z][A-Za-z0-9_]*(?=[\s(])|[A-Za-z_]\w*(?=[\s,)]))/g;
// `@DomainEvents` — a method whose RETURNED `new XEvent(…)` objects are
// published on save (the sibling of `registerEvent` in the same backlog item).
const SPRING_DOMAIN_EVENTS_ANNO_RE = /@DomainEvents\b/;
const SPRING_NEW_TYPE_RE = /\bnew\s+([A-Z][A-Za-z0-9_]*)\s*\(/g;
const SPRING_LISTENER_ANNO_RE = /@(?:EventListener|TransactionalEventListener)\b/;
const SPRING_ANNO_TYPE_RE = /@(?:EventListener|TransactionalEventListener)\s*\(\s*([A-Z][A-Za-z0-9_]*)\.class/;
const SPRING_APP_LISTENER_RE = /\bApplicationListener\s*</;
const SPRING_AGGREGATE_ROOT_RE = /\bAbstractAggregateRoot\b/;
const SPRING_JAVA_EXT = /\.java$/;
const SPRING_FANOUT_CAP = 80;

/** The first parameter's type from a Java method `signature` (`"void (XEvent e)"` → `XEvent`).
 *  Skips a leading `final`/`@Anno`, strips generics, and requires a PascalCase class name (event
 *  types are classes) — so a no-arg or primitive-param method yields null. */
function springFirstParamType(sig: string | undefined): string | null {
  if (!sig) return null;
  const open = sig.indexOf('(');
  if (open < 0) return null;
  const close = sig.indexOf(')', open);
  const inner = sig.slice(open + 1, close < 0 ? sig.length : close).trim();
  if (!inner) return null;
  const first = inner.split(',')[0]!.trim();
  const toks = first.split(/\s+/).filter((t) => t && t !== 'final' && !t.startsWith('@'));
  if (toks.length < 2) return null; // need `Type name`
  const type = toks[toks.length - 2]!.replace(/<.*$/, ''); // drop generic args
  return /^[A-Z][A-Za-z0-9_]*$/.test(type) ? type : null;
}

async function springEventEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // Pass 1 — event-type → listener methods, scanning only event-relevant files.
  // This is the ONLY full read sweep: publisher files are recorded here so
  // pass 2 re-reads just those instead of every .java file again (#1212 —
  // the double full-repo read was one of the tail's longest unyielded spans).
  const listeners = new Map<string, Node[]>();
  const publisherFiles: string[] = [];
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!SPRING_JAVA_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content) continue;
    if (content.includes('.publishEvent(')
      || (SPRING_AGGREGATE_ROOT_RE.test(content) && content.includes('registerEvent('))
      || content.includes('@DomainEvents')) publisherFiles.push(file);
    const hasAnno = content.includes('@EventListener') || content.includes('@TransactionalEventListener');
    const hasAppListener = SPRING_APP_LISTENER_RE.test(content);
    if (!hasAnno && !hasAppListener) continue;
    const lines = content.split('\n');
    for (const node of ctx.getNodesInFile(file)) {
      if (node.kind !== 'method') continue;
      // Collect this method's own leading annotation block (consecutive `@`-lines from startLine).
      const annoLines: string[] = [];
      for (let i = node.startLine - 1; i < lines.length && i < node.startLine + 7; i++) {
        const t = (lines[i] ?? '').trim();
        if (!t.startsWith('@')) break; // reached the declaration → stop (no bleed into next method)
        annoLines.push(t);
      }
      const head = annoLines.join('\n');
      const annotated = hasAnno && SPRING_LISTENER_ANNO_RE.test(head);
      const isAppListener = hasAppListener && node.name === 'onApplicationEvent';
      if (!annotated && !isAppListener) continue;
      let type = springFirstParamType(node.signature);
      if (!type && annotated) {
        const m = SPRING_ANNO_TYPE_RE.exec(head);
        if (m) type = m[1]!;
      }
      if (!type) continue;
      let arr = listeners.get(type);
      if (!arr) { arr = []; listeners.set(type, arr); }
      arr.push(node);
    }
  }
  if (!listeners.size) return [];

  // Pass 2 — link each publish/register site → every listener of the event type.
  // Only the publisher files recorded in pass 1 are (re-)read.
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of publisherFiles) {
    if ((++scannedFiles & 15) === 0) await onYield();
    const content = ctx.readFile(file);
    if (!content) continue;
    const safe = stripCommentsForRegex(content, 'java');
    const safeLines = safe.split('\n');
    const nodesInFile = ctx.getNodesInFile(file);
    let m: RegExpExecArray | null;
    let added = 0; // per-file fan-out cap (same convention as the other passes)
    const emit = (disp: Node, type: string, line: number): void => {
      const targets = listeners.get(type);
      if (!targets || !targets.length) return;
      for (const target of targets) {
        if (target.id === disp.id || added >= SPRING_FANOUT_CAP) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'spring-event', via: type, registeredAt: `${file}:${line}` },
        });
        added++;
      }
    };
    // The event type of a call-site arg: inline `new X(…)` directly, else the
    // identifier resolved within the enclosing method (decl/param/`= new X`),
    // the same narrow inference MediatR uses. Untyped args yield nothing.
    const argType = (arg: string, disp: Node, line: number): string | null =>
      resolveMediatrArgType(arg, safeLines, disp.startLine, line);
    if (content.includes('.publishEvent(')) {
      SPRING_PUBLISH_RE.lastIndex = 0;
      while ((m = SPRING_PUBLISH_RE.exec(safe)) && added < SPRING_FANOUT_CAP) {
        const line = lineOf(safe, m.index);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp) continue;
        const type = argType(m[1]!, disp, line);
        if (type) emit(disp, type, line);
      }
      SPRING_REPUBLISH_RE.lastIndex = 0;
      while ((m = SPRING_REPUBLISH_RE.exec(safe)) && added < SPRING_FANOUT_CAP) {
        const line = lineOf(safe, m.index);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp) continue;
        // Gate: the enclosing method must itself be a registered listener —
        // registered in the map by its param type, or carrying the annotation.
        const pType = springFirstParamType(disp.signature);
        let isListener = !!pType && (listeners.get(pType) ?? []).some((l) => l.id === disp.id);
        if (!isListener) {
          for (let i = disp.startLine - 1; i < safeLines.length && i < disp.startLine + 7; i++) {
            const t = (safeLines[i] ?? '').trim();
            if (!t.startsWith('@')) break;
            if (SPRING_LISTENER_ANNO_RE.test(t)) { isListener = true; break; }
          }
        }
        if (!isListener) continue;
        for (const targets of listeners.values()) {
          for (const target of targets) {
            if (target.id === disp.id || added >= SPRING_FANOUT_CAP) continue;
            const key = `${disp.id}>${target.id}`;
            if (seen.has(key)) continue;
            seen.add(key);
            edges.push({
              source: disp.id,
              target: target.id,
              kind: 'calls',
              line,
              provenance: 'heuristic',
              metadata: { synthesizedBy: 'spring-event', via: 'delegate:*', registeredAt: `${file}:${line}` },
            });
            added++;
          }
        }
      }
    }
    // `registerEvent(…)` — AbstractAggregateRoot's domain-event hook; gated on
    // the file actually referencing the aggregate base so a same-named helper
    // in a non-DDD file is skipped.
    if (SPRING_AGGREGATE_ROOT_RE.test(safe)) {
      SPRING_REGISTER_RE.lastIndex = 0;
      while ((m = SPRING_REGISTER_RE.exec(safe)) && added < SPRING_FANOUT_CAP) {
        const line = lineOf(safe, m.index);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp) continue;
        const type = argType(m[1]!, disp, line);
        if (type) emit(disp, type, line);
      }
    }
    // `@DomainEvents` — the method's RETURNED `new XEvent(…)` objects are
    // published on save: scan its body for constructions, filtered through the
    // listener map so helper-object constructions are ignored.
    if (safe.includes('@DomainEvents')) {
      for (const node of nodesInFile) {
        if (node.kind !== 'method') continue;
        let domainEvents = false;
        for (let i = node.startLine - 1; i < safeLines.length && i < node.startLine + 7; i++) {
          const t = (safeLines[i] ?? '').trim();
          if (!t.startsWith('@')) break;
          if (SPRING_DOMAIN_EVENTS_ANNO_RE.test(t)) { domainEvents = true; break; }
        }
        if (!domainEvents) continue;
        const end = node.endLine ?? node.startLine;
        const body = safeLines.slice(node.startLine - 1, end).join('\n');
        SPRING_NEW_TYPE_RE.lastIndex = 0;
        while ((m = SPRING_NEW_TYPE_RE.exec(body)) && added < SPRING_FANOUT_CAP) {
          const line = node.startLine + lineOf(body, m.index) - 1;
          emit(node, m[1]!, line);
        }
      }
    }
  }
  return edges;
}

// ── MediatR request/notification dispatch (C#/.NET) ───────────────────────────
// MediatR decouples a Send/Publish call site from its Handle method through a mediator,
// linked by the request/notification TYPE (the IRequestHandler<T,…> generic):
//   // CancelOrderCommandHandler.cs — the handler
//   public class CancelOrderCommandHandler : IRequestHandler<CancelOrderCommand, bool> {
//       public async Task<bool> Handle(CancelOrderCommand request, CancellationToken ct) { … }
//   // some controller — the dispatch (usually a DIFFERENT file)
//   var command = new CancelOrderCommand(orderId);   await _mediator.Send(command);
// Bridge it: link the enclosing method at each mediator `.Send(x)`/`.Publish(x)` site → the
// `Handle` method of the handler for x's type. The sent type is resolved from the argument —
// inline `new X(…)`, a local `var v = new X(…)`, or a parameter/local declared `X v` — bounded
// to the enclosing method. Precision rests on TWO gates: the receiver must be mediator-ish
// (`mediator`/`sender`/`publisher`, so MAUI `MessagingCenter.Send` is ignored) AND the resolved
// type must be a known handler request type (so a same-named non-request DTO is never bridged).
// `Publish` also has a fan-out arm for the canonical domain-event loop —
// `foreach (var domainEvent in domainEvents) await mediator.Publish(domainEvent)`
// (eShop MediatorExtension) — where the arg is `var`-erased or declared as the
// notification base (`INotification`): runtime dispatch reaches every
// `INotificationHandler<T>.Handle`, so the site links to all of them. `Send`
// never fans out (single-handler semantics).
// C# has no `signature` on method nodes, so the handler's request type is read from the class
// base-list source (`: IRequestHandler<X,…>`), not a param signature.
const MEDIATR_HANDLER_BASE_RE = /(?:IRequestHandler|INotificationHandler)\s*<\s*([A-Za-z_]\w*)/;
// The arg after `Send(`/`Publish(`: `new X(…)`, a bare/member identifier, or
// (rare) an explicit generic call `Send<R>(x)`. A member path (`req.Command`,
// `this.cmd`) is CAPTURED so the arg resolver can decline it — resolving only
// the head ident's declared type would mis-bridge the member's type.
const MEDIATR_DISPATCH_RE = /([A-Za-z_][\w.]*)\s*\.\s*(Send|Publish)\s*(?:<[^<>\n]{0,80}>)?\s*\(\s*(new\s+[A-Z]\w*|[A-Za-z_]\w*(?:\.\w+)*)/g;
const MEDIATR_RECEIVER_RE = /(?:mediator|sender|publisher)/i;
// A `Publish(x)` arg declared as the notification base itself (`INotification`,
// `IDomainEvent`, `DomainEvent`…) — runtime dispatch reaches every notification
// handler, so the erased type fans out instead of missing a concrete key.
const MEDIATR_NOTIFICATION_ARG_RE = /^I?(?:Domain)?Notification$|^IDomainEvent$/;
const MEDIATR_CS_EXT = /\.cs$/;
const MEDIATR_FANOUT_CAP = 80;
const MEDIATR_HANDLER_DECL_LOOKAHEAD = 4; // lines from a class startLine to find a wrapped base list

/** The type sent at a MediatR `.Send(arg)`/`.Publish(arg)` site: an inline `new X(…)`, else
 *  `arg` as an identifier resolved within the enclosing method — a `… arg = new X(…)` assignment
 *  (wins), or a parameter/local declared `X arg`. Returns null when the type can't be seen. */
function resolveMediatrArgType(arg: string, lines: string[], methodStart: number, dispatchLine: number): string | null {
  const inl = /^new\s+([A-Z]\w*)/.exec(arg);
  if (inl) return inl[1]!;
  if (!/^[A-Za-z_]\w*$/.test(arg)) return null;
  const assignRe = new RegExp(`\\b${arg}\\b\\s*=\\s*new\\s+([A-Z]\\w*)`);
  // `X arg` or `X<…> arg` — the declared type resolves to its erased name
  // (`IdentifiedCommand<T,R> message` → `IdentifiedCommand`, the same erasure
  // `new X<…>` args already use; an interface-typed `IRequest<R> request` →
  // `IRequest`, which simply isn't a handler key and stays silent).
  const declRe = new RegExp(`\\b([A-Z]\\w*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\\s+${arg}\\b`);
  let declType: string | null = null;
  for (let i = Math.max(0, methodStart - 1); i < dispatchLine && i < lines.length; i++) {
    const ln = lines[i] ?? '';
    const a = assignRe.exec(ln);
    if (a) return a[1]!; // an explicit `arg = new X` is the most specific — take it
    if (!declType) {
      const d = declRe.exec(ln);
      if (d) declType = d[1]!; // a `X arg` declaration — remember, but keep scanning for an assignment
    }
  }
  return declType;
}

/** `arg` is the loop variable of a `foreach (… arg in <collection>)` where the
 *  collection name is event-ish (`domainEvents`, `notifications`…). The
 *  canonical DDD fan-out (eShop `MediatorExtension`): domain events are held as
 *  `INotification`, so `var domainEvent` erases the type — but the collection
 *  name carries the evidence that `Publish(arg)` reaches all handlers. */
function isForeachEventVar(arg: string, lines: string[], methodStart: number, dispatchLine: number): boolean {
  if (!/^[A-Za-z_]\w*$/.test(arg)) return false;
  const re = new RegExp(`foreach\\s*\\(\\s*(?:var|[A-Z]\\w*(?:<[\\w.<>,?\\s]*>)?)\\s+${arg}\\s+in\\s+([\\w.]+)`);
  for (let i = Math.max(0, methodStart - 1); i < dispatchLine && i < lines.length; i++) {
    const m = re.exec(lines[i] ?? '');
    if (m && /events|notifications/i.test(m[1]!)) return true;
  }
  return false;
}

async function mediatrDispatchEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // Pass 1 — request/notification type → the Handle method of each handler class.
  // `notificationHandlers` additionally records every INotificationHandler<T> —
  // a `Publish` of a base-typed or collection element fans out to all of them.
  const handlers = new Map<string, Node[]>();
  const notificationHandlers: Node[] = [];
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!MEDIATR_CS_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || (!content.includes('IRequestHandler<') && !content.includes('INotificationHandler<'))) continue;
    const lines = content.split('\n');
    const nodesInFile = ctx.getNodesInFile(file);
    for (const cls of nodesInFile) {
      if (cls.kind !== 'class') continue;
      const decl = lines.slice(cls.startLine - 1, cls.startLine - 1 + MEDIATR_HANDLER_DECL_LOOKAHEAD).join('\n');
      const m = MEDIATR_HANDLER_BASE_RE.exec(decl);
      if (!m) continue;
      const type = m[1]!;
      const end = cls.endLine ?? cls.startLine;
      const handle = nodesInFile.find(
        (n) => n.kind === 'method' && n.name === 'Handle' && n.startLine >= cls.startLine && n.startLine <= end
      );
      if (!handle) continue;
      let arr = handlers.get(type);
      if (!arr) { arr = []; handlers.set(type, arr); }
      arr.push(handle);
      if (/INotificationHandler\s*</.test(decl)) notificationHandlers.push(handle);
    }
  }
  if (!handlers.size) return [];

  // Pass 2 — link each mediator-ish .Send(x)/.Publish(x) → the handler of x's type.
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!MEDIATR_CS_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || (!content.includes('.Send(') && !content.includes('.Publish('))) continue;
    const safe = stripCommentsForRegex(content, 'csharp');
    const safeLines = safe.split('\n');
    const nodesInFile = ctx.getNodesInFile(file);
    MEDIATR_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    let added = 0;
    while ((m = MEDIATR_DISPATCH_RE.exec(safe)) && added < MEDIATR_FANOUT_CAP) {
      if (!MEDIATR_RECEIVER_RE.test(m[1]!)) continue; // not a mediator (MessagingCenter, HttpClient, …)
      const line = lineOf(safe, m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const verb = m[2]!;
      const arg = m[3]!;
      const type = resolveMediatrArgType(arg, safeLines, disp.startLine, line);
      let targets = type ? handlers.get(type) : undefined;
      // Publish of an erased/base-typed notification reaches every notification
      // handler (MediatR dispatches on the runtime type, whose static bound is
      // the INotification collection/base). Two evidence paths: a declared
      // notification-base arg type, or a foreach var over an event-ish
      // collection (eShop `foreach (var domainEvent in domainEvents)`).
      if (!targets && verb === 'Publish' && notificationHandlers.length) {
        const fan = type
          ? MEDIATR_NOTIFICATION_ARG_RE.test(type)
          : isForeachEventVar(arg, safeLines, disp.startLine, line);
        if (fan) targets = notificationHandlers;
      }
      if (!targets) continue;
      for (const target of targets) {
        if (target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'mediatr-dispatch', via: type ?? `${arg}:*`, registeredAt: `${file}:${line}` },
        });
        added++;
      }
    }
  }
  return edges;
}

// ── NgRx effects dispatch (Angular/TypeScript) ──────────────────────────────
// NgRx decouples a `store.dispatch(action)` call site from the effect that reacts to it,
// linked by the ACTION: an effect subscribes to the action stream via `ofType(...)` and a
// component/service dispatches that action onto the store:
//   // user.effects.ts — the handler side (three registration shapes)
//   loadUsers$ = createEffect(() => this.actions$.pipe(ofType(UsersActions.loadUsers), …));
//   @Effect() legacy$ = this.actions$.pipe(ofType(LoadUsers), …);
//   export const func$ = createEffect((a$ = inject(Actions)) => a$.pipe(ofType(refresh), …));
//   // users.component.ts — the dispatch side (usually a DIFFERENT file)
//   this.store.dispatch(UsersActions.loadUsers());   // or dispatch(new LoadUsers())
// Bridge it: link the enclosing method at each `*.store.dispatch(x)` site → every effect
// that ofType-subscribes to x's action. Both sides normalize to the same key space — the
// action creator's LAST dot-segment (`UsersActions.loadUsers` → `loadUsers`) or the legacy
// action CLASS name (`new LoadUsers()` → `LoadUsers`). Precision gates: the dispatch
// receiver must match /store/i (drops `dispatchEvent` and other `.dispatch(` protocols —
// bare `dispatch(` never matches since a receiver is required), and the dispatched action
// must have a registered ofType handler. `{dispatch:false}` effects still register — they
// are ofType subscribers. The READ side is the same hop in reverse: any `.ts`
// file that imports `@ngrx` and calls `*.store.select(sel)`/`selectSignal(sel)`
// (inside an effect — the `concatLatestFrom`/`withLatestFrom` args are the common
// carriers — or in a component/guard reading state) links the reader → the
// selector node as `synthesizedBy:'ngrx-select'`, resolved through the reading
// file's imports with ambiguity declining. NgRx Signal Store
// (`signalStore`/`rxMethod`) stays out of scope — a different registration shape.
// `EffectsModule.forRoot`/`provideEffects` registration lists are unnecessary — the
// ofType scan finds effect definitions directly.
const NGRX_OFTYPE_RE = /ofType\s*\(([^)]*)\)/g;
const NGRX_CREATE_EFFECT_RE = /\bcreateEffect\s*\(/;
const NGRX_EFFECT_ANNO_RE = /@Effect\b/;
const NGRX_DISPATCH_RE = /([\w$.]*)\s*\.\s*dispatch\s*\(\s*(new\s+(\w+)|([\w$.]+)\s*\(|(\w+))/g;
const NGRX_RECEIVER_RE = /store/i;
const NGRX_TS_EXT = /\.tsx?$/;
const NGRX_JS_EXT = /\.(?:tsx?|jsx?|mjs|cjs)$/;
const NGRX_FANOUT_CAP = 80;
const NGRX_ANNO_LOOKBACK = 3; // lines above a member's startLine a `@Effect` decorator may sit
// The READ side of the store — `*.store.select(sel)` / `*.store.selectSignal(sel)`
// (the args of `concatLatestFrom`/`withLatestFrom` inside effects are the common
// carriers, but a component's `pending$ = this.store.select(sel)` is the same
// hop). The receiver must look store-ish; the arg is a bare or namespace-dotted
// selector ref — a quoted string key (`store.select('count')`) never matches.
const NGRX_SELECT_RE = /([\w$.]*)\s*\.\s*select(?:Signal)?\s*\(\s*([A-Za-z_$][\w$.]*)\s*\)/g;
// A candidate selector node must PRODUCE a selector (a createSelector-family
// call in its signature) or live in a conventional selectors file — a plain
// `function selectX`/`const selectX` elsewhere is not evidence enough.
const NGRX_SELECTOR_SIG_RE = /\bcreateSelector\b|\bcreateFeatureSelector\b|\bcreateStructuredSelector\b|\bcreateSelectorFactory\b|\bgetSelectors\b|\bgetRouterSelectors\b/;
const NGRX_SELECTOR_FILE_RE = /(?:^|[/\\])[^/\\]*selectors?[^/\\]*\.|(?:^|[/\\])selectors?[/\\]/i;
const NGRX_SELECTOR_KINDS = new Set<NodeKind>(['constant', 'function']);

/** Normalize an ofType arg or dispatch callee to its action key: the LAST dot-segment
 *  (`TaskSharedActions.restoreTask` → `restoreTask`, `loadUsers` → `loadUsers`). String-typed
 *  ofType args (`ofType('[Users] Load')`) are skipped — a dispatch site can never produce
 *  that key. Returns null for anything that isn't a plain dotted identifier. */
function ngrxActionKey(expr: string): string | null {
  const t = expr.trim();
  if (!t || /^['"`]/.test(t)) return null;
  const last = t.split('.').pop()!.trim();
  return /^[A-Za-z_$][\w$]*$/.test(last) ? last : null;
}

/** The action dispatched at a `store.dispatch(arg)` site when `arg` is a bare identifier:
 *  resolved within the enclosing method — a `… arg = someCreator(…)` / `… arg = new X(…)`
 *  assignment (wins), or a parameter/local annotated `arg: X`. Returns null when the
 *  action can't be seen. Adapted from resolveMediatrArgType for TS syntax. */
function resolveNgrxDispatchArg(arg: string, lines: string[], methodStart: number, dispatchLine: number): string | null {
  if (!/^[A-Za-z_$][\w$]*$/.test(arg)) return null;
  const assignRe = new RegExp(`\\b${arg}\\b\\s*=\\s*(?:new\\s+)?([A-Za-z_$][\\w$.]*)\\s*\\(`);
  const declRe = new RegExp(`\\b${arg}\\b\\s*:\\s*([A-Za-z_$][\\w$.]*)`);
  let declType: string | null = null;
  for (let i = Math.max(0, methodStart - 1); i < dispatchLine && i < lines.length; i++) {
    const ln = lines[i] ?? '';
    const a = assignRe.exec(ln);
    if (a) return ngrxActionKey(a[1]!); // an explicit `arg = creator()` / `arg = new X()` wins
    if (!declType) {
      const d = declRe.exec(ln);
      if (d) declType = d[1]!; // an `arg: SomeAction` annotation — remember, but keep scanning for an assignment
    }
  }
  return declType ? ngrxActionKey(declType) : null; // `arg: Ns.Action` normalizes to `Action` too
}

/** Directory segments of a file path (everything before the basename). */
function dirSegs(filePath: string): string[] {
  return filePath.split('/').slice(0, -1);
}

/** Innermost `property` node containing `line` — class-field readers like
 *  `user$ = this.store.select(…)` that neither enclosingFn (methods/functions/
 *  components) nor enclosingValue (constants/variables) covers. */
function enclosingProperty(nodesInFile: readonly Node[], line: number): Node | null {
  let best: Node | null = null;
  for (const n of nodesInFile) {
    if (n.kind !== 'property') continue;
    const end = n.endLine ?? n.startLine;
    if (n.startLine <= line && end >= line && (!best || n.startLine >= best.startLine)) best = n;
  }
  return best;
}

/** The selector node a `store.select(ref)` arg names. `ref` is bare
 *  (`selectIds`) or namespace-dotted (`BookSelectors.selectIds`); the name is
 *  the last segment. Candidates are gated to selector-producing nodes
 *  (NGRX_SELECTOR_SIG_RE on the signature, or a selectors-file path). A
 *  `Ns.sel`/`sel` import mapping in the effect's file pins the candidate's
 *  file — and a pin that yields nothing declines rather than falling back to
 *  a same-named selector elsewhere. Unpinned, same-file wins, then the
 *  candidate sharing the longest directory prefix with the effect; a tie
 *  declines (NgRx features repeat selector names — `selectLoading`,
 *  `selectError` — so "the only one in this feature's dir" is the honest
 *  disambiguator). */
function resolveNgrxSelector(
  ctx: ResolutionContext,
  fromFile: string,
  expr: string,
  imports: Map<string, string>
): Node | null {
  const name = ngrxActionKey(expr);
  if (!name) return null;
  let all: Node[];
  try {
    all = ctx.getNodesByName(name);
  } catch {
    return null;
  }
  const gated = all.filter(
    (n) =>
      NGRX_SELECTOR_KINDS.has(n.kind) &&
      ((n.signature !== undefined && NGRX_SELECTOR_SIG_RE.test(n.signature)) ||
        NGRX_SELECTOR_FILE_RE.test(n.filePath))
  );
  if (!gated.length) return null;
  const head = expr.trim().split('.')[0]!;
  const pinnedFile = imports.get(head);
  if (pinnedFile) {
    const pinned = gated.filter((n) => n.filePath === pinnedFile);
    return pinned.length === 1 ? pinned[0]! : null;
  }
  const sameFile = gated.filter((n) => n.filePath === fromFile);
  const pool = sameFile.length ? sameFile : gated;
  if (pool.length === 1) return pool[0]!;
  const aSegs = dirSegs(fromFile);
  let best: Node | null = null;
  let bestShared = -1;
  let tie = false;
  for (const n of pool) {
    const bSegs = dirSegs(n.filePath);
    let shared = 0;
    while (shared < aSegs.length && shared < bSegs.length && aSegs[shared] === bSegs[shared]) shared++;
    if (shared > bestShared) {
      bestShared = shared;
      best = n;
      tie = false;
    } else if (shared === bestShared) {
      tie = true;
    }
  }
  return tie ? null : best;
}

async function ngrxEffectEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // Pass 1 — action key → effect nodes, scanning only .ts files that mention effects.
  // `.ts` files containing `.dispatch(` are recorded here so pass 2 re-reads just those
  // (plus any non-.ts JS-family dispatch file) instead of every file again — the spring
  // publisherFiles pattern.
  const effectsByAction = new Map<string, Node[]>();
  const tsDispatchFiles: string[] = [];
  const tsSelectFiles: string[] = [];
  const register = (key: string | null, node: Node): void => {
    if (!key) return;
    let arr = effectsByAction.get(key);
    if (!arr) { arr = []; effectsByAction.set(key, arr); }
    arr.push(node);
  };
  const registerOfTypes = (src: string, node: Node): void => {
    NGRX_OFTYPE_RE.lastIndex = 0;
    let om: RegExpExecArray | null;
    while ((om = NGRX_OFTYPE_RE.exec(src))) {
      for (const arg of om[1]!.split(',')) register(ngrxActionKey(arg), node);
    }
  };
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!NGRX_TS_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content) continue;
    if (content.includes('.dispatch(')) tsDispatchFiles.push(file);
    // `.select(` reads count only in files that import @ngrx — that gate keeps
    // Akita-style `store.select` APIs and unrelated `.select` protocols out.
    if (content.includes('.select(') && content.includes('@ngrx')) tsSelectFiles.push(file);
    if (!content.includes('@ngrx/effects') && !content.includes('ofType(') && !content.includes('createEffect(')) continue;
    const lines = content.split('\n');
    const safe = stripCommentsForRegex(content, 'typescript');
    for (const node of ctx.getNodesInFile(file)) {
      if (node.kind === 'constant') {
        // Functional effect: `export const x$ = createEffect((a$ = inject(Actions)) => …)`.
        // The initializer (captured in `signature`) gates reading the body — the
        // redux-thunk precedent.
        if (!node.signature || !NGRX_CREATE_EFFECT_RE.test(node.signature)) continue;
        const span = sliceLines(safe, node.startLine, node.endLine);
        if (span) registerOfTypes(span, node);
        continue;
      }
      if (node.kind !== 'method' && node.kind !== 'property') continue;
      // Class-field `x$ = createEffect(() => …)` extracts as `method` (the call arg holds
      // an arrow fn); legacy `@Effect() x$ = actions$.pipe(ofType(…))` is a `property`
      // (its pipe args are call expressions, not direct arrows).
      const span = sliceLines(safe, node.startLine, node.endLine);
      if (!span) continue;
      let isEffect = NGRX_CREATE_EFFECT_RE.test(span) || NGRX_EFFECT_ANNO_RE.test(span);
      if (!isEffect) {
        // The decorator can sit on lines just above the member's startLine.
        for (let i = node.startLine - 2; i >= 0 && i >= node.startLine - 1 - NGRX_ANNO_LOOKBACK; i--) {
          const t = (lines[i] ?? '').trim();
          if (NGRX_EFFECT_ANNO_RE.test(t)) isEffect = true;
          if (!t.startsWith('@')) break; // past the decorator block → stop
        }
      }
      if (!isEffect) continue;
      registerOfTypes(span, node);
    }
  }
  if (!effectsByAction.size && !tsSelectFiles.length) return [];

  // Non-.ts JS-family dispatch/select files weren't read in pass 1 — find them
  // now (rare: NgRx is a TypeScript ecosystem, but a mixed repo may use .js).
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (NGRX_TS_EXT.test(file) || !NGRX_JS_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content) continue;
    if (content.includes('.dispatch(')) tsDispatchFiles.push(file);
    if (content.includes('.select(') && content.includes('@ngrx')) tsSelectFiles.push(file);
  }

  const edges: Edge[] = [];
  const seen = new Set<string>();
  // `localName` → the file the import resolves to (`importedFrom` resolves the
  // module specifier the way the import resolver does — binding rows carry no
  // resolvedPath). Cached per file; a file with no usable mappings resolves
  // unpinned.
  const importCache = new Map<string, Map<string, string>>();
  const importsOf = (file: string): Map<string, string> => {
    let m = importCache.get(file);
    if (!m) {
      try {
        m = importedFrom(ctx, file, file.endsWith('x') ? 'tsx' : 'typescript');
      } catch {
        m = new Map();
      }
      importCache.set(file, m);
    }
    return m;
  };

  // Read pass — link each `*.store.select(sel)`/`selectSignal(sel)` call → the
  // selector node it reads. Effects (`concatLatestFrom`/`withLatestFrom` args),
  // components and guards all make the same hop; the source is the enclosing
  // node at the call line. Resolution is selector-gated and import-pinned; an
  // ambiguous name declines.
  for (const file of tsSelectFiles) {
    if ((++scannedFiles & 15) === 0) await onYield();
    const content = ctx.readFile(file);
    if (!content) continue;
    const safe = stripCommentsForRegex(content, 'typescript');
    const lineAt = makeLineAt(safe, 1);
    const nodesInFile = ctx.getNodesInFile(file);
    const imports = importsOf(file);
    NGRX_SELECT_RE.lastIndex = 0;
    let sm: RegExpExecArray | null;
    let selectAdded = 0;
    while ((sm = NGRX_SELECT_RE.exec(safe)) && selectAdded < NGRX_FANOUT_CAP) {
      if (!NGRX_RECEIVER_RE.test(sm[1]!)) continue; // not a store receiver
      const sel = ngrxActionKey(sm[2]!);
      if (!sel) continue;
      const line = lineAt(sm.index);
      const reader =
        enclosingFn(nodesInFile, line) ?? enclosingValue(nodesInFile, line) ?? enclosingProperty(nodesInFile, line);
      if (!reader) continue;
      const target = resolveNgrxSelector(ctx, file, sm[2]!.trim(), imports);
      if (!target || target.id === reader.id) continue;
      const dedupKey = `${reader.id}>${target.id}`;
      if (seen.has(dedupKey)) continue;
      seen.add(dedupKey);
      edges.push({
        source: reader.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'ngrx-select', via: sel, readAt: `${file}:${line}` },
      });
      selectAdded++;
    }
  }
  for (const file of effectsByAction.size ? tsDispatchFiles : []) {
    if ((++scannedFiles & 15) === 0) await onYield();
    const content = ctx.readFile(file);
    if (!content || !content.includes('.dispatch(')) continue;
    const safe = stripCommentsForRegex(content, 'typescript');
    const safeLines = safe.split('\n');
    const lineAt = makeLineAt(safe, 1);
    const nodesInFile = ctx.getNodesInFile(file);
    NGRX_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    let added = 0;
    while ((m = NGRX_DISPATCH_RE.exec(safe)) && added < NGRX_FANOUT_CAP) {
      if (!NGRX_RECEIVER_RE.test(m[1]!)) continue; // not a store (dispatchEvent, dispatcher, …)
      const line = lineAt(m.index);
      // enclosingFn covers methods/functions/components (incl. class-field effects);
      // enclosingValue reaches a dispatch written inside a functional-effect constant;
      // enclosingProperty covers a class-field dispatch (`x = this.store.dispatch(…)`).
      const disp =
        enclosingFn(nodesInFile, line) ?? enclosingValue(nodesInFile, line) ?? enclosingProperty(nodesInFile, line);
      if (!disp) continue;
      const key = m[3] ? m[3]! : m[4] ? ngrxActionKey(m[4]!) : resolveNgrxDispatchArg(m[5]!, safeLines, disp.startLine, line);
      if (!key) continue;
      const targets = effectsByAction.get(key);
      if (!targets) continue;
      for (const target of targets) {
        if (target.id === disp.id) continue;
        const dedupKey = `${disp.id}>${target.id}`;
        if (seen.has(dedupKey)) continue;
        seen.add(dedupKey);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'ngrx-dispatch', via: key, registeredAt: target.id },
        });
        added++;
      }
    }
  }
  return edges;
}

// ── Sidekiq job dispatch (Ruby) ───────────────────────────────────────────────
// Sidekiq decouples a job's enqueue site from the worker's `perform`, linked by the WORKER
// CLASS NAME:
//   # app/workers/destroy_user_worker.rb
//   class DestroyUserWorker
//     include Sidekiq::Worker          # or Sidekiq::Job (the modern alias)
//     def perform(user_id) … end
//   # app/services/… — a DIFFERENT file
//   DestroyUserWorker.perform_async(user.id)   # also .perform_in(t, …) / .perform_at(t, …)
// Bridge it: link the enclosing method at each `Worker.perform_async/_in/_at(…)` site → that
// worker's instance `perform`. Name-keyed (like Celery): the receiver class must be a Sidekiq
// worker — gated by reading `include Sidekiq::Job|Worker` from the class body, since that mixin
// is an external gem module that forms no resolvable edge. ActiveJob's `perform_later`/`_now` is
// a different shape and deliberately not matched, so an ActiveJob-only app yields 0.
const SIDEKIQ_DISPATCH_RE = /([A-Z][A-Za-z0-9_]*(?:::[A-Z][A-Za-z0-9_]*)*)\s*\.\s*perform_(?:async|in|at)\b/g;
const SIDEKIQ_WORKER_RE = /\binclude\s+(?:::)?Sidekiq::(?:Job|Worker)\b/;
// `class Foo < Bar` — the superclass clause. Applied to the class's DECL line
// only, so a nested `class Inner < X` in the body can't pose as the outer
// class's superclass.
const SIDEKIQ_SUPERCLASS_RE = /\bclass\s+[A-Z][\w:]*\s*<\s*(?:::)?([A-Z][A-Za-z0-9_:]*)/;
// `Sidekiq::Client.push('class' => 'Worker', …)` — dispatch keyed by the job
// payload's class name STRING (also `class: 'W'` / `:class => 'W'` / a `W`
// class literal). The `Sidekiq::Client` receiver + the literal `class` key gate
// it to the canonical serialized form.
const SIDEKIQ_PUSH_RE = /\bSidekiq::Client\.push(?:_bulk)?\s*\(\s*\{?\s*(?:['"]class['"]\s*=>|:class\s*=>|class\s*:)\s*(?:['"]([A-Z][A-Za-z0-9_:]*)['"]|([A-Z][A-Za-z0-9_:]*)\b)/g;
// `Jobs.enqueue(:job_sym)`/`Jobs.enqueue(JobClass)` — the Discourse job facade,
// which constantizes `"::Jobs::#{sym.camelcase}"` and pushes it onto Sidekiq.
// `enqueue_in`/`enqueue_at` take the job ref as the SECOND arg (a delay first).
// The receiver is the top-level `Jobs` module — `::Jobs` qualifies, a nested
// `Foo::Jobs` does not. The job class's entry point is `execute` (Discourse's
// `Jobs::Base#perform` is the wrapper), not `perform` itself.
const SIDEKIQ_JOBS_ENQUEUE_RE = /(?:^|[^\w:])(?:::)?Jobs\.enqueue\s*\(\s*(?::([a-zA-Z_]\w*)|([A-Z][A-Za-z0-9_:]*))/g;
const SIDEKIQ_JOBS_ENQUEUE_TIMED_RE = /(?:^|[^\w:])(?:::)?Jobs\.enqueue_(?:in|at)\s*\([^,\n]+,\s*(?::([a-zA-Z_]\w*)|([A-Z][A-Za-z0-9_:]*))/g;
const SIDEKIQ_RB_EXT = /\.rb$/;
const SIDEKIQ_FANOUT_CAP = 80;
const SIDEKIQ_SUPER_MAX_DEPTH = 8; // bound on the worker superclass walk

async function sidekiqDispatchEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // class node → the class BODY source (memoized per file read through ctx).
  const bodyOf = (cls: Node): string => {
    const content = ctx.readFile(cls.filePath);
    if (!content) return '';
    const end = cls.endLine ?? cls.startLine;
    return content.split('\n').slice(cls.startLine - 1, end).join('\n');
  };
  const performIn = (cls: Node): Node | null => {
    const end = cls.endLine ?? cls.startLine;
    return ctx.getNodesInFile(cls.filePath).find(
      (n) => n.kind === 'method' && n.name === 'perform' && n.startLine >= cls.startLine && n.startLine <= end
    ) ?? null;
  };
  // Resolve a class NAME to class nodes — namespaced by qualified name, else
  // simple-name candidates (used for both worker refs and superclass lookups).
  const classNamed = (ref: string): Node[] => {
    if (ref.includes('::')) {
      const q = ctx.getNodesByQualifiedName(ref).filter((n) => n.kind === 'class');
      if (q.length) return q;
    }
    return ctx.getNodesByName(ref.split('::').pop()!).filter((n) => n.kind === 'class');
  };

  // class node id → its instance `perform` method (null if the class isn't a Sidekiq worker),
  // memoized. A class is a worker when its own body includes `Sidekiq::Job|Worker` — OR when
  // a SUPERCLASS does (`class Foo < BaseWorker` where only `BaseWorker` has the include —
  // diaspora). The chain walk is bounded and cycle-guarded; the dispatched perform resolves
  // to the NEAREST `perform` on the chain (the subclass's own override wins, else the
  // inherited ancestor's).
  // The superclass a class's decl line names — resolved to a UNIQUE class node
  // (a same-named-superclass collision can't prove which `< Base` is meant, so
  // it bails rather than guess — precision over recall).
  const superclassOf = (cls: Node): Node | null => {
    const firstLine = bodyOf(cls).split('\n')[0] ?? '';
    const sup = SIDEKIQ_SUPERCLASS_RE.exec(firstLine)?.[1];
    if (!sup) return null;
    const cands = classNamed(sup);
    return cands.length === 1 ? cands[0]! : null;
  };
  const performCache = new Map<string, Node | null>();
  const isWorker = (cls: Node, seen: Set<string>): boolean => {
    if (seen.has(cls.id) || seen.size > SIDEKIQ_SUPER_MAX_DEPTH) return false;
    seen.add(cls.id);
    if (SIDEKIQ_WORKER_RE.test(bodyOf(cls))) return true;
    const sup = superclassOf(cls);
    return sup !== null && isWorker(sup, seen);
  };
  const performOf = (cls: Node): Node | null => {
    let v = performCache.get(cls.id);
    if (v !== undefined) return v;
    v = null;
    if (isWorker(cls, new Set())) {
      // Nearest perform wins: own override, else the closest worker ancestor's.
      const seen = new Set<string>([cls.id]);
      let cur: Node | null = cls;
      while (cur && seen.size <= SIDEKIQ_SUPER_MAX_DEPTH) {
        v = performIn(cur);
        if (v) break;
        cur = superclassOf(cur);
        if (cur) {
          if (seen.has(cur.id)) break;
          seen.add(cur.id);
        }
      }
    }
    performCache.set(cls.id, v);
    return v;
  };

  // Resolve a (possibly namespaced) worker reference to its `perform`. A namespaced ref is
  // matched by EXACT qualified name first, so same-named workers in different namespaces
  // (forem has four `SendEmailNotificationWorker`s) resolve to the right one; an unqualified
  // ref falls back to the simple name and links only when a single worker bears it — an
  // ambiguous collision bails (precision over recall).
  const resolve = (ref: string): Node | null => {
    if (ref.includes('::')) {
      const q = ctx.getNodesByQualifiedName(ref).find((n) => n.kind === 'class' && performOf(n));
      if (q) return performOf(q);
    }
    const workers = ctx.getNodesByName(ref.split('::').pop()!).filter((n) => n.kind === 'class' && performOf(n));
    return workers.length === 1 ? performOf(workers[0]!) : null;
  };

  // The `Jobs.enqueue` facade (Discourse): the symbol is constantized as
  // `"::Jobs::#{sym.camelcase}"` and the class's entry method is `execute`
  // (`Jobs::Base#perform` is the framework wrapper around it). Nearest method
  // of `name` on the superclass chain wins; the class must be a worker
  // (`Jobs::Base` itself carries the `Sidekiq::Worker` include, so every
  // `class Jobs::X < Jobs::Base` qualifies through the chain).
  const methodOnChain = (cls: Node, name: string): Node | null => {
    const seen = new Set<string>([cls.id]);
    let cur: Node | null = cls;
    while (cur && seen.size <= SIDEKIQ_SUPER_MAX_DEPTH) {
      const end = cur.endLine ?? cur.startLine;
      const found = ctx.getNodesInFile(cur.filePath).find(
        (n) => n.kind === 'method' && n.name === name && n.startLine >= cur!.startLine && n.startLine <= end,
      );
      if (found) return found;
      cur = superclassOf(cur);
      if (cur) {
        if (seen.has(cur.id)) break;
        seen.add(cur.id);
      }
    }
    return null;
  };
  const camelize = (sym: string): string =>
    sym
      .split('/')
      .map((seg) => seg.split('_').map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(''))
      .join('::');
  const resolveJob = (sym: string | undefined, cls: string | undefined): Node | null => {
    const ref = sym ? `Jobs::${camelize(sym)}` : cls!;
    for (const cand of classNamed(ref)) {
      if (isWorker(cand, new Set())) return methodOnChain(cand, 'execute') ?? methodOnChain(cand, 'perform');
    }
    return null;
  };

  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!SIDEKIQ_RB_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || (!/\.perform_(?:async|in|at)\b/.test(content) && !content.includes('Sidekiq::Client.push') && !/\bJobs\.enqueue/.test(content))) continue;
    const safe = stripCommentsForRegex(content, 'ruby');
    const nodesInFile = ctx.getNodesInFile(file);
    const lineAt = makeLineAt(safe, 1);
    let added = 0;
    SIDEKIQ_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = SIDEKIQ_DISPATCH_RE.exec(safe)) && added < SIDEKIQ_FANOUT_CAP) {
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const target = resolve(m[1]!);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'sidekiq-dispatch', via: m[1]!, registeredAt: `${file}:${line}` },
      });
      added++;
    }
    // `Sidekiq::Client.push('class' => 'W', …)` — dispatch keyed by the class
    // name string in the job payload. The `Sidekiq::Client` receiver + the
    // literal `'class'` key gate it to the canonical serialized form.
    SIDEKIQ_PUSH_RE.lastIndex = 0;
    while ((m = SIDEKIQ_PUSH_RE.exec(safe)) && added < SIDEKIQ_FANOUT_CAP) {
      const line = lineAt(m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      const target = resolve(m[1] ?? m[2]!);
      if (!target || target.id === disp.id) continue;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'sidekiq-dispatch', via: m[1] ?? m[2]!, registeredAt: `${file}:${line}` },
      });
      added++;
    }
    // `Jobs.enqueue(:sym)`/`Jobs.enqueue_in(N, :sym)`/`Jobs.enqueue_at(t, :sym)`
    // (Discourse): the job ref is a SYMBOL constantized as `Jobs::<camelcase>`,
    // or a class literal. Target is the job's `execute` (else `perform`) — the
    // enclosing method is the dispatch source, as with `perform_async` sites.
    for (const re of [SIDEKIQ_JOBS_ENQUEUE_RE, SIDEKIQ_JOBS_ENQUEUE_TIMED_RE]) {
      re.lastIndex = 0;
      while ((m = re.exec(safe)) && added < SIDEKIQ_FANOUT_CAP) {
        const line = lineAt(m.index);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp) continue;
        const target = resolveJob(m[1], m[2]);
        if (!target || target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'sidekiq-dispatch', via: m[1] ? `:${m[1]}` : m[2]!, registeredAt: `${file}:${line}` },
        });
        added++;
      }
    }
  }
  return edges;
}

// ── Erlang behaviour-callback dispatch ────────────────────────────────────────
// An Erlang behaviour is a compile-checked callback contract: the behaviour
// module declares `-callback init(...) -> ...`, implementers declare
// `-behaviour(B)` and export the callbacks, and the framework side dispatches
// through a VARIABLE module — cowboy's `Handler:init(Req, Opts)` and
// `Middleware:execute(Req, Env)` folds, ejabberd's `Mod:start/2`. Extraction
// deliberately leaves var-module calls silent (no static target), so the flow
// breaks at exactly the hop agents ask about (request → handler init). Bridge:
//
//   dispatch site `Var:fn(args…)` → every in-repo implementer of the behaviour
//   declaring `fn` with the SITE's arity — provided exactly ONE in-repo
//   behaviour declares (fn, arity); a name+arity collision across behaviours
//   bails (silent beats wrong) — and the implementer defines and exports `fn`.
//
// Behaviours are discovered by scanning every Erlang file for `-callback`
// declarations (not just `implements` targets), so a behaviour with zero
// implementers still participates in the ambiguity gate. Fan-out control: a
// mega-behaviour (ejabberd's gen_mod, ~200 mod_* implementers) would mint
// hundreds of edges per site that READ as complete coverage while being
// arbitrary — above the cap the site is skipped entirely and the boundary
// stays visibly dynamic (explore's boundary announcer covers it) instead of
// silently truncated.
const ERLANG_EXT = /\.(?:erl|hrl)$/;
// `Var:fn(` — variable (capitalized) module, lowercase function, immediate
// open-paren. The leading char class rejects `?MODULE:fn(` (macro), `a:b(`
// (static remote call, already linked), and mid-word matches.
const ERLANG_DISPATCH_RE = /(^|[^?\w@'])([A-Z][A-Za-z0-9_@]*):([a-z][A-Za-z0-9_@]*)\(/g;
const ERLANG_CALLBACK_DECL_RE = /(^|\n)\s*-callback\s+('[^'\n]+'|[a-z][A-Za-z0-9_@]*)\s*\(/g;
const ERLANG_BEHAVIOUR_FANOUT_CAP = 24;
// ── gen_server registered-name dispatch ─────────────────────────────────────
// `gen_server:call(Name, Req)`/`cast` dispatch by REGISTERED NAME, not by
// module reference: the server registered `Name`→pid at
// `gen_server:start*({local|global, Name}, Mod, …)`, and the dominant style
// registers under the module's own name (the `atom == module name`
// convention, often written `?MODULE`). The target callback is fixed by OTP:
// `call`/`multi_call` → `handle_call/3`, `cast`/`abcast` → `handle_cast/2`.
// Precision gates: the resolved module must declare `-behaviour(gen_server)`
// (or `-behavior`) AND define+export the callback; a name registered by two
// different modules, or one that resolves to nothing in-repo, stays silent.
// `-behaviour(gen_server)`/`(-behavior)` — the OTP gen_server implementer gate.
const ERLANG_GENSERVER_BEHAVIOUR_RE = /(^|\n)\s*-behaviou?r\s*\(\s*(?:'gen_server'|gen_server)\s*\)/;
// Registration: `gen_server:start|start_link|start_monitor({local|global,
// Name}, Mod, …)` — binds the registered Name to the callback module (`?MODULE`
// resolves to the file's own module).
const ERLANG_GENSERVER_REG_RE =
  /\bgen_server\s*:\s*start(?:_link|_monitor)?\s*\(\s*\{\s*(?:local|global)\s*,\s*(?:'([^'\n]+)'|(\?MODULE|[a-z][A-Za-z0-9_@]*))\s*\}\s*,\s*(\?MODULE|'[^'\n]+'|[a-z][A-Za-z0-9_@]*)/g;
// Dispatch: `gen_server:call|cast|abcast|multi_call(Ref, …)` where Ref is a
// bare/quoted atom, `?MODULE`, or `{local|global, Name}` (the `{via,…}` form
// routes through a registry module — statically opaque, deliberately not
// matched). A Capitalized first arg is a variable — unresolvable, skipped.
const ERLANG_GENSERVER_CALL_RE =
  /\bgen_server\s*:\s*(call|cast|abcast|multi_call)\s*\(\s*(\?MODULE|\{\s*(?:local|global)\s*,\s*(?:'[^'\n]+'|\?MODULE|[a-z][A-Za-z0-9_@]*)\s*\}|'[^'\n]+'|[a-z][A-Za-z0-9_@]*)/g;
const ERLANG_GENSERVER_FILE_CAP = 80;

/** Arity suffix of an Erlang function qualifiedName (`mod::fn/2` → 2, #1610). */
function erlangQnArity(qn: string): number {
  const m = /\/(\d{1,3})$/.exec(qn);
  return m ? Number(m[1]) : -1;
}

/**
 * Argument count of the call/declaration whose `(` sits at `openIdx` —
 * top-level commas + 1, `()` → 0, unbalanced/oversized → -1. Skips nested
 * (), [], {}, <<>> content, `"strings"`, `'atoms'`, and `$c` char literals,
 * so `-callback init(fun((a, b) -> ok), #{k => v}) -> ok.` counts 2.
 */
function erlangArityAt(src: string, openIdx: number): number {
  let depth = 1;
  // `<<1,2,3>>` binary literals: commas inside are element separators, not
  // argument separators. Tracked separately from bracket depth because the
  // single-char `<`/`>` comparison operators must stay inert (#1358).
  let binDepth = 0;
  let commas = 0;
  let sawArg = false;
  const limit = Math.min(src.length, openIdx + 4000);
  for (let i = openIdx + 1; i < limit; i++) {
    const ch = src[i]!;
    if (ch === '"' || ch === "'") {
      i++;
      while (i < limit && src[i] !== ch) {
        if (src[i] === '\\') i++;
        i++;
      }
      sawArg = true;
      continue;
    }
    if (ch === '$') {
      i++;
      if (src[i] === '\\') i++;
      sawArg = true;
      continue;
    }
    if (ch === '<' && src[i + 1] === '<') { binDepth++; i++; sawArg = true; continue; }
    if (ch === '>' && src[i + 1] === '>' && binDepth > 0) { binDepth--; i++; continue; }
    if (ch === '(' || ch === '[' || ch === '{') { depth++; sawArg = true; continue; }
    if (ch === ')' || ch === ']' || ch === '}') {
      depth--;
      if (depth === 0) return sawArg ? commas + 1 : 0;
      continue;
    }
    if (ch === ',' && depth === 1 && binDepth === 0) { commas++; continue; }
    if (!/\s/.test(ch)) sawArg = true;
  }
  return -1;
}

/**
 * Nix module-system option wiring. A NixOS/home-manager/nix-darwin option is
 * DECLARED in one module (`options.launchd.user.agents = mkOption { ... }`)
 * and SET in others (`launchd.user.agents.yabai = { ... }` inside a module's
 * config) — the connection happens by option-path unification inside the
 * module-system evaluator, so there is no static call/import edge to follow
 * and flow questions ("how does services.yabai.enable become a launchd
 * service?") go dark at the module boundary.
 *
 * This pass links each config-write binding to the option declaration whose
 * path is the longest static-segment prefix of the write path. Precision gates:
 *  - only STATIC segments participate: plain identifiers, plus quoted segments
 *    (`"git/config"`, `"com.apple.dock"`) as opaque verbatim tokens that match
 *    only quote-exactly; an interpolated (`${name}`) segment ends the prefix,
 *    so dynamic paths never match beyond their static head;
 *  - matched prefixes must be ≥2 segments: 1-segment paths would wrongly link
 *    every package's `meta = { ... }` attrset to nixos's `options.meta`;
 *  - a prefix declared in more than one file is ambiguous → no edge (a wrong
 *    edge is worse than none);
 *  - writes physically inside an options block are declaration internals
 *    (types, defaults, examples), never config writes → excluded.
 * Both declaration spellings register: flat (`options.a.b = ...`) by name, and
 * nested (`options = { a.b = ...; }`) by line-span containment.
 */
function nixLeadingPlainSegments(name: string): string[] {
  const segs: string[] = [];
  let i = 0;
  const n = name.length;
  while (i < n) {
    if (name[i] === '"') {
      // Quoted segment — an opaque verbatim token (quotes kept, so it can
      // never collide with a plain identifier). `NSGlobalDomain."com.apple.
      // mouse.tapBehavior"` must match ITS OWN quoted declaration, not
      // whichever sibling registered the shared plain prefix first.
      let j = i + 1;
      while (j < n && name[j] !== '"') {
        if (name[j] === '\\') j++;
        j++;
      }
      if (j >= n) return segs; // unterminated — stop at the static head
      const tok = name.slice(i, j + 1);
      if (tok.includes('${')) return segs; // interpolated → dynamic → stop
      segs.push(tok);
      i = j + 1;
      if (i >= n) break;
      if (name[i] !== '.') return segs;
      i++;
      continue;
    }
    let j = i;
    while (j < n && name[j] !== '.') {
      if (name[j] === '"' || (name[j] === '$' && name[j + 1] === '{')) return segs;
      j++;
    }
    const seg = name.slice(i, j);
    if (!/^[A-Za-z_][A-Za-z0-9_'-]*$/.test(seg)) return segs;
    segs.push(seg);
    i = j + 1;
  }
  return segs;
}

async function nixOptionPathEdges(queries: QueryBuilder, onYield: MaybeYield): Promise<Edge[]> {
  let scanned255 = 0;
  type Rec = { id: string; filePath: string; startLine: number; endLine: number; segs: string[] };

  // One streaming pass over nix bindings (variables + the odd function-valued
  // option); memory stays O(bindings-kept), not O(all nodes) (#610).
  const byFile = new Map<string, Rec[]>();
  let scanned = 0;
  for (const kind of ['variable', 'function'] as NodeKind[]) {
    for (const node of queries.iterateNodesByKind(kind)) {
      if ((++scanned255 & 63) === 0) await onYield();
      if ((++scanned & 0x3fff) === 0 && onYield) await onYield();
      if (node.language !== 'nix') continue;
      const segs = nixLeadingPlainSegments(node.name);
      if (segs.length === 0) continue;
      const rec: Rec = {
        id: node.id,
        filePath: node.filePath,
        startLine: node.startLine,
        endLine: node.endLine,
        segs,
      };
      const arr = byFile.get(node.filePath);
      if (arr) arr.push(rec);
      else byFile.set(node.filePath, [rec]);
    }
  }

  // Per file: walk bindings outermost-first with a stack of active option
  // spans, composing nested declaration paths (`options = { services.foo = {
  // enable = mkOption ...; }; }` registers services.foo AND services.foo.enable).
  // An `options` binding nested inside another option span is a SUBMODULE's
  // own namespace (`attrsOf (submodule { options = ...; })`) — its internals
  // are not globally addressable, so the sentinel blocks registration below it
  // while still excluding the region from write candidates.
  const SUBMODULE = '\u0000submodule';
  const decls = new Map<string, Rec[]>();
  const writes: Rec[] = [];
  const register = (path: string[], rec: Rec) => {
    if (path.length < 2 || path.includes(SUBMODULE)) return;
    const key = path.join('.');
    const arr = decls.get(key);
    if (arr) arr.push(rec);
    else decls.set(key, [rec]);
  };
  for (const recs of byFile.values()) {
    recs.sort((a, b) => a.startLine - b.startLine || b.endLine - a.endLine);
    const stack: Array<{ start: number; end: number; prefix: string[] }> = [];
    for (const rec of recs) {
      while (stack.length > 0 && stack[stack.length - 1]!.end < rec.startLine) stack.pop();
      // Strict containment at line granularity: a one-line nested binding is
      // indistinguishable from its container, so it stays unclassified (rare
      // in module code, where option blocks are multi-line).
      const enclosing =
        stack.length > 0 &&
        rec.startLine >= stack[stack.length - 1]!.start &&
        rec.endLine <= stack[stack.length - 1]!.end &&
        !(rec.startLine === stack[stack.length - 1]!.start && rec.endLine === stack[stack.length - 1]!.end)
          ? stack[stack.length - 1]!
          : null;

      if (rec.segs[0] === 'options') {
        const ownPath = rec.segs.slice(1); // [] for the bare `options = { ... }` spelling
        const prefix = enclosing ? [SUBMODULE] : ownPath;
        register(prefix, rec);
        stack.push({ start: rec.startLine, end: rec.endLine, prefix });
        continue;
      }
      if (enclosing) {
        const composed = [...enclosing.prefix, ...rec.segs];
        register(composed, rec);
        stack.push({ start: rec.startLine, end: rec.endLine, prefix: composed });
        continue;
      }
      if (rec.segs.length >= 2) {
        writes.push(rec);
      }
    }
  }
  if (decls.size === 0 || writes.length === 0) return [];

  const edges: Edge[] = [];
  for (const w of writes) {
    // `config.services.x = ...` spells the same write with an explicit prefix.
    const segs = w.segs[0] === 'config' ? w.segs.slice(1) : w.segs;
    if (segs.length < 2) continue;
    // Longest prefix wins; an ambiguous longest match does NOT fall back to a
    // shorter one (that would link `services.nginx.virtualHosts.x` to
    // `options.services.nginx` when virtualHosts is the contested path).
    for (let len = Math.min(segs.length, 6); len >= 2; len--) {
      const candidates = decls.get(segs.slice(0, len).join('.'));
      if (!candidates || candidates.length === 0) continue;
      const files = new Set(candidates.map((c) => c.filePath));
      if (files.size === 1) {
        const target = candidates[0]!;
        if (target.id !== w.id) {
          edges.push({
            source: w.id,
            target: target.id,
            kind: 'references',
            line: w.startLine,
            provenance: 'heuristic',
            metadata: {
              synthesizedBy: 'nix-option-path',
              optionPath: segs.slice(0, len).join('.'),
              registeredAt: `${target.filePath}:${target.startLine}`,
            },
          });
        }
      }
      break; // longest hit decides, matched or ambiguous
    }
  }
  return edges;
}

async function erlangBehaviourDispatchEdges(queries: QueryBuilder, ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  let scanned255 = 0;
  // Cheap language gate: no Erlang modules → no cost beyond one streamed
  // kind scan (never a materialized array of every namespace — #1212).
  const erlangModules: Node[] = [];
  for (const n of queries.iterateNodesByKind('namespace')) {
    if ((++scanned255 & 63) === 0) await onYield();
    if (n.language === 'erlang') erlangModules.push(n);
  }
  if (erlangModules.length === 0) return [];

  // Pass 1 — scan every Erlang file with `-callback` decls: behaviour module →
  // its (name, arity) callback set, and the global `name/arity` → declaring
  // behaviours map that drives the ambiguity gate.
  const moduleByFile = new Map<string, Node>();
  for (const ns of erlangModules) {
    if (!moduleByFile.has(ns.filePath)) moduleByFile.set(ns.filePath, ns);
  }
  const declaringBehaviours = new Map<string, Node[]>(); // `fn/arity` → behaviour namespaces
  const callbackNames = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!ERLANG_EXT.test(file)) continue;
    const behaviour = moduleByFile.get(file);
    if (!behaviour) continue; // a .hrl or module-less file can't be a behaviour
    const content = ctx.readFile(file);
    if (!content || !content.includes('-callback')) continue;
    const safe = stripCommentsForRegex(content, 'erlang');
    ERLANG_CALLBACK_DECL_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = ERLANG_CALLBACK_DECL_RE.exec(safe))) {
      const name = m[2]!.replace(/^'|'$/g, '');
      const arity = erlangArityAt(safe, m.index + m[0].length - 1);
      if (arity < 0) continue;
      const key = `${name}/${arity}`;
      const arr = declaringBehaviours.get(key);
      if (arr) {
        if (!arr.some((b) => b.id === behaviour.id)) arr.push(behaviour);
      } else {
        declaringBehaviours.set(key, [behaviour]);
      }
      callbackNames.add(name);
    }
  }
  const edges: Edge[] = [];
  const seen = new Set<string>();

  // ── gen_server registered-name dispatch ─────────────────────────────────
  // Independent of the -callback machinery: a gen_server module usually has NO
  // `-callback` decls of its own (they live in OTP, out-of-repo), so this pass
  // must run before the behaviour early-return below. Two sweeps: (A) index
  // `gen_server:start*({local|global, Name}, Mod, …)` registrations and the
  // modules declaring `-behaviour(gen_server)`; (B) link each
  // `gen_server:call|cast(Name, …)` site → the server's `handle_call/3` /
  // `handle_cast/2`.
  const genServerModules = new Map<string, Node>(); // module name → namespace node
  const registeredNames = new Map<string, Set<string>>(); // regName → module names
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!ERLANG_EXT.test(file)) continue;
    const mod = moduleByFile.get(file);
    if (!mod) continue; // a module-less .hrl can't be a gen_server
    const content = ctx.readFile(file);
    if (!content || !content.includes('gen_server')) continue;
    const safe = stripCommentsForRegex(content, 'erlang');
    if (ERLANG_GENSERVER_BEHAVIOUR_RE.test(safe)) genServerModules.set(mod.name, mod);
    ERLANG_GENSERVER_REG_RE.lastIndex = 0;
    let rm: RegExpExecArray | null;
    while ((rm = ERLANG_GENSERVER_REG_RE.exec(safe))) {
      const regName = rm[1] ?? (rm[2] === '?MODULE' ? mod.name : rm[2]!);
      const rawMod = rm[3]!;
      const modName = rawMod === '?MODULE' ? mod.name : rawMod.replace(/^'|'$/g, '');
      let set = registeredNames.get(regName);
      if (!set) { set = new Set(); registeredNames.set(regName, set); }
      set.add(modName);
    }
  }
  if (genServerModules.size || registeredNames.size) {
    for (const file of ctx.getAllFiles()) {
      if ((++scannedFiles & 15) === 0) await onYield();
      if (!ERLANG_EXT.test(file)) continue;
      const content = ctx.readFile(file);
      if (!content || !content.includes('gen_server:')) continue;
      const safe = stripCommentsForRegex(content, 'erlang');
      const nodesInFile = ctx.getNodesInFile(file);
      const lineAt = makeLineAt(safe, 1);
      const ownMod = moduleByFile.get(file);
      ERLANG_GENSERVER_CALL_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      let added = 0;
      while ((m = ERLANG_GENSERVER_CALL_RE.exec(safe)) && added < ERLANG_GENSERVER_FILE_CAP) {
        const verb = m[1]!;
        const rawRef = m[2]!;
        // Resolve the ref to a NAME: `?MODULE` → this file's module,
        // `{local|global, N}` → N, else the bare/quoted atom itself.
        let name: string | null = null;
        if (rawRef === '?MODULE') name = ownMod?.name ?? null;
        else if (rawRef.startsWith('{')) {
          const inner = /\{\s*(?:local|global)\s*,\s*(?:'([^'\n]+)'|(\?MODULE|[a-z][A-Za-z0-9_@]*))\s*\}/.exec(rawRef);
          if (!inner) continue; // `{via,…}` — registry-module form, opaque
          name = inner[1] ?? (inner[2] === '?MODULE' ? ownMod?.name ?? null : inner[2]!);
        } else name = rawRef.replace(/^'|'$/g, '');
        if (!name) continue;
        // A registered name wins over the module-name convention (it IS the
        // runtime binding); a name registered by two modules is ambiguous —
        // bail rather than guess.
        const regs = registeredNames.get(name);
        if (regs && regs.size > 1) continue;
        const modName = regs && regs.size === 1 ? [...regs][0]! : name;
        const mod = genServerModules.get(modName);
        if (!mod) continue;
        const cb = verb === 'call' || verb === 'multi_call' ? 'handle_call' : 'handle_cast';
        const arity = verb === 'call' || verb === 'multi_call' ? 3 : 2;
        const target = ctx
          .getNodesInFile(mod.filePath)
          .find(
            (n) =>
              n.kind === 'function' &&
              n.name === cb &&
              erlangQnArity(n.qualifiedName) === arity &&
              n.isExported !== false,
          );
        if (!target) continue;
        const line = lineAt(m.index);
        const disp = enclosingFn(nodesInFile, line);
        if (!disp) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: {
            synthesizedBy: 'erlang-gen-server',
            via: `${mod.name}:${cb}/${arity}`,
            registeredAt: `${file}:${line}`,
          },
        });
        added++;
      }
    }
  }

  if (declaringBehaviours.size === 0) return edges;

  // Implementer target lookup, lazy per (behaviour, fn, arity): implementers
  // come from the `implements` edges extraction resolved, and the target is
  // the implementer module's own exported `fn` node OF THE SITE'S ARITY —
  // function qualifiedNames carry arity (`mod::fn/2`, #1610), so the arity the
  // dispatch site used selects among same-named definitions.
  const targetCache = new Map<string, Node[]>();
  const targetsOf = (behaviour: Node, fn: string, arity: number): Node[] => {
    const cacheKey = `${behaviour.id}#${fn}/${arity}`;
    let targets = targetCache.get(cacheKey);
    if (targets) return targets;
    targets = [];
    for (const e of queries.getIncomingEdges(behaviour.id, ['implements'])) {
      const impl = queries.getNodeById(e.source);
      if (!impl || impl.language !== 'erlang' || impl.kind !== 'namespace') continue;
      const fnNode = ctx
        .getNodesInFile(impl.filePath)
        .find(
          (n) =>
            n.kind === 'function' &&
            n.name === fn &&
            erlangQnArity(n.qualifiedName) === arity &&
            n.isExported !== false,
        );
      if (fnNode) targets.push(fnNode);
    }
    targetCache.set(cacheKey, targets);
    return targets;
  };

  // Pass 2 — dispatch sites. Only files containing a var-module call shape are
  // scanned in full.
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!ERLANG_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !/[A-Z][A-Za-z0-9_@]*:[a-z]/.test(content)) continue;
    const safe = stripCommentsForRegex(content, 'erlang');
    const nodesInFile = ctx.getNodesInFile(file);
    ERLANG_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = ERLANG_DISPATCH_RE.exec(safe))) {
      const fn = m[3]!;
      if (!callbackNames.has(fn)) continue;
      const openIdx = m.index + m[0].length - 1;
      const arity = erlangArityAt(safe, openIdx);
      if (arity < 0) continue;
      const behaviours = declaringBehaviours.get(`${fn}/${arity}`);
      if (!behaviours || behaviours.length !== 1) continue; // unknown or ambiguous
      const behaviour = behaviours[0]!;
      const targets = targetsOf(behaviour, fn, arity);
      if (targets.length === 0 || targets.length > ERLANG_BEHAVIOUR_FANOUT_CAP) continue;
      const line = lineOf(safe, m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      for (const target of targets) {
        if (target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: {
            synthesizedBy: 'erlang-behaviour',
            via: `${behaviour.name}:${fn}/${arity}`,
            registeredAt: `${file}:${line}`,
          },
        });
      }
    }
  }
  return edges;
}

// ── Laravel events (PHP) ──────────────────────────────────────────────────────
// Laravel decouples an event dispatch from its listener(s), linked by the EVENT CLASS:
//   // app/Events/PlaybackStarted.php  +  app/Listeners/UpdateLastfmNowPlaying.php
//   class UpdateLastfmNowPlaying { public function handle(PlaybackStarted $event) { … } }
//   // a controller / service — a DIFFERENT file
//   event(new PlaybackStarted($song, $user));
// Bridge it: link the enclosing method at each `event(new XEvent(...))` site → every listener's
// `handle` for XEvent. Listeners come from TWO registration mechanisms (both real, both needed):
//   (A) auto-discovery — a `handle(EventType $e)` typed first param (also splits a union A|B);
//   (B) the `protected $listen = [ XEvent::class => [Listener::class, …] ]` map in an
//       EventServiceProvider, which also covers a listener whose `handle()` is UNTYPED.
// Only `event(new X)` is matched — queued JOBS dispatch via `::dispatch()` and their `handle()`
// takes an injected service, never an event type, so jobs are excluded by construction.
const LARAVEL_DISPATCH_RE = /\bevent\s*\(\s*new\s+\\?([A-Za-z_][\w\\]*)/g;
const LARAVEL_PHP_EXT = /\.php$/;
const LARAVEL_FANOUT_CAP = 200;
// A `$listen` entry: `Event::class => [Listener::class, …]`, key/values as `::class` or strings.
const LISTEN_ENTRY_RE = /(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")\s*=>\s*\[([^\]]*)\]/g;
const LISTEN_CLASS_RE = /(?:([A-Za-z_\\][\w\\]*)::class|'([^']+)'|"([^"]+)")/g;
// `Event::listen(X::class, function (…) {…})` — the class-literal first arg is
// the event anchor; the closure may be untyped (`Event::listen` is the facade
// form — `Events::listen`/helper-arg variants are deliberately not matched).
const LARAVEL_LISTEN_KEYED_RE = /\bEvent::listen\s*\(\s*([A-Za-z_\\][\w\\]*)::class\s*,\s*(?:static\s+)?(?:function|fn)\b/g;
// `Event::listen(function (X $e) {…})` — the closure-as-only-arg form (also
// the `fn (X $e) =>` arrow): the event anchor is the closure's TYPED first
// param, union-split like laravelHandleEventTypes. A string-keyed first arg
// (`Event::listen('a.b', fn(X $e))`) is deliberately NOT matched — `event(new X)`
// dispatches by class name, never by a custom string, so that edge could never
// fire. Closures aren't extracted as nodes, so the enclosing method stands in
// as the listener target.
const LARAVEL_LISTEN_CLOSURE_RE = /\bEvent::listen\s*\(\s*(?:static\s+)?(?:function|fn)\s*\(\s*(\??[A-Za-z_\\][\w\\|]*)\s+&?\s*(?:\.\.\.\s*)?\$/g;

/** Short class name from a PHP reference: `\App\Events\Foo` / `App\Events::Foo` → `Foo`. */
function phpSimpleName(s: string): string {
  return s.replace(/^\\/, '').split('\\').pop()!.split('::').pop()!.trim();
}

/** The first-parameter class type(s) of a `handle(...)` declaration — union-split, short-named,
 *  primitives dropped. `handle(A|B $e)` → [A, B]; `handle(string $x)` / `handle()` → []. */
function laravelHandleEventTypes(decl: string): string[] {
  const m = /function\s+handle\s*\(\s*(?:\.\.\.\s*)?(\??[A-Za-z_\\][\w\\|]*)\s+&?\s*(?:\.\.\.\s*)?\$/.exec(decl);
  if (!m) return [];
  return m[1]!
    .replace(/^\?/, '')
    .split('|')
    .map((t) => phpSimpleName(t))
    .filter((t) => /^[A-Z]\w*$/.test(t));
}

/** From an opening `[`, the bracket-balanced body up to its matching `]`. */
function phpArrayBody(src: string, openIdx: number): string | null {
  let depth = 0;
  for (let i = openIdx; i < src.length; i++) {
    if (src[i] === '[') depth++;
    else if (src[i] === ']' && --depth === 0) return src.slice(openIdx + 1, i);
  }
  return null;
}

async function laravelEventEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // event short name → its listener `handle` methods (deduped by node id).
  const listeners = new Map<string, Map<string, Node>>();
  const add = (event: string, handle: Node) => {
    let m = listeners.get(event);
    if (!m) { m = new Map(); listeners.set(event, m); }
    m.set(handle.id, handle);
  };
  const handleOf = (cls: Node): Node | null =>
    ctx
      .getNodesInFile(cls.filePath)
      .find(
        (n) => n.kind === 'method' && n.name === 'handle'
          && n.startLine >= cls.startLine && n.startLine <= (cls.endLine ?? cls.startLine)
      ) ?? null;

  // Pass 1 — build the event→handle map from both registration mechanisms.
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!LARAVEL_PHP_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content) continue;

    // (A) typed listener handles — node-driven, so a commented-out method can't leak in.
    if (content.includes('function handle')) {
      const lines = content.split('\n');
      for (const node of ctx.getNodesInFile(file)) {
        if (node.kind !== 'method' || node.name !== 'handle') continue;
        const decl = lines.slice(node.startLine - 1, node.startLine + 2).join('\n');
        for (const ev of laravelHandleEventTypes(decl)) add(ev, node);
      }
    }

    // (B) the EventServiceProvider `$listen` map — parsed from comment-stripped source so a
    // fully-commented map (firefly's, on auto-discovery) contributes nothing.
    if (content.includes('$listen')) {
      const safe = stripCommentsForRegex(content, 'php');
      const decl = safe.search(/\$listen\s*=\s*\[/);
      const body = decl >= 0 ? phpArrayBody(safe, safe.indexOf('[', decl)) : null;
      if (body) {
        LISTEN_ENTRY_RE.lastIndex = 0;
        let em: RegExpExecArray | null;
        while ((em = LISTEN_ENTRY_RE.exec(body))) {
          const event = phpSimpleName(em[1] ?? em[2] ?? em[3] ?? '');
          LISTEN_CLASS_RE.lastIndex = 0;
          let lm: RegExpExecArray | null;
          while ((lm = LISTEN_CLASS_RE.exec(em[4]!))) {
            const ln = phpSimpleName(lm[1] ?? lm[2] ?? lm[3] ?? '');
            const cls = ctx.getNodesByName(ln).find((n) => n.kind === 'class' && handleOf(n));
            if (cls) add(event, handleOf(cls)!);
          }
        }
      }
    }

    // (C) `Event::listen` closure registrations — the listener is a closure,
    // which extraction doesn't node-ify, so the enclosing METHOD stands in as
    // the listener target (an `Event::listen` outside any method — e.g. a
    // routes file — attributes nothing). Two shapes: class-keyed
    // (`Event::listen(X::class, fn)` — anchor is the class literal) and
    // inferred (`Event::listen(fn(X $e) => …)` — anchor is the typed first
    // param). `Events::listen`, `$events->listen`, string-callable listeners,
    // and string-keyed closures stay out of scope (see the regex comments).
    if (content.includes('Event::listen')) {
      const safe = stripCommentsForRegex(content, 'php');
      const nodesInFile = ctx.getNodesInFile(file);
      const lineAt = makeLineAt(safe, 1);
      let lm: RegExpExecArray | null;
      LARAVEL_LISTEN_KEYED_RE.lastIndex = 0;
      while ((lm = LARAVEL_LISTEN_KEYED_RE.exec(safe))) {
        const enclosing = enclosingFn(nodesInFile, lineAt(lm.index));
        if (enclosing) add(phpSimpleName(lm[1]!), enclosing);
      }
      LARAVEL_LISTEN_CLOSURE_RE.lastIndex = 0;
      while ((lm = LARAVEL_LISTEN_CLOSURE_RE.exec(safe))) {
        const enclosing = enclosingFn(nodesInFile, lineAt(lm.index));
        if (!enclosing) continue;
        for (const t of lm[1]!.replace(/^\?/, '').split('|')) {
          const ev = phpSimpleName(t);
          if (/^[A-Z]\w*$/.test(ev)) add(ev, enclosing);
        }
      }
    }
  }
  if (!listeners.size) return [];

  // Pass 2 — link each event(new X(...)) site → every listener of X.
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!LARAVEL_PHP_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('event(')) continue;
    const safe = stripCommentsForRegex(content, 'php');
    const nodesInFile = ctx.getNodesInFile(file);
    LARAVEL_DISPATCH_RE.lastIndex = 0;
    let m: RegExpExecArray | null;
    let added = 0;
    while ((m = LARAVEL_DISPATCH_RE.exec(safe)) && added < LARAVEL_FANOUT_CAP) {
      const targets = listeners.get(phpSimpleName(m[1]!));
      if (!targets) continue;
      const line = lineOf(safe, m.index);
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) continue;
      for (const target of targets.values()) {
        if (target.id === disp.id) continue;
        const key = `${disp.id}>${target.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        edges.push({
          source: disp.id,
          target: target.id,
          kind: 'calls',
          line,
          provenance: 'heuristic',
          metadata: { synthesizedBy: 'laravel-event', via: phpSimpleName(m[1]!), registeredAt: `${file}:${line}` },
        });
        added++;
      }
    }
  }
  return edges;
}

// ── Laravel queued jobs (PHP) ─────────────────────────────────────────────────
// A Dispatchable job is dispatched through the trait's static entry, a bus
// facade, or the global helper — each returns a PendingDispatch whose real
// target is the job's `handle`:
//   SendWebhookMessage::dispatch($message)->afterResponse();   // firefly-iii
//   Dispatcher::dispatch(new DeleteSongFilesJob($files));      // koel facade
//   dispatch(new GenerateAlbumThumbnailJob($album));           // helper
// The gate: the dispatched class must be Dispatchable — its own `use
// Dispatchable` trait line, or a superclass's (koel's `QueuedJob` base),
// chain-walked with a cycle guard. `event(new X)` and `$listen` are the event
// synth's shapes; a non-Dispatchable class dispatched through any form here
// declines. `->dispatch(`, `::dispatchSync/dispatchAfterResponse/dispatchIf/
// dispatchUnless` are the same PendingDispatch mechanism; `dispatch(new X)`
// helper requires the `new`-ed arg since a bare helper has no receiver class.
const LARAVEL_JOB_STATIC_RE = /\b\\?([A-Za-z_][\w\\]*)::(?:dispatch|dispatchSync|dispatchAfterResponse|dispatchIf|dispatchUnless)\s*\(/g;
const LARAVEL_JOB_FACADE_RE = /\b(?:Dispatcher|Bus|Queue|Illuminate\\Support\\Facades\\Bus|Illuminate\\Bus\\Dispatcher)::dispatch\s*\(\s*new\s+\\?([A-Za-z_][\w\\]*)/g;
const LARAVEL_JOB_HELPER_RE = /\bdispatch\s*\(\s*new\s+\\?([A-Za-z_][\w\\]*)/g;
// Class-body trait window: trait `use` lines always precede the first method.
// The list ends at `;` or a `{` adaptation block (`use A { a as b; }`).
const LARAVEL_TRAIT_USE_RE = /^\s*use\s+([A-Za-z_\\][\w\\,\s]*)/;
const LARAVEL_EXTENDS_RE = /\bextends\s+([A-Za-z_][\w\\]*)/;
const LARAVEL_DISPATCHABLE_DEPTH = 6;

/** The `use …` trait statements inside a class body — everything up to the
 *  first `function` keyword (trait uses precede methods in every real class). */
function laravelClassTraitUses(cls: Node, lines: string[]): string[] {
  const out: string[] = [];
  for (let i = cls.startLine; i < (cls.endLine ?? cls.startLine) && i < lines.length; i++) {
    const t = lines[i] ?? '';
    if (/\bfunction\b/.test(t)) break;
    const m = LARAVEL_TRAIT_USE_RE.exec(t);
    if (m) for (const seg of m[1]!.split(',')) out.push(phpSimpleName(seg));
  }
  return out;
}

async function laravelJobDispatchEdges(ctx: ResolutionContext, onYield: MaybeYield): Promise<Edge[]> {
  let scannedFiles = 0;
  // Pass 1 — class → its trait uses + `extends` base (for the Dispatchable walk).
  interface PhpClassInfo { cls: Node; traits: string[]; base: string | null }
  const classInfo = new Map<string, PhpClassInfo>(); // by node id
  const classesByName = new Map<string, PhpClassInfo[]>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!LARAVEL_PHP_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || (!content.includes('use ') && !content.includes('extends '))) continue;
    const lines = stripCommentsForRegex(content, 'php').split('\n');
    for (const cls of ctx.getNodesInFile(file)) {
      if (cls.kind !== 'class') continue;
      const decl = lines[cls.startLine - 1] ?? '';
      const info: PhpClassInfo = {
        cls,
        traits: laravelClassTraitUses(cls, lines),
        base: LARAVEL_EXTENDS_RE.exec(decl)?.[1]?.split('\\').pop() ?? null,
      };
      classInfo.set(cls.id, info);
      let arr = classesByName.get(cls.name);
      if (!arr) { arr = []; classesByName.set(cls.name, arr); }
      arr.push(info);
    }
  }
  if (!classInfo.size) return [];

  // Dispatchable when the class or an ancestor carries the trait — koel's jobs
  // all `extends QueuedJob` (the base holds `use Dispatchable`). Unique-name
  // bases only; an ambiguous superclass name declines the chain.
  const dispatchable = new Map<string, boolean>();
  const isDispatchable = (info: PhpClassInfo, depth: number, seen: Set<string>): boolean => {
    let v = dispatchable.get(info.cls.id);
    if (v !== undefined) return v;
    if (depth <= 0 || seen.has(info.cls.id)) return false;
    seen.add(info.cls.id);
    v = info.traits.some((t) => t === 'Dispatchable');
    if (!v && info.base) {
      const bases = (classesByName.get(info.base) ?? []).filter((b) => b.cls.id !== info.cls.id);
      const base = bases.length === 1 ? bases[0] : bases.find((b) => b.cls.filePath === info.cls.filePath);
      if (base) v = isDispatchable(base, depth - 1, seen);
    }
    dispatchable.set(info.cls.id, v);
    return v;
  };

  // The job's `handle` — own method first, else up the superclass chain.
  const handleCache = new Map<string, Node | null>();
  const jobHandle = (info: PhpClassInfo, depth: number, seen: Set<string>): Node | null => {
    let v = handleCache.get(info.cls.id);
    if (v !== undefined) return v;
    v = null;
    if (depth > 0 && !seen.has(info.cls.id)) {
      seen.add(info.cls.id);
      v =
        ctx
          .getNodesInFile(info.cls.filePath)
          .find(
            (n) =>
              n.kind === 'method' && n.name === 'handle' &&
              n.startLine >= info.cls.startLine && n.startLine <= (info.cls.endLine ?? info.cls.startLine)
          ) ?? null;
      if (!v && info.base) {
        const bases = (classesByName.get(info.base) ?? []).filter((b) => b.cls.id !== info.cls.id);
        const base = bases.length === 1 ? bases[0] : bases.find((b) => b.cls.filePath === info.cls.filePath);
        if (base) v = jobHandle(base, depth - 1, seen);
      }
    }
    handleCache.set(info.cls.id, v);
    return v;
  };

  // A class simple-name → its dispatchable job (exactly one, else ambiguous).
  const dispatchableJob = (name: string, dispatchFile: string): PhpClassInfo | null => {
    const cands = (classesByName.get(name) ?? []).filter((c) => isDispatchable(c, LARAVEL_DISPATCHABLE_DEPTH, new Set()));
    if (!cands.length) return null;
    return cands.length === 1 ? cands[0]! : cands.find((c) => c.cls.filePath === dispatchFile) ?? null;
  };

  // Pass 2 — dispatch sites.
  const edges: Edge[] = [];
  const seen = new Set<string>();
  for (const file of ctx.getAllFiles()) {
    if ((++scannedFiles & 15) === 0) await onYield();
    if (!LARAVEL_PHP_EXT.test(file)) continue;
    const content = ctx.readFile(file);
    if (!content || !content.includes('dispatch')) continue;
    const safe = stripCommentsForRegex(content, 'php');
    const nodesInFile = ctx.getNodesInFile(file);
    const lineAt = makeLineAt(safe, 1);
    let m: RegExpExecArray | null;
    let added = 0;
    const emit = (jobName: string, line: number) => {
      if (added >= LARAVEL_FANOUT_CAP) return;
      const disp = enclosingFn(nodesInFile, line);
      if (!disp) return;
      const job = dispatchableJob(phpSimpleName(jobName), file);
      if (!job) return;
      const target = jobHandle(job, LARAVEL_DISPATCHABLE_DEPTH, new Set());
      if (!target || target.id === disp.id) return;
      const key = `${disp.id}>${target.id}`;
      if (seen.has(key)) return;
      seen.add(key);
      edges.push({
        source: disp.id,
        target: target.id,
        kind: 'calls',
        line,
        provenance: 'heuristic',
        metadata: { synthesizedBy: 'laravel-job', via: phpSimpleName(jobName), registeredAt: `${file}:${line}` },
      });
      added++;
    };
    // `XJob::dispatch(…)` — the static trait entry. A facade receiver
    // (`Dispatcher::dispatch(new XJob(…))`) is handled by the facade arm —
    // `Dispatcher` is never Dispatchable so it declines here naturally.
    LARAVEL_JOB_STATIC_RE.lastIndex = 0;
    while ((m = LARAVEL_JOB_STATIC_RE.exec(safe)) && added < LARAVEL_FANOUT_CAP) {
      emit(m[1]!, lineAt(m.index));
    }
    LARAVEL_JOB_FACADE_RE.lastIndex = 0;
    while ((m = LARAVEL_JOB_FACADE_RE.exec(safe)) && added < LARAVEL_FANOUT_CAP) {
      emit(m[1]!, lineAt(m.index));
    }
    // `dispatch(new XJob(…))` helper — a bare call only: a `:`/`>`/`$` before
    // `dispatch` means `X::dispatch`/`->dispatch`/`$x->dispatch` (handled
    // above or not a job dispatch at all).
    LARAVEL_JOB_HELPER_RE.lastIndex = 0;
    while ((m = LARAVEL_JOB_HELPER_RE.exec(safe)) && added < LARAVEL_FANOUT_CAP) {
      const before = safe[m.index - 1];
      if (before === ':' || before === '>' || before === '$' || /[\w]/.test(before ?? '')) continue;
      emit(m[1]!, lineAt(m.index));
    }
  }
  return edges;
}

/**
 * Synthesize dispatcher→callback edges (field observers + EventEmitters +
 * React re-render + JSX children + Vue templates + SvelteKit load + RN event
 * channel + Fabric native-impl + MyBatis Java↔XML + Gin middleware chain +
 * Redux-thunk dispatch chain + object-literal registry dispatch + RTK Query
 * generated-hook → endpoint + Pinia useStore().action() + Vuex string dispatch +
 * Celery task .delay()/.apply_async() → task body + Spring publishEvent → @EventListener +
 * MediatR Send/Publish → IRequestHandler/INotificationHandler +
 * NgRx store.dispatch → ofType effect +
 * Sidekiq Worker.perform_async → #perform + Laravel event(new X) → listener handle +
 * Drupal invokeAll/invoke/alter → hook impl).
 * Returns the count added. Never throws into indexing — callers wrap in try/catch.
 */

/**
 * Number of progress steps synthesizeCallbackEdges reports: one per `__mark()`
 * call (every synthesis pass, plus the dedupe-merge and edge-insert steps).
 * Cosmetic only — drift just makes the progress bar end early or jump — and a
 * test pins it to the actual step count (registry passes + the fixed
 * pre/post marks) so adding a pass without bumping this fails loudly instead
 * of silently skewing the bar.
 */
/** `has(...)` shape passed to pass gates — true when the project contains any of the languages. */
type HasLang = (...ls: string[]) => boolean;

/**
 * One independent synthesis pass. Every pass scans the COMMITTED graph (plus
 * source via ctx) and returns an edge list; nothing it produces is persisted
 * until the ordered merge in synthesizeCallbackEdges — which is what makes
 * execution order free and the passes safe to fan out across the resolver
 * pool's read-only workers. `gate` short-circuits a pass whose language never
 * appears in the project (its result is provably empty — see #1212).
 */
export interface SynthPassDef {
  name: string;
  gate: (has: HasLang) => boolean;
  run: (
    queries: QueryBuilder,
    ctx: ResolutionContext,
    yieldToLoop: MaybeYield,
    subProgress?: (fraction: number) => void
  ) => Promise<Edge[]>;
}

const ALWAYS = (): boolean => true;

/**
 * The independent passes, in MERGE ORDER — the first-seen dedup in
 * synthesizeCallbackEdges follows this array, so reordering entries changes
 * which duplicate edge wins. The two Go pre-passes (cross-file method
 * `contains`, implicit `implements`) are NOT here: they persist before these
 * run because interfaceOverrideEdges reads their edges from the DB.
 */
export const SYNTH_PASSES: SynthPassDef[] = [
  { name: 'fieldEdges', gate: ALWAYS, run: (q, c, y) => fieldChannelEdges(q, c, y) },
  { name: 'closureCollEdges', gate: ALWAYS, run: (q, c, y) => closureCollectionEdges(q, c, y) },
  // Cross-tier channels — a client's `fetch('/api/x')` onto its own route,
  // a queue job onto its consumer, a bus / socket event onto its handler.
  // Before the in-process emitter pass: the same (source, target) pair
  // keeps the more specific edge — the one that says which tier it crosses.
  { name: 'tierEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => crossTierEdges(c, y) },
  { name: 'emitterEdges', gate: ALWAYS, run: (_q, c, y) => eventEmitterEdges(c, y) },
  { name: 'windowMessageEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => windowMessageEdges(c, y) },
  { name: 'renderEdges', gate: ALWAYS, run: (q, c, y) => reactRenderEdges(q, c, y) },
  { name: 'jsxEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => reactJsxChildEdges(c, y) },
  { name: 'vueEdges', gate: (has) => has('vue'), run: (_q, c, y) => vueTemplateEdges(c, y) },
  { name: 'svelteKitEdges', gate: (has) => has('svelte'), run: (_q, c, y) => svelteKitLoadEdges(c, y) },
  { name: 'pascalEdges', gate: ALWAYS, run: (_q, c, y) => pascalFormEdges(c, y) },
  { name: 'flutterEdges', gate: (has) => has('dart'), run: (q, c, y) => flutterBuildEdges(q, c, y) },
  { name: 'arkuiStateEdges', gate: (has) => has('arkts'), run: (q, c, y) => arkuiStateBuildEdges(q, c, y) },
  { name: 'arkuiEmitter', gate: (has) => has('arkts'), run: (_q, c, y) => arkuiEmitterEdges(c, y) },
  { name: 'arkuiRoutes', gate: (has) => has('arkts'), run: (_q, c, y) => arkuiRouterEdges(c, y) },
  { name: 'cppEdges', gate: (has) => has('cpp'), run: (q, _c, y) => cppOverrideEdges(q, y) },
  {
    name: 'ifaceEdges',
    gate: (has) => has('java', 'kotlin', 'csharp', 'swift', 'scala', 'go', 'rust', 'arkts', ...JS_FAMILY),
    run: (q, _c, y) => interfaceOverrideEdges(q, y),
  },
  { name: 'kotlinExpectActual', gate: (has) => has('kotlin'), run: (q, _c, y) => kotlinExpectActualEdges(q, y) },
  { name: 'goGrpcEdges', gate: (has) => has('go'), run: (q, _c, y) => goGrpcStubImplEdges(q, y) },
  { name: 'rnEventEdgesList', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => rnEventEdges(c, y) },
  // `NativeModules[key].method()` — needs both a JS callsite and a native
  // module impl to link.
  {
    name: 'rnDynModuleEdges',
    gate: (has) => has(...JS_FAMILY) && has('objc', 'swift', 'java', 'kotlin'),
    run: (_q, c, y) => rnDynamicModuleEdges(c, y),
  },
  { name: 'fabricNativeEdges', gate: ALWAYS, run: (_q, c, y) => fabricNativeImplEdges(c, y) },
  // Expo module nodes (`expo-module:` ids) are emitted only from .swift/.kt
  // files, and a pair needs BOTH platforms — so without both languages the
  // pass's only collection loop is provably empty (it was streaming every
  // method row on pure-Java repos to find nothing).
  { name: 'expoXPlatEdges', gate: (has) => has('swift') && has('kotlin'), run: (q, _c, y) => expoCrossPlatformEdges(q, y) },
  // An RN cross-platform edge requires a JS-language caller on the native
  // method (`isBridge`) — no JS-family files means no JS-language nodes, so
  // the result is provably empty.
  { name: 'rnXPlatEdges', gate: (has) => has(...JS_FAMILY), run: (q, _c, y) => rnCrossPlatformEdges(q, y) },
  {
    name: 'mybatisEdges',
    gate: (has) => has('java', 'kotlin') && has('xml'),
    run: (q, _c, y) => mybatisJavaXmlEdges(q, y),
  },
  { name: 'ginEdges', gate: (has) => has('go'), run: (q, c, y) => ginMiddlewareChainEdges(q, c, y) },
  { name: 'thunkEdges', gate: (has) => has(...JS_FAMILY), run: (q, c, y) => reduxThunkEdges(q, c, y) },
  { name: 'registryEdges', gate: ALWAYS, run: (_q, c, y) => objectRegistryEdges(c, y) },
  { name: 'rtkEdges', gate: (has) => has(...JS_FAMILY), run: (q, c, y) => rtkQueryEdges(q, c, y) },
  { name: 'piniaEdges', gate: (has) => has('vue', ...JS_FAMILY), run: (_q, c, y) => piniaStoreEdges(c, y) },
  { name: 'vuexEdges', gate: (has) => has('vue', ...JS_FAMILY), run: (_q, c, y) => vuexDispatchEdges(c, y) },
  { name: 'celeryEdges', gate: (has) => has('python'), run: (_q, c, y) => celeryDispatchEdges(c, y) },
  { name: 'springEdges', gate: (has) => has('java'), run: (_q, c, y) => springEventEdges(c, y) },
  { name: 'mediatrEdges', gate: (has) => has('csharp'), run: (_q, c, y) => mediatrDispatchEdges(c, y) },
  { name: 'ngrxEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => ngrxEffectEdges(c, y) },
  { name: 'sidekiqEdges', gate: (has) => has('ruby'), run: (_q, c, y) => sidekiqDispatchEdges(c, y) },
  {
    name: 'erlangBehaviourEdges',
    gate: (has) => has('erlang'),
    run: (q, c, y) => erlangBehaviourDispatchEdges(q, c, y),
  },
  { name: 'laravelEdges', gate: (has) => has('php'), run: async (_q, c, y) => (await laravelEventEdges(c, y)).concat(await laravelJobDispatchEdges(c, y)) },
  { name: 'drupalHookEdges', gate: (has) => has('php'), run: (q, c, y) => drupalHookEdges(q, c, y) },
  {
    name: 'cFnPtrEdges',
    gate: (has) => has('c', 'cpp'),
    run: (q, c, y, sub) => cFnPointerDispatchEdges(q, c, y, sub),
  },
  { name: 'goframeEdges', gate: (has) => has('go'), run: (_q, c, y) => goframeRouteEdges(c, y) },
  // `router.push(await helper())` — the helper's return literals are the screens.
  { name: 'expoRouterReturnEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => expoRouterReturnEdges(c, y) },
  // `<Link href="/x">` / an internal `<a href>` — markup, not a call; the component navigates.
  { name: 'nextLinkEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => nextLinkEdges(c, y) },
  { name: 'reactRouterLinkEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => reactRouterLinkEdges(c, y) },
  { name: 'tanstackLinkEdges', gate: (has) => has(...JS_FAMILY), run: (_q, c, y) => tanstackLinkEdges(c, y) },
  { name: 'vueRouterLinkEdges', gate: (has) => has('vue', ...JS_FAMILY), run: (_q, c, y) => vueRouterLinkEdges(c, y) },
  { name: 'svelteKitPageEdges', gate: (has) => has('svelte'), run: (_q, c, y) => svelteKitPageComponentEdges(c, y) },
  { name: 'svelteKitLinkEdges', gate: (has) => has('svelte'), run: (_q, c, y) => svelteKitLinkEdges(c, y) },
  { name: 'nixOptionEdges', gate: (has) => has('nix'), run: (q, _c, y) => nixOptionPathEdges(q, y) },
];

/** Fixed non-registry steps: goMethodContains, goImplements, dedupe-merge, insertMergedEdges. */
const FIXED_SYNTH_STEPS = 4;
export const SYNTH_PROGRESS_STEPS = SYNTH_PASSES.length + FIXED_SYNTH_STEPS;
/**
 * The context the synthesis passes share for one run: file contents and
 * per-file nodes are read once and handed to every pass. The resolver's own
 * caches are LRUs sized for resolution batches (1,000 files of content, 5,000
 * of nodes); ~30 passes each walking every file of a 13k-file repository
 * evicted each other's entries, so every pass re-read the repository from
 * disk and SQLite (a quarter of a trezor-suite run each). Content is capped
 * at `RUN_CACHE_BYTES`; past it, reads fall through to the resolver.
 */
const RUN_CACHE_BYTES = 256 * 1024 * 1024;
function withRunCache(ctx: ResolutionContext): ResolutionContext {
  const files = new Map<string, string | null>();
  const nodes = new Map<string, Node[]>();
  let bytes = 0;
  return {
    ...ctx,
    readFile(filePath: string): string | null {
      const hit = files.get(filePath);
      if (hit !== undefined) return hit;
      const content = ctx.readFile(filePath);
      if (bytes + (content?.length ?? 0) <= RUN_CACHE_BYTES) {
        files.set(filePath, content);
        bytes += content?.length ?? 0;
      }
      return content;
    },
    getNodesInFile(filePath: string): Node[] {
      let list = nodes.get(filePath);
      if (!list) {
        list = ctx.getNodesInFile(filePath);
        if (bytes <= RUN_CACHE_BYTES) nodes.set(filePath, list);
      }
      return list;
    },
  };
}

export async function synthesizeCallbackEdges(
  queries: QueryBuilder,
  ctx: ResolutionContext,
  onProgress?: (done: number, total: number) => void,
  // A live resolver pool to fan the independent passes across (structural type
  // so this file never imports the pool — resolver-worker imports THIS file).
  // Null/omitted → the sequential path, byte-identical to the pool path.
  pool?: { runSynthPass(name: string): Promise<{ edges: Edge[]; ms: number }> } | null,
  // WAL-valve writer backstop (WalCheckpointValve.backpressure), called at
  // pool-idle points in the edge-insert loops below — the passes themselves
  // only read; every write in this function happens with the pool idle.
  backpressure?: () => Promise<void> | null
): Promise<number> {
  return withStripMemo(() => synthesizeWith(queries, withRunCache(ctx), onProgress, pool, backpressure));
}

async function synthesizeWith(
  queries: QueryBuilder,
  ctx: ResolutionContext,
  onProgress?: (done: number, total: number) => void,
  // A live resolver pool to fan the independent passes across (structural type
  // so this file never imports the pool — resolver-worker imports THIS file).
  // Null/omitted → the sequential path, byte-identical to the pool path.
  pool?: { runSynthPass(name: string): Promise<{ edges: Edge[]; ms: number }> } | null,
  // WAL-valve writer backstop (WalCheckpointValve.backpressure), called at
  // pool-idle points in the edge-insert loops below — the passes themselves
  // only read; every write in this function happens with the pool idle.
  backpressure?: () => Promise<void> | null
): Promise<number> {
  // Each sub-pass below is a whole-graph scan, and there are ~30 of them, all
  // running synchronously on the indexer's main thread. Their AGGREGATE can run
  // for well over a minute on a large repo — long enough for the #850 liveness
  // watchdog to SIGKILL the process mid-index (#1091), since its heartbeat lives
  // on this same thread. Yield between passes so the heartbeat can fire; a pass
  // that itself hangs (a real wedge) never reaches the next yield, so the
  // watchdog still catches that. See ./cooperative-yield.
  const yieldToLoop = createYielder();

  // Synthesis runs AFTER the resolution progress bar reaches 100%, so without
  // its own progress the UI freezes at "Resolving refs 100%" for the whole
  // tail — long enough on big repos that users conclude the index hung and
  // kill it. Report each completed pass; the caller surfaces it as its own
  // progress phase. Emit 0/total up front so the phase flips immediately.
  // Emissions are throttled to whole-percent movement (each consumes a UI
  // message); values may be fractional steps from within-pass reporting.
  let passesDone = 0;
  let lastPct = -1;
  const emit = (value: number): void => {
    if (!onProgress) return;
    const v = Math.min(value, SYNTH_PROGRESS_STEPS);
    const pct = Math.floor((v / SYNTH_PROGRESS_STEPS) * 100);
    if (pct === lastPct) return;
    lastPct = pct;
    onProgress(v, SYNTH_PROGRESS_STEPS);
  };
  // A single long pass otherwise parks the bar between steps; a pass that
  // takes this callback reports a 0..1 fraction of its own work, surfaced
  // here as fractional progress within its step.
  const subProgress = (fraction: number): void =>
    emit(passesDone + Math.max(0, Math.min(fraction, 1)));
  emit(0);

  // Per-pass wall-clock timing to stderr, opt-in via CODEGRAPH_SYNTH_TIMINGS
  // (=1: passes over 250ms; =all: every pass). This is the diagnostic that
  // located both the #1091/#1122 watchdog stalls and the #1212 OOM — keep it.
  const markT = { t: Date.now() };
  const __mark = (label: string): void => {
    const now = Date.now();
    const dt = now - markT.t;
    markT.t = now;
    if (process.env.CODEGRAPH_SYNTH_TIMINGS && (dt > 250 || process.env.CODEGRAPH_SYNTH_TIMINGS === 'all')) {
      console.error(`[synth-timing] ${label}: ${dt}ms`);
    }
    passesDone++;
    emit(passesDone);
  };

  // Language gating: one indexed DISTINCT over the files table lets a pass
  // whose own filters reference a specific language/extension be skipped
  // outright when the project has no such files — its result is provably
  // empty, so skipping is behavior-identical and the cost drops to zero
  // (the Kotlin pass was the OOM culprit on the pure-C Linux kernel, #1212).
  // Passes without an explicit language filter always run.
  const langs = queries.getDistinctFileLanguages();
  const has = (...ls: string[]): boolean => ls.some((l) => langs.has(l));
  const NONE: Edge[] = [];

  // Cross-file Go method→type `contains` edges must be synthesized AND persisted
  // FIRST: a method declared in a different file from its receiver type is
  // otherwise orphaned from the struct, and goImplementsEdges (next) derives a
  // struct's method set from its `contains` edges — so without this it would
  // under-count the interfaces a cross-file struct satisfies. (#583)
  // Writer-side WAL backstop for the insert loops here (see the param doc):
  // one fstat when under the valve's hard cap, a parked full backfill past it.
  const foldIfOver = async (): Promise<void> => {
    const bp = backpressure?.();
    if (bp) await bp;
  };

  const goMethodContains = has('go') ? await goCrossFileMethodContainsEdges(queries, yieldToLoop) : NONE;
  for (let i = 0; i < goMethodContains.length; i += 2000) {
    queries.insertEdges(goMethodContains.slice(i, i + 2000));
    await yieldToLoop();
    await foldIfOver();
  }
  await yieldToLoop(); __mark('goMethodContains');

  // Go implicit `implements` edges must be synthesized AND persisted next: the
  // interface-dispatch bridge below reads `implements` edges from the DB, and
  // Go has none statically. (Other languages already have static implements
  // edges from extraction, so they don't need this pre-pass.)
  const goImpl = has('go') ? await goImplementsEdges(queries, yieldToLoop) : NONE;
  for (let i = 0; i < goImpl.length; i += 2000) {
    queries.insertEdges(goImpl.slice(i, i + 2000));
    await yieldToLoop();
    await foldIfOver();
  }
  await yieldToLoop(); __mark('goImplements');

  // Run the independent passes (see SYNTH_PASSES). Their results are merged in
  // REGISTRY ORDER below regardless of execution order, and none of their edges
  // persist until that merge — so every pass sees the same committed
  // post-resolution DB state whether it runs sequentially here or on a resolver
  // pool worker. With a live pool (already booted on ≥150k-ref repos), passes
  // fan out across its read-only workers and the per-pass wall-clock comes from
  // the worker; a pass that fails on a worker falls back to running on the main
  // thread, so a worker crash isolates to a retry instead of failing synthesis.
  const passEdges: Edge[][] = new Array<Edge[]>(SYNTH_PASSES.length).fill(NONE);
  const markPass = (label: string, dt: number): void => {
    if (process.env.CODEGRAPH_SYNTH_TIMINGS && (dt > 250 || process.env.CODEGRAPH_SYNTH_TIMINGS === 'all')) {
      console.error(`[synth-timing] ${label}: ${dt}ms`);
    }
    passesDone++;
    emit(passesDone);
  };
  const runPassOnMain = async (i: number): Promise<void> => {
    const pass = SYNTH_PASSES[i]!;
    const t0 = Date.now();
    passEdges[i] = await pass.run(queries, ctx, yieldToLoop, subProgress);
    await yieldToLoop();
    markPass(pass.name, Date.now() - t0);
  };

  const gatedIn: number[] = [];
  for (let i = 0; i < SYNTH_PASSES.length; i++) {
    if (SYNTH_PASSES[i]!.gate(has)) gatedIn.push(i);
    else markPass(SYNTH_PASSES[i]!.name, 0);
  }

  // Above this node count, a pass that OOM-killed its worker must NOT be
  // retried on the main thread — the retry would OOM the whole process and
  // take the index with it (the #1212 failure class). Below it, a worker
  // failure is more likely a transient crash than a memory ceiling, and the
  // main-thread retry keeps coverage. Skipping loses only that pass's
  // synthesized edges; the index still completes.
  const MAIN_RETRY_MAX_NODES = 1_500_000;
  const graphNodes = queries.getNodeAndEdgeCount().nodes;

  if (pool && gatedIn.length > 1) {
    await Promise.all(
      gatedIn.map(async (i) => {
        const pass = SYNTH_PASSES[i]!;
        try {
          const out = await pool.runSynthPass(pass.name);
          passEdges[i] = out.edges;
          markPass(pass.name, out.ms);
        } catch (err) {
          if (graphNodes > MAIN_RETRY_MAX_NODES) {
            // Worker died at a scale where the main-thread retry is a process
            // OOM risk: skip the pass, keep the index alive, and say so.
            console.error(
              `[synthesis] pass '${pass.name}' failed on a worker at ${graphNodes} nodes — skipped (edges from this pass are absent): ${err instanceof Error ? err.message : String(err)}`
            );
            markPass(`${pass.name} (skipped at scale)`, 0);
            return;
          }
          // Worker-side failure (crash, OOM, unknown pass after a version
          // mismatch): retry this one pass on the main thread.
          await runPassOnMain(i);
        }
      })
    );
  } else {
    for (const i of gatedIn) {
      await runPassOnMain(i);
    }
  }

  const merged: Edge[] = [];
  const seen = new Set<string>();
  for (const e of passEdges.flat()) {
    const key = `${e.source}>${e.target}`;
    if (seen.has(key)) continue;
    seen.add(key);
    merged.push(e);
  }
  __mark('dedupe-merge');
  // External endpoint nodes the http pass points at: created here, on the
  // writer, because passes may run on read-only workers. insertEdges drops an
  // edge whose target does not exist, so they go in first.
  // Only missing ones: replacing an existing endpoint row would cascade away
  // the edges earlier runs attached to it.
  const endpoints = endpointNodesFor(merged, (file) => queries.getFileByPath(file)?.language ?? 'unknown');
  if (endpoints.length > 0) {
    const existing = queries.getExistingNodeIds(endpoints.map((n) => n.id));
    const missing = endpoints.filter((n) => !existing.has(n.id));
    if (missing.length > 0) queries.insertNodes(missing);
  }
  // Chunked insert with yields: on the Linux kernel the merged synthesized
  // edge set is ~275k rows, and one transaction for all of them was a 20s
  // unyielded main-thread span (#1212 follow-up) — the last one in the tail.
  for (let i = 0; i < merged.length; i += 2000) {
    queries.insertEdges(merged.slice(i, i + 2000));
    await yieldToLoop();
    await foldIfOver();
  }
  __mark('insertMergedEdges');
  return merged.length + goImpl.length + goMethodContains.length;
}
