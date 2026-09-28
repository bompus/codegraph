/**
 * Dynamic-boundary surfacing (#687).
 *
 * When the flow an agent asked codegraph_explore about does NOT fully connect,
 * the Flow section announces WHERE the static path ends — the dynamic-dispatch
 * site (computed member call, getattr, typed bus, runtime-keyed emit), with
 * candidate targets when a key is statically visible — instead of silently
 * showing nothing. Deterministic, query-time only, no graph mutation, and a
 * fully connected flow must never produce the section.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';
import { scanDynamicDispatch } from '../src/mcp/dynamic-boundaries';

// ---------------------------------------------------------------------------
// Unit: the scanner
// ---------------------------------------------------------------------------

describe('scanDynamicDispatch', () => {
  it('detects a computed member call with a literal key', () => {
    const body = `function go(p) {\n  table['save'](p);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 10);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('computed-call');
    expect(m[0]!.key).toBe('save');
    expect(m[0]!.line).toBe(11); // absolute: body starts at file line 10
    expect(m[0]!.snippet).toContain("table['save'](p)");
  });

  it('detects a computed member call with a runtime key (no key extracted)', () => {
    const body = `dispatch(action) {\n  this.handlers[action.type](action.payload);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('computed-call');
    expect(m[0]!.key).toBeUndefined();
  });

  it('does not fire on dispatch shapes inside comments or strings', () => {
    const body = [
      'function safe() {',
      "  // this.handlers[action.type](payload) — commented out",
      '  const doc = "call handlers[key](p) to dispatch";',
      '  return 1;',
      '}',
    ].join('\n');
    expect(scanDynamicDispatch(body, 'typescript', 1)).toHaveLength(0);
  });

  it('does not treat plain indexing or array literals as dispatch', () => {
    const body = `function f(xs) {\n  const a = xs[0];\n  const b = [1, 2, 3];\n  return a + b[1];\n}`;
    expect(scanDynamicDispatch(body, 'typescript', 1)).toHaveLength(0);
  });

  it('detects python getattr immediate-call', () => {
    const body = `def run(self, name):\n    return getattr(self, name)(1)`;
    const m = scanDynamicDispatch(body, 'python', 5);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('getattr-call');
  });

  it('detects two-step getattr only when the assigned name is called later', () => {
    const called = `def process(self, kind, p):\n    handler = getattr(self, 'handle_' + kind)\n    return handler(p)`;
    const m = scanDynamicDispatch(called, 'python', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('getattr-assign');
    expect(m[0]!.key).toBe('handle_'); // the literal prefix — enough to shortlist

    const notCalled = `def peek(self, kind):\n    handler = getattr(self, 'handle_' + kind)\n    return handler`;
    expect(scanDynamicDispatch(notCalled, 'python', 1)).toHaveLength(0);
  });

  it('detects ruby send with a symbol key', () => {
    const body = `def run(name)\n  target.send(:handle_save, 1)\nend`;
    const m = scanDynamicDispatch(body, 'ruby', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('ruby-send');
    expect(m[0]!.key).toBe('handle_save');
  });

  it('detects typed message dispatch and marks the key as a type', () => {
    const body = `public async Task<int> Create(CreateCmd c) {\n  return await _mediator.Send(new CreateTodoItemCommand(c));\n}`;
    const m = scanDynamicDispatch(body, 'csharp', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('typed-bus');
    expect(m[0]!.key).toBe('CreateTodoItemCommand');
    expect(m[0]!.keyIsType).toBe(true);
  });

  it('detects runtime-keyed emit and announces literal-keyed emit with its key', () => {
    const runtime = `notify(name, data) {\n  this.emitter.emit(name, data);\n}`;
    const m = scanDynamicDispatch(runtime, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('var-key-dispatch');

    // Intent change: a literal key at a dispatch site is now announced WITH
    // the key. The event-bus synthesizer still connects matching literal
    // emit→handler pairs statically, so this scanner only ever sees a
    // literal-keyed site on a flow that FAILED to connect — where an honest
    // keyed announcement beats silence.
    const literal = `notify(data) {\n  this.emitter.emit('saved', data);\n}`;
    const lm = scanDynamicDispatch(literal, 'typescript', 1);
    expect(lm).toHaveLength(1);
    expect(lm[0]!.form).toBe('literal-key-dispatch');
    expect(lm[0]!.key).toBe('saved');
  });

  it('dedupes repeated same-form/same-key sites and counts the extras', () => {
    const body = [
      'route(a) {',
      '  this.table[a.type](a.p);',
      '  this.table[a.kind](a.p);',
      '  this.table[a.name](a.p);',
      '}',
    ].join('\n');
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.moreSites).toBe(2);
  });

  it('detects reflective dispatch with a literal method name as key', () => {
    const body = `public void run(Object o) {\n  o.getClass().getMethod("handlePing").invoke(o);\n}`;
    const m = scanDynamicDispatch(body, 'java', 1);
    expect(m.length).toBeGreaterThanOrEqual(1);
    expect(m[0]!.form).toBe('reflection');
    expect(m[0]!.key).toBe('handlePing');
  });

  // --- async-dispatch ------------------------------------------------------

  it('detects async dispatch and extracts the callback identifier as key', () => {
    const body = `function schedule(p) {\n  setTimeout(handleTick, 100);\n  return p.then(processNext);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(2);
    expect(m.every((x) => x.form === 'async-dispatch')).toBe(true);
    expect(m[0]!.key).toBe('handleTick');
    expect(m[1]!.key).toBe('processNext');
  });

  it('announces async dispatch with no key for anonymous callbacks', () => {
    const body = `function schedule() {\n  setTimeout(() => tick(), 0);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('async-dispatch');
    expect(m[0]!.key).toBeUndefined();
  });

  it('does not fire async-dispatch on .then with an arrow-function arg', () => {
    const body = `function load(p) {\n  return p.then((v) => v + 1);\n}`;
    expect(scanDynamicDispatch(body, 'typescript', 1)).toHaveLength(0);
  });

  it('detects a go routine with the callee name as key', () => {
    const body = `func serve() {\n\tgo worker.Run()\n}`;
    const m = scanDynamicDispatch(body, 'go', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('async-dispatch');
    expect(m[0]!.key).toBe('Run');
  });

  it('detects a python Thread target kwarg as the key', () => {
    const body = `def start(self):\n    t = Thread(target=self.worker)\n    t.start()`;
    const m = scanDynamicDispatch(body, 'python', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('async-dispatch');
    expect(m[0]!.key).toBe('worker');
  });

  it('detects executor submit but yields new-X args to typed-bus', () => {
    const body = `void run(Runnable task) {\n  executor.submit(task);\n  executor.execute(new MyJob());\n}`;
    const m = scanDynamicDispatch(body, 'java', 1);
    expect(m).toHaveLength(2);
    // Sites emit in FORMS-table order, not source order — typed-bus is earlier.
    expect(m[0]!.form).toBe('typed-bus');
    expect(m[0]!.key).toBe('MyJob');
    expect(m[1]!.form).toBe('async-dispatch');
    expect(m[1]!.key).toBe('task');
  });

  // --- service-locator -----------------------------------------------------

  it('detects a service-locator lookup with a typeof() type key', () => {
    const body = `public void Configure() {\n  var svc = provider.GetService(typeof(FooService));\n}`;
    const m = scanDynamicDispatch(body, 'csharp', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('service-locator');
    expect(m[0]!.key).toBe('FooService');
    expect(m[0]!.keyIsType).toBe(true);
  });

  it('detects a container getBean with a .class type key', () => {
    const body = `void wire() {\n  beanFactory.getBean(OrderService.class);\n}`;
    const m = scanDynamicDispatch(body, 'java', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('service-locator');
    expect(m[0]!.key).toBe('OrderService');
    expect(m[0]!.keyIsType).toBe(true);
  });

  it('detects a generic resolve<T> as a type key', () => {
    const body = `function boot() {\n  const h = container.resolve<FooHandler>();\n  return h;\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('service-locator');
    expect(m[0]!.key).toBe('FooHandler');
    expect(m[0]!.keyIsType).toBe(true);
  });

  it('detects a string-keyed locator lookup with a plain key', () => {
    const body = `def build(self):\n    return self.services.get('payment.gateway')`;
    const m = scanDynamicDispatch(body, 'python', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('service-locator');
    expect(m[0]!.key).toBe('payment.gateway');
    expect(m[0]!.keyIsType).toBeUndefined();
  });

  it('does not fire service-locator on a non-container receiver', () => {
    const body = `def fetch(self):\n    return self.cache.get('payment.gateway')`;
    expect(scanDynamicDispatch(body, 'python', 1)).toHaveLength(0);
  });

  // --- literal-key-dispatch -------------------------------------------------

  it('detects redux-style dispatch({type: ...}) with the action type as key', () => {
    const body = `function checkout() {\n  store.dispatch({ type: 'cart/submit', payload: 1 });\n}`;
    // '/' is outside the key charset — the site is announced without a key.
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('literal-key-dispatch');

    const keyed = `function checkout() {\n  store.dispatch({ type: 'submitCart' });\n}`;
    const km = scanDynamicDispatch(keyed, 'typescript', 1);
    expect(km[0]!.key).toBe('submitCart');
  });

  it('detects a wordpress hook dispatch with the hook name as key', () => {
    const body = `function save($id) {\n  do_action('save_post', $id);\n}`;
    const m = scanDynamicDispatch(body, 'php', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('literal-key-dispatch');
    expect(m[0]!.key).toBe('save_post');
  });

  it('detects dispatchEvent(new CustomEvent("literal"))', () => {
    const body = `function done() {\n  this.dispatchEvent(new CustomEvent('saved', { detail: 1 }));\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('literal-key-dispatch');
    expect(m[0]!.key).toBe('saved');
  });

  it('detects a QUOTED `type` object-literal key — contents are blanked in the scanned text', () => {
    // `{'type':'submitCart'}` — the scanner blanks string contents, so the
    // quoted key arrives as `{'    ':' '}`; the quoted-prop arm still sees the
    // dispatch shape and keyFrom reads the key off the ORIGINAL text.
    const body = `function checkout() {\n  store.dispatch({'type':'submitCart'});\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('literal-key-dispatch');
    expect(m[0]!.key).toBe('submitCart');

    const dq = scanDynamicDispatch(`function checkout() {\n  store.dispatch({"type": "pay"});\n}`, 'typescript', 1);
    expect(dq[0]!.key).toBe('pay');
  });

  it('detects {type:…} object-literal dispatch on the non-dispatch verbs', () => {
    const body = [
      'function tick() {',
      "  emitter.emit({ type: 'saved' });",
      "  socket.send({ type: 'ping' });",
      '}',
    ].join('\n');
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(2);
    expect(m.every((x) => x.form === 'literal-key-dispatch')).toBe(true);
    expect(m.map((x) => x.key).sort()).toEqual(['ping', 'saved']);
    // postMessage keeps its ipc-channel form — the type: fallback keys it there.
    const pm = scanDynamicDispatch(`function tick() {\n  channel.postMessage({'type':'scroll'});\n}`, 'typescript', 1);
    expect(pm).toHaveLength(1);
    expect(pm[0]!.form).toBe('ipc-channel');
    expect(pm[0]!.key).toBe('scroll');
  });

  it('does not fire the object-literal arm on a bare non-type key', () => {
    // `payload` isn't `type` and isn't quoted — nothing to announce with.
    const body = `function save() {\n  store.dispatch({payload: 1});\n}`;
    expect(scanDynamicDispatch(body, 'typescript', 1)).toHaveLength(0);
  });

  // --- ipc-channel ----------------------------------------------------------

  it('detects an electron ipcMain handler with the channel as key', () => {
    const body = `function wire() {\n  ipcMain.handle('get-config', handleGetConfig);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('ipc-channel');
    expect(m[0]!.key).toBe('get-config');
  });

  it('detects worker messaging sites; a Worker script path is not a key', () => {
    const body = `function spawn() {\n  const w = new Worker('worker.ts');\n  w.onmessage = handleMsg;\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    // Both sites share form `ipc-channel` with no key → dedupe folds the
    // second into moreSites.
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('ipc-channel');
    expect(m[0]!.key).toBeUndefined();
    expect(m[0]!.moreSites).toBe(1);
  });

  it('extracts the payload `type` key when the ipc channel position is a numeric id', () => {
    const body = `function wire() {\n  ipcRenderer.sendTo(3, { type: 'refresh' });\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('ipc-channel');
    expect(m[0]!.key).toBe('refresh');
  });

  // --- delegate-invoke ------------------------------------------------------

  it('detects a c# event subscription with the handler as key', () => {
    const body = `public void Wire() {\n  button.Click += OnClicked;\n}`;
    const m = scanDynamicDispatch(body, 'csharp', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('delegate-invoke');
    expect(m[0]!.key).toBe('OnClicked');
  });

  it('detects a c# delegate Invoke with the event name as key', () => {
    const body = `public void Fire() {\n  this.Click.Invoke(args);\n}`;
    const m = scanDynamicDispatch(body, 'csharp', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('delegate-invoke');
    expect(m[0]!.key).toBe('Click');
  });

  // --- reactive-chain --------------------------------------------------------

  it('detects an rxjs subscribe chain (announce-only, no key)', () => {
    const body = `function bind() {\n  this.obs$.pipe(map(f)).subscribe(g);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('reactive-chain');
    expect(m[0]!.key).toBeUndefined();
    expect(m[0]!.moreSites).toBe(1); // .pipe( and .subscribe( share form|key
  });

  it('detects MobX autorun / reactor subscribe per language', () => {
    const js = scanDynamicDispatch(`function f() {\n  autorun(() => track());\n}`, 'typescript', 1);
    expect(js[0]!.form).toBe('reactive-chain');
    const java = scanDynamicDispatch(`void f() {\n  flux.subscribeOn(Schedulers.boundedElastic()).subscribe(r);\n}`, 'java', 1);
    expect(java[0]!.form).toBe('reactive-chain');
    const cs = scanDynamicDispatch(`void f() {\n  observable.Subscribe(handler);\n}`, 'csharp', 1);
    expect(cs[0]!.form).toBe('reactive-chain');
  });

  // --- extensions to existing forms -----------------------------------------

  it('detects a ruby define_method and const_get (type key)', () => {
    const body = `def self.install\n  define_method(:save_all) { |x| x }\nend`;
    const m = scanDynamicDispatch(body, 'ruby', 1);
    expect(m[0]!.form).toBe('ruby-send');
    expect(m[0]!.key).toBe('save_all');

    const cg = scanDynamicDispatch(`def lookup\n  const_get("FooHandler")\nend`, 'ruby', 1);
    expect(cg[0]!.form).toBe('ruby-send');
    expect(cg[0]!.key).toBe('FooHandler');
    expect(cg[0]!.keyIsType).toBe(true);
  });

  it('detects php expression-method and __call dispatch', () => {
    const body = `function run($m) {\n  $this->{$m}();\n  $proxy->__call('x');\n}`;
    const m = scanDynamicDispatch(body, 'php', 1);
    expect(m.length).toBeGreaterThanOrEqual(1);
    expect(m.every((x) => x.form === 'php-dynamic')).toBe(true);
  });

  it('detects ServiceLoader.load with a .class type key', () => {
    const body = `void load() {\n  ServiceLoader.load(Plugin.class);\n}`;
    const m = scanDynamicDispatch(body, 'java', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('reflection');
    expect(m[0]!.key).toBe('Plugin');
    expect(m[0]!.keyIsType).toBe(true);
  });

  it('detects an emitter.on registration with a runtime key', () => {
    const body = `function bind(name, fn) {\n  emitter.on(name, fn);\n}`;
    const m = scanDynamicDispatch(body, 'typescript', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('var-key-dispatch');
    // subscribe stays with reactive-chain — var-key-dispatch must not claim it
    const sub = `function bind(fn) {\n  emitter.subscribe(fn);\n}`;
    expect(scanDynamicDispatch(sub, 'typescript', 1)[0]!.form).toBe('reactive-chain');
  });

  it('detects the new typed-bus verbs', () => {
    const body = `public void Go() {\n  bus.Submit(new RebuildIndex());\n}`;
    const m = scanDynamicDispatch(body, 'csharp', 1);
    expect(m).toHaveLength(1);
    expect(m[0]!.form).toBe('typed-bus');
    expect(m[0]!.key).toBe('RebuildIndex');
  });
});

// ---------------------------------------------------------------------------
// Integration: codegraph_explore output
// ---------------------------------------------------------------------------

describe('codegraph_explore — dynamic boundaries', () => {
  let testDir: string;
  let cg: CodeGraph;
  let handler: ToolHandler;

  const setup = async (files: Record<string, string>, include: string[]) => {
    testDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-boundary-'));
    const src = path.join(testDir, 'src');
    fs.mkdirSync(src, { recursive: true });
    for (const [name, content] of Object.entries(files)) {
      fs.writeFileSync(path.join(src, name), content);
    }
    cg = CodeGraph.initSync(testDir, { config: { include, exclude: [] } });
    await cg.indexAll();
    handler = new ToolHandler(cg);
  };

  afterEach(() => {
    if (cg) cg.destroy();
    if (testDir && fs.existsSync(testDir)) fs.rmSync(testDir, { recursive: true, force: true });
  });

  it('announces a template import boundary from indexed source (#1967)', async () => {
    await setup({
      'loader.ts': [
        'export async function loadLocale(lang: string) {',
        '  return import(`./locales/${lang}.js`);',
        '}',
      ].join('\n'),
      'locale.ts': 'export function translate() { return "hello"; }',
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'loadLocale translate' });
    const text = res.content[0].text as string;
    expect(res.isError).not.toBe(true);
    expect(text).toContain('**Dynamic boundaries');
    expect(text).toContain('dynamic import');
    expect(text).toMatch(/loader\.ts:2/);
    expect(text).not.toContain('candidates for key');
  });

  it('announces the boundary site and shortlists the keyed candidate', async () => {
    await setup({
      'router.ts': [
        'type Handler = (p: unknown) => void;',
        'export class Router {',
        '  private table: Record<string, Handler> = {};',
        '  add(key: string, fn: Handler) { this.table[key] = fn; }',
        '  routeSave(payload: unknown) {',
        "    this.table['save'](payload);",
        '  }',
        '}',
      ].join('\n'),
      'handlers.ts': [
        "import { Router } from './router';",
        'export function onSave(payload: unknown) { return payload; }',
        'export function wire(r: Router) { r.add("save", onSave); }',
      ].join('\n'),
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'routeSave onSave' });
    const text = res.content[0].text as string;

    expect(text).toContain('**Dynamic boundaries');
    expect(text).toContain('computed member call');
    expect(text).toMatch(/router\.ts:6/); // the exact dispatch site
    expect(text).toContain('candidates for key `save`');
    expect(text).toContain('onSave');
    expect(text).toContain('← you named this');
    // Honesty constraint: never steer the agent to Read.
    expect(text).not.toMatch(/\buse Read\b/i);
  });

  it('announces a runtime-keyed boundary with no candidate list', async () => {
    await setup({
      'bus.ts': [
        'type Action = { type: string; payload?: unknown };',
        'type Handler = (p: unknown) => void;',
        'export class Bus {',
        '  private table: Record<string, Handler> = {};',
        '  route(action: Action) {',
        '    this.table[action.type](action.payload);',
        '  }',
        '}',
      ].join('\n'),
      'handlers.ts': 'export function onSave(payload: unknown) { return payload; }',
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'route onSave' });
    const text = res.content[0].text as string;

    expect(text).toContain('**Dynamic boundaries');
    expect(text).toContain('computed member call');
    expect(text).not.toContain('candidates for key'); // runtime key → no shortlist to claim
  });

  it('surfaces the boundary even when the other symbol is not in the graph', async () => {
    await setup({
      'bus.ts': [
        'type Action = { type: string; payload?: unknown };',
        'type Handler = (p: unknown) => void;',
        'export class Bus {',
        '  private table: Record<string, Handler> = {};',
        '  route(action: Action) {',
        '    this.table[action.type](action.payload);',
        '  }',
        '}',
      ].join('\n'),
    }, ['**/*.ts']);

    // `processPayment` does not exist anywhere — only `route` resolves.
    const res = await handler.execute('codegraph_explore', { query: 'route processPayment' });
    const text = res.content[0].text as string;
    expect(text).toContain('**Dynamic boundaries');
  });

  it('renders a direct synthesized emit→handler hop as a dynamic-dispatch link (#687 criterion 1)', async () => {
    // Custom EventBus with a LITERAL key: the event-emitter synthesizer
    // bridges emit→handler, but the 2-node chain was invisible — too short
    // for the Flow section and skipped by the links section as "in-chain".
    await setup({
      'bus.ts': [
        'type Handler = (p: unknown) => void;',
        'export class EventBus {',
        '  private listeners: Record<string, Handler[]> = {};',
        '  on(event: string, fn: Handler) { (this.listeners[event] ??= []).push(fn); }',
        '  emit(event: string, payload: unknown) { for (const fn of this.listeners[event] ?? []) fn(payload); }',
        '}',
        'export const bus = new EventBus();',
      ].join('\n'),
      'billing.ts': [
        "import { bus } from './bus';",
        'export function settleInvoice(payload: unknown) { return payload; }',
        "bus.on('invoice.settled', settleInvoice);",
      ].join('\n'),
      'checkout.ts': [
        "import { bus } from './bus';",
        'export function completeCheckout(order: unknown) {',
        "  bus.emit('invoice.settled', order);",
        '}',
      ].join('\n'),
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'completeCheckout settleInvoice' });
    const text = res.content[0].text as string;

    expect(text).toContain('**Dynamic-dispatch links among your symbols');
    expect(text).toMatch(/completeCheckout → settleInvoice/);
    expect(text).toContain('invoice.settled');
    // Connected via the synthesized edge — no boundary to announce.
    expect(text).not.toContain('**Dynamic boundaries');
  });

  it('never adds the section to a fully connected flow', async () => {
    await setup({
      'pipeline.ts': [
        'export function stepOne() { return stepTwo(); }',
        'export function stepTwo() { return stepThree(); }',
        'export function stepThree() { return 3; }',
      ].join('\n'),
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'stepOne stepThree' });
    const text = res.content[0].text as string;
    expect(text).toContain('**Flow');
    expect(text).not.toContain('**Dynamic boundaries');
  });

  it('python getattr dispatch surfaces with a prefix-key candidate', async () => {
    await setup({
      'service.py': [
        'class Service:',
        '    def handle_save(self, payload):',
        '        return payload',
        '',
        '    def process(self, kind, payload):',
        "        handler = getattr(self, 'handle_' + kind)",
        '        return handler(payload)',
      ].join('\n'),
    }, ['**/*.py']);

    const res = await handler.execute('codegraph_explore', { query: 'process handle_save' });
    const text = res.content[0].text as string;

    expect(text).toContain('**Dynamic boundaries');
    expect(text).toContain('getattr');
    expect(text).toContain('handle_save');
  });
});

