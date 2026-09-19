/**
 * Dynamic-dispatch boundary detection for codegraph_explore (#687).
 *
 * When the flow an agent asked about does NOT connect statically, the cause is
 * almost always a dynamic-dispatch site: a computed member call, getattr,
 * reflection, a string-keyed bus, a typed command/mediator dispatch, an async
 * handoff (timer/goroutine/executor), a service-locator lookup, an IPC
 * channel, a delegate invoke, or a reactive chain. Guessing
 * the missing edge was rejected (silent beats wrong — a wrong edge poisons the
 * map and teaches abandonment). Instead, explore ANNOUNCES the boundary
 * honestly: the exact site where the static path ends, the dispatch form, and
 * — when a key is statically visible (string literal, `:symbol`, `new Type`)
 * — that key, so the caller can shortlist candidate targets.
 *
 * Detection is deterministic regex over the comment/string-stripped bodies of
 * the symbols the agent named, at QUERY TIME only. The graph is never mutated;
 * an unbroken flow never triggers a scan. Matching runs on the stripped text
 * (so commented-out / string-embedded code can't fire) but snippets and keys
 * are sliced from the ORIGINAL source at the same offsets — both strippers
 * blank contents in place, preserving offsets, precisely for this.
 * (`stripCommentsForRegex` blanks comments but deliberately KEEPS string
 * contents — framework extractors need route literals; here a dispatch shape
 * inside a string is a false positive, so {@link blankStringContents} blanks
 * them too, quotes preserved.)
 */
import { blankStringContents, stripCommentsForRegex, type CommentLang } from '../resolution/strip-comments';
export { blankStringContents } from '../resolution/strip-comments';

export interface BoundaryMatch {
  /** Stable form id, e.g. 'computed-call' — used for per-form dedupe. */
  form: string;
  /** Human label for the dispatch form, e.g. 'computed member call'. */
  label: string;
  /** One-line source snippet of the site (from the original, untrimmed text). */
  snippet: string;
  /** 1-based line within the scanned body's FILE (absolute, ready to print). */
  line: number;
  /**
   * Statically-visible dispatch key, when one exists: the string literal in
   * `handlers['save']`, the `:symbol` in ruby `send`, the type name in
   * `Send(new CreateCmd(...))`. Drives candidate lookup. Undefined when the
   * key is a runtime value (variable, computed expression).
   */
  key?: string;
  /** For typed-bus matches the key is a TYPE name (candidates ~ `${key}Handler`). */
  keyIsType?: boolean;
  /** Additional sites of the same form+key in this body beyond the reported one. */
  moreSites?: number;
}

interface FormSpec {
  form: string;
  label: string;
  /** Languages this form applies to; undefined = all. Node.language values. */
  langs?: Set<string>;
  re: RegExp;
  /**
   * Derive the dispatch key from the ORIGINAL-source snippet around the match
   * (match start .. match end + keyWindow). Return undefined when no static key.
   */
  keyFrom?: (orig: string) => { key: string; keyIsType?: boolean } | undefined;
  /**
   * Extra ORIGINAL chars after the match end handed to keyFrom, capped at the
   * first newline — for forms whose key trails the matched prefix, e.g.
   * `.getMethod(` → `"handlePing"`. Forms with $-anchored keyFrom regexes
   * must leave this unset (the anchor relies on the slice ending at the match).
   */
  keyWindow?: number;
}

const JS_FAMILY = new Set(['typescript', 'javascript', 'tsx', 'jsx', 'vue', 'svelte', 'astro', 'arkts']);
const PY = new Set(['python']);
const RB = new Set(['ruby']);
const PHP = new Set(['php']);
const JVM_CS_GO = new Set(['java', 'kotlin', 'scala', 'csharp', 'go']);
const SWIFT_OBJC = new Set(['swift', 'objc', 'objcpp', 'objective-c']);
/** JVM languages only — the forms that split JVM from C#/Go need this subset. */
const JVM = new Set(['java', 'kotlin', 'scala']);
const CS = new Set(['csharp']);
const GO = new Set(['go']);
const RUST = new Set(['rust']);