// ---------------------------------------------------------------------------
// Integration: interface/registry dispatch (a named method has many impls)
// ---------------------------------------------------------------------------

describe('codegraph_explore — interface dispatch', () => {
  let testDir: string;
  let cg: CodeGraph;
  let handler: ToolHandler;

  const setup = async (files: Record<string, string>, include: string[]) => {
    testDir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-iface-'));
    const src = path.join(testDir, 'src');
    fs.mkdirSync(src, { recursive: true });
    for (const [name, content] of Object.entries(files)) {
      fs.writeFileSync(path.join(src, name), content);
    }
    cg = CodeGraph.initSync(testDir, { config: { include, exclude: [] } });
    await cg.indexAll();
    handler = new ToolHandler(cg);
  };

  afterEach(() => {
    if (cg) cg.destroy();
    if (testDir && fs.existsSync(testDir)) fs.rmSync(testDir, { recursive: true, force: true });
  });

  // 9 classes implement INodeType, each with execute(); a runtime registry lookup
  // dispatches to one. The agent names the static entry + `execute`, which can't
  // resolve to a single impl — the boundary IS the answer.
  const nodeFamily = (n: number) => {
    const names = ['Http', 'Set', 'If', 'Merge', 'Code', 'Webhook', 'Cron', 'Func', 'NoOp', 'Switch', 'Wait', 'Filter'];
    return [
      'export interface INodeType { execute(): unknown; }',
      ...names.slice(0, n).map((nm, i) => `export class ${nm}Node implements INodeType { execute() { return ${i}; } }`),
    ].join('\n');
  };
  const engine = [
    "import { registry } from './registry';",
    'export class WorkflowExecute {',
    '  processRunExecutionData() { return this.runNode(); }',
    '  runNode() { return this.executeNode(); }',
    '  executeNode() {',
    "    const nodeType = registry.get('http');",
    '    return nodeType.execute();',
    '  }',
    '}',
  ].join('\n');
  const registry = [
    "import type { INodeType } from './nodes';",
    'class Registry {',
    '  private m: Record<string, INodeType> = {};',
    '  get(k: string): INodeType { return this.m[k]!; }',
    '}',
    'export const registry = new Registry();',
  ].join('\n');

  it('announces the interface, the TRUE implementer count, and sample targets', async () => {
    await setup({ 'nodes.ts': nodeFamily(9), 'registry.ts': registry, 'engine.ts': engine }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'processRunExecutionData executeNode execute' });
    const text = res.content[0].text as string;

    expect(text).toContain('**Interface dispatch (a named method has many implementations)');
    expect(text).toMatch(/`execute` → runtime dispatch to \*\*9\*\* types implementing `INodeType`/);
    // a couple of concrete targets, with file:line
    expect(text).toMatch(/\b\w+Node\.execute` \(/);
    // never steer to Read
    expect(text).not.toMatch(/\buse Read\b/i);
  });

  it('stays SILENT on a fully connected flow with no polymorphic family', async () => {
    await setup({
      'pipeline.ts': [
        'export function stepOne() { return stepTwo(); }',
        'export function stepTwo() { return stepThree(); }',
        'export function stepThree() { return 3; }',
      ].join('\n'),
    }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'stepOne stepThree' });
    const text = res.content[0].text as string;
    expect(text).toContain('**Flow');
    expect(text).not.toContain('**Interface dispatch');
  });

  // vscode shape: many unrelated classes share a lifecycle base and happen to
  // share a member name the base never declares. That is not dispatch through
  // the base — announcing it put "runtime dispatch to 2706 types implementing
  // Disposable" at the top of every answer whose query said "extension".
  const lifecycleFamily = () => {
    const names = ['Editor', 'Terminal', 'Search', 'Debug', 'Scm', 'Chat', 'Notebook', 'Output', 'Tasks', 'Remote'];
    return [
      'export abstract class Disposable { dispose(): void {} }',
      ...names.map((nm, i) => [
        `export class ${nm}Service extends Disposable {`,
        `  get extension(): string { return '${nm.toLowerCase()}'; }`,
        `  dispose(): void { super.dispose(); }`,
        `  describe${nm}() { return this.extension + ${i}; }`,
        '}',
      ].join('\n')),
    ].join('\n');
  };

  it('stays SILENT for a shared name the common base never declares', async () => {
    await setup({ 'services.ts': lifecycleFamily() }, ['**/*.ts']);
    const res = await handler.execute('codegraph_explore', { query: 'extension describeEditor describeChat' });
    const text = res.content[0].text as string;
    expect(text).not.toMatch(/`extension` → runtime dispatch/);
  });

  it('still announces a member the common base declares', async () => {
    await setup({ 'services.ts': lifecycleFamily() }, ['**/*.ts']);
    const res = await handler.execute('codegraph_explore', { query: 'dispose describeEditor describeChat' });
    const text = res.content[0].text as string;
    expect(text).toMatch(/`dispose` → runtime dispatch to \*\*10\*\* types implementing `Disposable`/);
  });

  it('stays SILENT when the interface family is below the polymorphism threshold (3 impls)', async () => {
    await setup({ 'nodes.ts': nodeFamily(3), 'registry.ts': registry, 'engine.ts': engine }, ['**/*.ts']);

    const res = await handler.execute('codegraph_explore', { query: 'processRunExecutionData executeNode execute' });
    const text = res.content[0].text as string;
    expect(text).not.toContain('**Interface dispatch');
  });
});

describe('scanDynamicDispatch — dynamic import arguments (#1967)', () => {
  const forms = (body: string): string[] =>
    scanDynamicDispatch(body, 'typescript', 1).map((m) => m.form);

  it('flags an import built from a template literal with a substitution', () => {
    expect(forms('async function load(lang) {\n  return import(`./locales/${lang}.js`);\n}')).toEqual(['dynamic-import']);
    expect(forms('function load(name) {\n  return require(`./plugins/${name}`);\n}')).toEqual(['dynamic-import']);
  });

  it('flags an import built by string concatenation', () => {
    expect(forms("async function load(lang) {\n  return import('./locales/' + lang + '.js');\n}")).toEqual(['dynamic-import']);
    expect(forms('function load(dir) {\n  return require(dir + "/index");\n}')).toEqual(['dynamic-import']);
  });

  it('still flags an import of a bare expression', () => {
    expect(forms('async function load(p) {\n  return import(p);\n}')).toEqual(['dynamic-import']);
  });

  it('leaves a single complete literal alone', () => {
    expect(forms("async function a() {\n  return import('./fixed.js');\n}")).toEqual([]);
    expect(forms('async function a() {\n  return import(`./fixed.js`);\n}')).toEqual([]);
    expect(forms('function a() {\n  return require("./fixed");\n}')).toEqual([]);
    expect(forms("async function a() {\n  return import('./data.json', { with: { type: 'json' } });\n}")).toEqual([]);
  });
});


describe('scanDynamicDispatch — import escapes and trivia (#1967)', () => {
  it.each(['import', 'require'])('%s respects escapes and complete literals', (call) => {
    const staticArgs = [
      '`./plugins/\\${name}`',
      '`./plugins/\\\\\\${name}`',
      '`./plugins/\\`fixed`',
      '"./plugins/\\"fixed"',
      "'./plugins/\\'fixed'",
      '/* before */ `./fixed` /* after */',
      '// before\n "./fixed" // after\n',
      '`./fixed`, { with: { type: "json" } }',
      '"./${name}"',
    ];
    for (const arg of staticArgs) {
      expect(scanDynamicDispatch(`${call}(${arg})`, 'typescript', 1), arg).toEqual([]);
    }
    const runtimeArgs = [
      '`./plugins/\\\\${name}`',
      '`./plugins/\\${literal}/${name}`',
      '`./plugins/${`nested-${name}`}`',
      '/* before */ "./plugins/" /* after */ + name',
      '`./plugins/\\${literal}` + name',
      '"./plugins/".concat(name)',
      '// before\n `./plugins/${name}`, { with: { type: "json" } }',
    ];
    for (const arg of runtimeArgs) {
      const matches = scanDynamicDispatch(`function load() {\n  ${call}(${arg});\n}`, 'typescript', 20);
      expect(matches, arg).toHaveLength(1);
      expect(matches[0], arg).toMatchObject({ form: 'dynamic-import', line: 21 });
    }
  });

  it.each(['javascript', 'typescript', 'jsx', 'tsx', 'vue', 'svelte', 'astro', 'arkts'])(
    'detects runtime imports across %s', (language) => {
      expect(scanDynamicDispatch('import(`./${name}`)', language, 1)[0]?.form).toBe('dynamic-import');
      expect(scanDynamicDispatch('import(`./fixed`)', language, 1)).toEqual([]);
    },
  );
});