/** Exactly one quoted literal and no concatenation → that literal is the key. */
function singleStringLiteral(text: string): string | undefined {
  const m = text.match(/^[^'"`]*(['"`])([\w.:-]{2,64})\1[^'"`]*$/);
  return m ? m[2] : undefined;
}

/** Last dotted segment of a name chain (`Foo.Bar` → `Bar`). */
function lastSegment(chain: string): string {
  return chain.split('.').pop() ?? chain;
}

/**
 * Chain segments that are never the dispatch target: call-tail combinators
 * (`foo.bind(this)` → `foo`), lambda keywords (`thread::spawn(move || …)`),
 * and `self`/`this`/`new` so a lone receiver never becomes a key.
 */
const NON_TARGET_SEGS = new Set([
  'bind', 'call', 'apply', 'then', 'catch', 'finally',
  'move', 'async', 'function', 'func', 'fn', 'self', 'this', 'new',
]);

/** Last segment of a chain with call-tail noise dropped (`foo.bind` → `foo`). */
function targetSegment(chain: string): string | undefined {
  const segs = chain.split('.').filter((s) => s && !NON_TARGET_SEGS.has(s));
  return segs[segs.length - 1];
}

/**
 * The callee identifier of a call's first argument → dispatch key.
 * `setTimeout(handleTick, 0)` → `handleTick`, `create_task(self.process())`
 * → `process`, `Thread(target=worker)` → `worker`, `Foo::bar` method refs
 * → `bar`. Arrow params, lambdas and non-identifier args yield nothing — a
 * wrong key is worse than none.
 */
function identArgKey(orig: string): { key: string } | undefined {
  // Method-reference form: `supplyAsync(this::load)` → `load`; `Foo::new` → `Foo`.
  const mref = orig.match(/\(\s*([\w$]+)\s*::\s*([\w$]+)/);
  if (mref) return { key: NON_TARGET_SEGS.has(mref[2]!) ? mref[1]! : mref[2]! };
  // `(` ws (kwarg= ws)? ident(.ident)* — the ident must be the WHOLE first
  // argument: followed by `,` `)` or `(` (a call like `coro()`), so kwargs
  // (`name='x'`) and arrow params (`x =>`) cannot leak their names as keys.
  const m = orig.match(/\(\s*(?:[A-Za-z_][\w$]*\s*[=:]\s*)?([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)(?=\s*[),(])/);
  if (m) {
    const key = targetSegment(m[1]!);
    if (key) return { key };
  }
  // Kwarg fallback for `Thread(name='x', target=run)` — the dispatch kwarg
  // is not always first; the value must still be an identifier.
  const kw = orig.match(/\b(?:target|name|execute)\s*[=:]\s*([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)/);
  if (!kw) return undefined;
  const key = targetSegment(kw[1]!);
  return key ? { key } : undefined;
}

const FORMS: FormSpec[] = [
  {
    // handlers[action.type](payload) / registry[key](args) / table[k](...) —
    // the `](` adjacency is the gate; a word/`)`/`]` char must precede `[` so
    // array literals and markdown-ish text in prose can't fire.
    form: 'computed-call',
    label: 'computed member call',
    re: /[\w$)\]]\s*\[([^[\]\n]{1,80})\]\s*\(/g,
    keyFrom: (orig) => {
      const inner = orig.match(/\[([^[\]\n]{1,80})\]\s*\($/);
      const key = inner ? singleStringLiteral(inner[1]!) : undefined;
      return key ? { key } : undefined;
    },
  },
  {
    // import(expr) / require(expr) with a NON-literal argument → runtime module
    // choice. Literal imports are ordinary edges and never reach this scanner.
    form: 'dynamic-import',
    label: 'dynamic import',
    langs: JS_FAMILY,
    re: /\b(?:import|require)\s*\(\s*(?![\s'"`)])/g,
  },
  {
    form: 'dynamic-import',
    label: 'dynamic import',
    langs: PY,
    re: /\bimportlib\.import_module\s*\(|\b__import__\s*\(/g,
  },
  {
    // obj.send(:method_name) / public_send / method(:name) — plus the
    // metaprogramming forms that define or catch dispatch: define_method,
    // method_missing, const_get, instance/class/module_eval, respond_to_missing.
    form: 'ruby-send',
    label: 'send dispatch',
    langs: RB,
    re: /\.(?:public_)?send\s*\(\s*:?\w+|\bmethod\s*\(\s*:\w+\s*\)|\bdefine_method\s*\(|\bmethod_missing\b|\bconst_get\s*\(|\b(?:instance|class|module)_eval\s*\(|\brespond_to_missing\b/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      // const_get("Foo") / const_get(:Foo) — the constant name is a type key.
      const cg = orig.match(/const_get\s*\(\s*['":]?([A-Z]\w*)/);
      if (cg) return { key: cg[1]!, keyIsType: true };
      const m = orig.match(/:(\w+)/);
      return m ? { key: m[1]! } : undefined;
    },
  },
  {
    // call_user_func([$this, 'method']) / $this->$method() / $callback() /
    // $this->{$expr}() / __call / forward_static_call — PHP variable
    // functions and callables. ($obj->$var and static::$var are already
    // covered by the bare `$ident(` arm.)
    form: 'php-dynamic',
    label: 'dynamic call',
    langs: PHP,
    re: /\bcall_user_func(?:_array)?\s*\(|\$this\s*->\s*\$\w+\s*\(|\$\w+\s*\(|->\s*\{[^}\n]{1,60}\}\s*\(|->\s*__call\s*\(|\bforward_static_call(?:_array)?\s*\(|\bstatic\s*::\s*\$\w+\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      const key = singleStringLiteral(orig);
      return key ? { key } : undefined;
    },
  },
  {
    // Reflection: Method.invoke / getMethod("x") / Class.forName / Go
    // reflect MethodByName / C# Activator.CreateInstance, GetMethod —
    // plus ServiceLoader.load / newInstance / get(Declared)Constructor.
    form: 'reflection',
    label: 'reflective dispatch',
    langs: JVM_CS_GO,
    re: /\.invoke\s*\(|\.get(?:Declared)?Method\s*\(|\.GetMethod\s*\(|MethodByName\s*\(|Activator\.CreateInstance|Class\.forName\s*\(|\bServiceLoader\.load\s*\(|\.newInstance\s*\(|\.get(?:Declared)?Constructor\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      // `Foo.class` args (ServiceLoader.load(Foo.class), getConstructor(String.class))
      // resolve to a TYPE key; a string literal stays a plain key.
      const t = orig.match(/\b([A-Z]\w*)\s*\.\s*class\b/);
      if (t) return { key: t[1]!, keyIsType: true };
      const key = singleStringLiteral(orig);
      return key ? { key } : undefined;
    },
  },
  {
    // new Proxy(target, handler) / Reflect.get|apply — JS metaobject dispatch.
    form: 'proxy-reflect',
    label: 'Proxy/Reflect dispatch',
    langs: JS_FAMILY,
    re: /\bnew\s+Proxy\s*\(|\bReflect\.(?:get|apply|construct)\s*\(/g,
  },
  {
    // mediator.Send(new CreateTodoItemCommand(...)) / bus.publish(new OrderEvent(...))
    // — typed message dispatch (MediatR/CQRS/event-bus). The request TYPE is the
    // key; the conventional target is `<Type>Handler`.
    form: 'typed-bus',
    label: 'typed message dispatch',
    re: /\.(?:[Ss]end|[Pp]ublish|[Dd]ispatch|[Ee]xecute|[Pp]ost|[Ee]mit|[Nn]otify|[Aa]nnounce|[Rr]aise|[Dd]eliver|[Ss]ubmit|[Oo]ffer|[Ee]nqueue)(?:Async)?\s*(?:<[^<>\n]{0,80}>)?\s*\(\s*new\s+([A-Z]\w*)/g,
    keyFrom: (orig) => {
      const m = orig.match(/new\s+([A-Z]\w*)$/);
      return m ? { key: m[1]!, keyIsType: true } : undefined;
    },
  },
  {
    // emitter.emit(eventVar, ...) / store.dispatch(action) / emitter.on(name, fn)
    // — string-keyed dispatch/registration where the key is a RUNTIME value.
    // The literal-keyed shape belongs to literal-key-dispatch; `subscribe`,
    // `observe`, `watch` stay out — reactive-chain owns them.
    form: 'var-key-dispatch',
    label: 'string-keyed dispatch (runtime key)',
    re: /\.(?:emit|dispatch|trigger|fire|publish|broadcast|on|once|addListener|addEventListener|off|notify|signal|announce|raise|deliver)\s*\(\s*[A-Za-z_$][\w$]*(?:\.[\w$]+){0,3}\s*[,)]/g,
  },
  {
    // Swift/ObjC: #selector(name) / NSClassFromString / performSelector(:) /
    // NSSelectorFromString / objc_msgSend / .perform( — runtime selector dispatch.
    form: 'selector',
    label: 'selector dispatch',
    langs: SWIFT_OBJC,
    re: /#selector\s*\(\s*([\w.]+)|NSClassFromString\s*\(|\bperformSelector(?:InBackground|OnMainThread)?\s*[:({]|\bNSSelectorFromString\s*\(|\bobjc_msgSend\s*\(|\.perform\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      const m = orig.match(/#selector\s*\(\s*([\w.]+)/) ?? orig.match(/@selector\s*\(\s*(\w+)/);
      if (m) return { key: lastSegment(m[1]!) };
      // NSSelectorFromString(@"handleTap:") — selector strings carry `:`
      // arg markers; the name before the first colon is the method.
      const lit = orig.match(/NSSelectorFromString\s*\(\s*@?['"]([\w:]+)/);
      if (lit) return { key: lit[1]!.split(':')[0]! };
      return undefined;
    },
  },
  // ---------------------------------------------------------------------------
  // Keyed/specific forms sit ABOVE the noisy announce-only reactive-chain:
  // FORMS is scanned in order and the scan stops at MAX_MATCHES_PER_BODY, so
  // the forms carrying a candidate-driving key earn their slot first.
  // ---------------------------------------------------------------------------
  {
    // setTimeout(cb) / queueMicrotask(fn) / p.then(next) — the callback
    // identifier is the dispatch key and resolves via getNodesByName directly.
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: JS_FAMILY,
    re: /\b(?:setTimeout|setInterval|queueMicrotask|setImmediate|requestAnimationFrame|requestIdleCallback|process\.nextTick)\s*\(|\.then\s*\(\s*[A-Za-z_$]/g,
    keyWindow: 80,
    keyFrom: (orig) => identArgKey(orig),
  },
  {
    // go worker.Run() / go s.handle() — the goroutine callee is the key.
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: GO,
    re: /\bgo\s+[A-Za-z_][\w.]*\s*\(/g,
    keyFrom: (orig) => {
      const m = orig.match(/\bgo\s+([A-Za-z_][\w.]*)/);
      const key = m ? targetSegment(m[1]!) : undefined;
      return key ? { key } : undefined;
    },
  },
  {
    // executor.submit(task) / CompletableFuture.supplyAsync(..) / new Thread(r).
    // The `new X(` lookahead keeps typed-bus the single owner of `new`-arg calls.
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: JVM,
    re: /\.(?:submit|execute|schedule|post|postDelayed)\s*\(\s*(?!new\s+[A-Z]|['"`])|\bCompletableFuture\.(?:supply|run)Async\s*\(|\bnew\s+Thread\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => identArgKey(orig),
  },
  {
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: CS,
    re: /\bTask\.Run\s*\(|\bTask\.Factory\.StartNew\s*\(|\bParallel\.(?:For|ForEach|Invoke)\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => identArgKey(orig),
  },
  {
    // asyncio.create_task(coro()) / loop.call_soon(cb) / Thread(target=worker).
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: PY,
    re: /\b(?:create_task|ensure_future|call_soon|call_later|run_coroutine_threadsafe)\s*\(|\b(?:Thread|Process)\s*\([^)\n]{0,80}?target\s*=/g,
    keyWindow: 80,
    keyFrom: (orig) => identArgKey(orig),
  },
  {
    // dispatch_async(queue, block) / queue.async { } / .asyncAfter(.
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: SWIFT_OBJC,
    re: /\bdispatch_(?:async|sync|after)\s*\(|\.async(?:After)?\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      // dispatch_*'s first arg is the QUEUE, not the work — take the arg
      // after the comma instead; a trailing closure yields nothing.
      if (/^dispatch_/.test(orig)) {
        const m = orig.match(/,\s*(?:[A-Za-z_]\w*\s*[=:]\s*)?([A-Za-z_][\w.]*)\s*[,)]/);
        const key = m ? targetSegment(m[1]!) : undefined;
        return key ? { key } : undefined;
      }
      return identArgKey(orig);
    },
  },
  {
    form: 'async-dispatch',
    label: 'async dispatch',
    langs: RUST,
    re: /\bthread::spawn\s*\(|\btokio::spawn\s*\(|\.spawn\s*\(/g,
    keyWindow: 80,
    keyFrom: (orig) => identArgKey(orig),
  },
  {
    // container.get('x') / provider.GetService(typeof(Foo)) /
    // injector.resolve<Foo>() / beanFactory.getBean(Foo.class) / App::make —
    // service-location: the receiver names the container, the arg is the key.
    form: 'service-locator',
    label: 'service-locator lookup',
    re: /\b(?:container|context|provider|locator|injector|kernel|app|services|serviceProvider|beanFactory|host|scope|resolver|registry|client)(?:->|\.|::)(?:get|resolve|getService|GetService|GetRequiredService|getBean|make|getInstance|lookup|select|create)\s*(?:<[^<>\n]{0,80}>)?\s*\(/g,
    keyWindow: 120,
    keyFrom: (orig) => {
      // `resolve<Foo>(` — a generic type argument is a type key.
      const gen = orig.match(/[A-Za-z]\w*\s*<\s*([A-Z][\w.]*)\s*>/);
      if (gen) return { key: lastSegment(gen[1]!), keyIsType: true };
      const paren = orig.indexOf('(');
      if (paren === -1) return undefined;
      const args = orig.slice(paren);
      const t = args.match(/\btypeof\s*\(?\s*([A-Z]\w*)/)
        ?? args.match(/([A-Z]\w*)\s*::\s*class\b/)
        ?? args.match(/\b([A-Z]\w*)\s*\.\s*class\b/)
        ?? args.match(/\(\s*([A-Z]\w*)\s*[,)]/);
      if (t) return { key: t[1]!, keyIsType: true };
      const key = singleStringLiteral(args);
      return key ? { key } : undefined;
    },
  },
  {
    // emitter.emit('saved') / store.dispatch({type:'x'}) /
    // dispatchEvent(new CustomEvent('x')) / WP do_action('x') — the dispatch
    // key is IN the call. Announced-with-key even though the emitter
    // synthesizer connects matching literal emit→handler pairs statically:
    // this scanner only runs on a flow that FAILED to connect, where an
    // honest keyed site beats silence.
    //
    // The `{…:…}` arm covers `verb({type:'x'})` object-literal dispatch across
    // the same verb family — and a QUOTED first prop (`{'type':'x'}` /
    // `{"type":…}`): string CONTENTS are blanked before matching, so a quoted
    // key arrives as quote-blanks-quote (`'    '`) and `type` itself can only
    // match the bare form. postMessage stays with ipc-channel (its `type:`
    // fallback already keys the object-payload shape).
    form: 'literal-key-dispatch',
    label: 'literal-keyed dispatch',
    re: /\.(?:emit|dispatch|trigger|fire|publish|broadcast|sendAsync|sendMessage|send|post|notify|announce|raise|deliver|executeCommand|convertAndSend|basicPublish)\s*\(\s*(['"`])|\.(?:emit|dispatch|trigger|fire|publish|broadcast|sendAsync|sendMessage|send|post|notify|announce|raise|deliver|executeCommand|convertAndSend|basicPublish)\s*\(\s*\{\s*(?:type\s*:|['"`][^'"`\n]*['"`]\s*:)|\.dispatchEvent\s*\(\s*new\s+\w*Event\s*\(|\b(?:do_action|apply_filters|do_shortcode)\s*\(\s*(['"`])/g,
    keyWindow: 100,
    keyFrom: (orig) => {
      const t = orig.match(/['"]?type['"]?\s*:\s*(['"`])([\w.:-]{1,64})\1/)
        ?? orig.match(/(['"`])([\w.:-]{1,64})\1/);
      return t ? { key: t[2]! } : undefined;
    },
  },
  {
    // ipcMain.handle('ch') / ipcRenderer.invoke('ch') / worker.postMessage /
    // chrome.runtime.sendMessage — the channel literal is the key.
    form: 'ipc-channel',
    label: 'IPC channel',
    langs: JS_FAMILY,
    re: /\bipcMain\.(?:on|once|handle)\s*\(|\bipcRenderer\.(?:invoke|send|sendTo|sendToHost|on)\s*\(|\bonmessage\s*\(|\.onmessage\s*=|\bnew\s+Worker(?:s|Thread)?\s*\(|\bchrome\.(?:runtime|tabs)\.sendMessage\s*\(|\bchrome\.(?:runtime|tabs)\.onMessage\b|\.postMessage\s*\(/g,
    keyWindow: 100,
    keyFrom: (orig) => {
      // Only the ipc*/postMessage/chrome shapes carry a channel literal;
      // `new Worker('file.ts')` args are script paths, not channels.
      if (!/^(?:ipc|\.postMessage|chrome)/.test(orig)) return undefined;
      const m = orig.match(/\(\s*(['"`])([\w.:-]{1,64})\1/);
      if (m) return { key: m[2]! };
      // `sendTo(id, {type:'x'})` — the channel position holds a numeric id;
      // the payload object's `type` field is the dispatch key instead.
      const t = orig.match(/['"]?type['"]?\s*:\s*(['"`])([\w.:-]{1,64})\1/);
      return t ? { key: t[2]! } : undefined;
    },
  },
  {
    // C# delegate/event invoke: `handler.Invoke(args)` dispatches to whoever
    // subscribed (receiver = key); `evt += Handler` IS the subscription
    // (handler ident = key). Separate from `reflection` because its lowercase
    // `.invoke` never matches `.Invoke` and "reflective" is the wrong label.
    form: 'delegate-invoke',
    label: 'delegate/event invoke',
    langs: CS,
    // `x.y.Invoke` (member chain) or `lowercaseReceiver.Invoke` — a bare
    // `PascalCase.Invoke(` is a static class call (Parallel.Invoke), which
    // async-dispatch owns.
    re: /[\w$]+\??\.[\w$]+\??\.(?:Begin)?Invoke\s*\(|\b[a-z_][\w$]*\??\.(?:Begin)?Invoke\s*\(|\.[A-Za-z_]\w*\s*\+=\s*[A-Za-z_]/g,
    keyWindow: 80,
    keyFrom: (orig) => {
      const sub = orig.match(/\+=\s*([A-Za-z_][\w.]*)/);
      if (sub) {
        const key = targetSegment(sub[1]!);
        return key ? { key } : undefined;
      }
      const inv = orig.match(/([A-Za-z_][\w$]*)\??\.(?:Begin)?Invoke\s*\(/);
      if (inv) return { key: inv[1]! };
      return identArgKey(orig);
    },
  },
  {
    // RxJS / MobX / Vue: .subscribe/.pipe/.flatMap chains, autorun, reaction,
    // watchEffect, computed, observable, observe( — the reactive frontier
    // where synthesis is forbidden. Announce-only: no static key exists.
    form: 'reactive-chain',
    label: 'reactive chain',
    langs: JS_FAMILY,
    re: /\.(?:subscribe|pipe|flatMap|mergeMap|switchMap|concatMap|exhaustMap)\s*\(|\b(?:autorun|reaction|watchEffect|createEffect|computed|observable|observe)\s*\(/g,
  },
  {
    // Reactor/RxJava: Mono/Flux chains end at .subscribe/.block*/.blockingGet.
    form: 'reactive-chain',
    label: 'reactive chain',
    langs: JVM,
    re: /\.(?:subscribe|flatMap|concatMap|switchMap|then|doOnNext|observeOn|subscribeOn|block|blockFirst|blockLast|blockingGet)\s*\(/g,
  },
  {
    form: 'reactive-chain',
    label: 'reactive chain',
    langs: CS,
    re: /\.Subscribe\s*\(/g,
  },
];

/** Map a Node.language to the comment-stripper's language set. */
function commentLang(language: string): CommentLang | null {
  switch (language) {
    case 'python': return 'python';
    case 'ruby': return 'ruby';
    case 'rust': return 'rust';
    case 'php': return 'php';
    case 'go': return 'go';
    case 'javascript':
    case 'jsx':
      return 'javascript';
    case 'typescript':
    case 'tsx':
    case 'vue':
    case 'svelte':
    case 'astro':
    case 'arkts':
      return 'typescript';
    case 'java':
    case 'kotlin':
    case 'scala':
    case 'dart':
      return 'java';
    case 'csharp': return 'csharp';
    case 'swift': return 'swift';
    case 'c':
    case 'cpp':
    case 'objc':
    case 'objcpp':
      return 'java'; // C-style comments + double-quoted strings — close enough for blanking
    default: return null;
  }
}

const MAX_MATCHES_PER_BODY = 3;
const MAX_BODY_CHARS = 60_000; // a god-function tail is still scannable; beyond this, truncate


/**
 * Scan one symbol's body for dynamic-dispatch sites.
 *
 * @param body       the symbol's source text (sliced from the file)
 * @param language   Node.language of the symbol
 * @param fileStartLine 1-based line where `body` starts in its file — returned
 *                      line numbers are absolute file lines.
 */
export function scanDynamicDispatch(body: string, language: string, fileStartLine: number): BoundaryMatch[] {
  const original = body.length > MAX_BODY_CHARS ? body.slice(0, MAX_BODY_CHARS) : body;
  const lang = commentLang(language);
  const stripped = blankStringContents(lang ? stripCommentsForRegex(original, lang) : original);

  const out: BoundaryMatch[] = [];
  const seen = new Map<string, BoundaryMatch>(); // form+key → first match (counts extras)

  if (language === 'python') scanPythonGetattr(stripped, original, fileStartLine, out, seen);

  for (const spec of FORMS) {
    if (out.length >= MAX_MATCHES_PER_BODY) break;
    if (spec.langs && !spec.langs.has(language)) continue;
    spec.re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = spec.re.exec(stripped)) !== null) {
      let sliceEnd = m.index + m[0].length;
      if (spec.keyWindow) {
        const windowEnd = Math.min(original.length, sliceEnd + spec.keyWindow);
        const nl = original.indexOf('\n', sliceEnd);
        sliceEnd = nl !== -1 && nl < windowEnd ? nl : windowEnd;
      }
      const origSlice = original.slice(m.index, sliceEnd);
      const derived = spec.keyFrom?.(origSlice);
      const dedupeKey = `${spec.form}|${derived?.key ?? ''}`;
      const prior = seen.get(dedupeKey);
      if (prior) {
        prior.moreSites = (prior.moreSites ?? 0) + 1;
        continue;
      }
      const line = fileStartLine + countNewlines(original, m.index);
      const match: BoundaryMatch = {
        form: spec.form,
        label: spec.label,
        snippet: snippetAround(original, m.index),
        line,
        ...(derived ?? {}),
      };
      seen.set(dedupeKey, match);
      out.push(match);
      if (out.length >= MAX_MATCHES_PER_BODY) return out;
    }
  }
  return out;
}

/**
 * Python getattr dispatch — handled in code, not the FORMS table, because real
 * getattr calls have nested-call arguments spanning lines
 * (`getattr(self, request.method.lower(),\n  self.http_method_not_allowed)` —
 * DRF's APIView.dispatch) that a regex argument class can't bound. Two shapes:
 *   getattr(obj, name)(args)                      → immediate call
 *   handler = getattr(obj, name) ... handler(...)  → assigned, called later
 */
const GETATTR_RE = /\bgetattr\s*\(/g;
const MAX_GETATTR_ARGS = 300;

function scanPythonGetattr(stripped: string, original: string, fileStartLine: number, out: BoundaryMatch[], seen: Map<string, BoundaryMatch>): void {
  GETATTR_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = GETATTR_RE.exec(stripped)) !== null && out.length < MAX_MATCHES_PER_BODY) {
    const open = m.index + m[0].length - 1;
    const close = matchBalancedParen(stripped, open);
    if (close === -1) continue;

    let form: string | undefined;
    let label = '';
    // Immediate call: getattr(...)(
    const after = stripped.slice(close + 1, close + 8);
    if (/^\s*\(/.test(after)) {
      form = 'getattr-call';
      label = 'getattr dispatch';
    } else {
      // Assigned form: look back for `name =` and forward for `name(`.
      const lineStart = stripped.lastIndexOf('\n', m.index) + 1;
      const before = stripped.slice(lineStart, m.index);
      const assign = before.match(/(\w+)\s*=\s*$/);
      if (assign && new RegExp(`\\b${assign[1]}\\s*\\(`).test(stripped.slice(close + 1))) {
        form = 'getattr-assign';
        label = 'getattr dispatch (assigned, called later)';
      }
    }
    if (!form) continue;

    const key = singleStringLiteral(original.slice(open + 1, close));
    const dedupeKey = `${form}|${key ?? ''}`;
    const prior = seen.get(dedupeKey);
    if (prior) {
      prior.moreSites = (prior.moreSites ?? 0) + 1;
      continue;
    }
    const match: BoundaryMatch = {
      form,
      label,
      snippet: snippetAround(original, m.index),
      line: fileStartLine + countNewlines(original, m.index),
      ...(key ? { key } : {}),
    };
    seen.set(dedupeKey, match);
    out.push(match);
  }
}

/** Index of the `)` balancing `text[open]`, or -1 (cap: MAX_GETATTR_ARGS chars). */
function matchBalancedParen(text: string, open: number): number {
  let depth = 0;
  const end = Math.min(text.length, open + MAX_GETATTR_ARGS);
  for (let i = open; i < end; i++) {
    const c = text[i];
    if (c === '(') depth++;
    else if (c === ')' && --depth === 0) return i;
  }
  return -1;
}

function countNewlines(text: string, end: number): number {
  let n = 0;
  for (let i = 0; i < end; i++) if (text.charCodeAt(i) === 10) n++;
  return n;
}

/** The full source line containing `index`, trimmed and capped for display. */
function snippetAround(text: string, index: number): string {
  const lineStart = text.lastIndexOf('\n', index) + 1;
  let lineEnd = text.indexOf('\n', index);
  if (lineEnd === -1) lineEnd = text.length;
  const line = text.slice(lineStart, lineEnd).trim();
  return line.length > 120 ? line.slice(0, 117) + '...' : line;
}
