import { describe, it, expect } from 'vitest';
import { expressResolver } from '../src/resolution/frameworks/express';

describe('Express handler arguments', () => {
  // Only the handler — the LAST argument — decides whether the route has an
  // inline body. An arrow inside a middleware argument used to make the whole
  // route "inline", attributing the middleware's calls to it and dropping the
  // named handler.
  const refsOf = (src: string) =>
    expressResolver.extract!('routes.ts', src).references.map((r) => `${r.referenceKind}:${r.referenceName}`);

  it('keeps the named handler when a middleware argument holds an arrow', () => {
    const refs = refsOf(`router.get('/users', rateLimit({ keyGenerator: (req) => lookup(req.ip) }), getUsers);\n`);
    expect(refs).toContain('references:getUsers');
    expect(refs).not.toContain('calls:lookup');
  });

  it('keeps the named handler after an inline arrow middleware', () => {
    const refs = refsOf(`router.get('/x', (req, res, next) => audit(req, next), getUsers);\n`);
    expect(refs).toContain('references:getUsers');
    expect(refs).not.toContain('calls:audit');
  });

  it('still reads an inline handler, wrapped or after named middleware', () => {
    expect(refsOf(`router.post('/x', asyncHandler(async (req, res) => { createItem(req.body); }));\n`)).toContain('calls:createItem');
    const afterAuth = refsOf(`router.post('/x', auth, async (req, res) => { createItem(req.body); });\n`);
    expect(afterAuth).toContain('calls:createItem');
    expect(afterAuth).not.toContain('references:auth');
  });

  it('reads a `function` expression handler', () => {
    expect(refsOf(`router.get('/x', function (req, res) { listUsers(); });\n`)).toContain('calls:listUsers');
  });

  it('a `function` handler whose body holds an arrow keeps every call', () => {
    const plain = refsOf(`router.get('/x', function (req, res) { const u = findUser(req.id); res.json(u.items.map((i) => shape(i))); });\n`);
    expect(plain).toContain('calls:findUser');
    expect(plain).toContain('calls:shape');
    const chained = refsOf(`router.route('/x').get(function (req, res) { const a = load(); a.forEach((x) => shape(x)); });\n`);
    expect(chained).toContain('calls:load');
    expect(chained).toContain('calls:shape');
  });

  it('a `function` elsewhere in the argument is not the handler', () => {
    expect(refsOf(`router.get('/x', (req, res, cb = function () {}) => { svc(); });\n`)).toContain('calls:svc');
    expect(refsOf(`router.get('/x', wrap(ctrl.list, { onError: function (e) { report(e); } }));\n`)).not.toContain('calls:report');
    expect(refsOf(`router.get('/x', asyncHandler(function (req, res) { svc(); }));\n`)).toContain('calls:svc');
  });

  it('a `function` callback before the handler inside a wrapper is not the handler', () => {
    const refs = refsOf(`router.get('/x', withErrors(function onErr(e) { report(e); }, async (req, res) => { svc(); }));\n`);
    expect(refs).toContain('calls:svc');
    expect(refs).not.toContain('calls:report');
  });

  it('a regex literal in an argument does not throw off the split', () => {
    expect(refsOf(`router.get('/x', body('tags').matches(/^[^\\]]+$/), getUsers);\n`)).toContain('references:getUsers');
    expect(refsOf(`router.get('/x', check('q').matches(/\\[/), getUsers);\n`)).toContain('references:getUsers');
    const inline = refsOf(`router.get('/x', async (req, res) => { const re = /[^]]/, out = compute(req); save(out); });\n`);
    expect(inline).toContain('calls:compute');
    expect(inline).toContain('calls:save');
  });

  it('keywords in a handler body are not calls', () => {
    const refs = refsOf(`router.get('/x', function (req, res) { if (ok(req)) { for (const a of b) run(a); } items.forEach(function (i) { shape(i); }); });\n`);
    expect(refs).toEqual(expect.arrayContaining(['calls:ok', 'calls:run', 'calls:shape']));
    expect(refs.filter((r) => /^calls:(if|for|function|while|switch|catch|return)$/.test(r))).toEqual([]);
  });

  it('a JSX closing tag is not a regex', () => {
    const refs = expressResolver.extract!('server.tsx', `app.get('/a', (req, res) => { res.send(renderToString(<div>{a()}</div>)); });\napp.get('/b', (req, res) => { res.send(renderPage(<>{b()}</>)); });\n`).references.map((r) => `${r.referenceKind}:${r.referenceName}`);
    expect(refs).toEqual(expect.arrayContaining(['calls:renderToString', 'calls:a', 'calls:renderPage', 'calls:b']));
  });

  it('a self-closing JSX tag after an attribute, and division after `}`, are not regexes', () => {
    const tsx = (src: string) =>
      expressResolver.extract!('server.tsx', src).references.map((r) => `${r.referenceKind}:${r.referenceName}`);
    expect(tsx(`app.get('*', (req, res) => {\n  const html = renderToString(<App store={store} />);\n  sendPage(res, html);\n});\n`)).toEqual(
      expect.arrayContaining(['calls:renderToString', 'calls:sendPage'])
    );
    expect(refsOf(`router.get('/x', (req, res) => { const v = { a: 1 }\n/ 2; svc(v); });\n`)).toContain('calls:svc');
  });

  it('division after `!` or `++` is not a regex', () => {
    expect(refsOf(`router.get('/x', rateLimit({ max: cfg.max! / 2 }), getUsers);\n`)).toContain('references:getUsers');
    expect(refsOf(`router.get('/x', limiter(hits++ / 2), getUsers);\n`)).toContain('references:getUsers');
  });

  it('an `async function` handler whose body has a `}` regex and an arrow keeps every call', () => {
    const refs = refsOf(`router.route('/x').get(async function (req, res) { const s = t.replace(/}/g, ''); load(); items.map((i) => shape(i)); });\n`);
    expect(refs).toContain('calls:load');
    expect(refs).toContain('calls:shape');
  });

  it('`async` and a nested `function` declaration name are not calls; a regex with `)` is skipped', () => {
    const refs = refsOf(`router.get('/x', function (req, res) { function helper(a) { return a; } Promise.all(items.map(async (i) => save(i))); });\n`);
    expect(refs).toContain('calls:save');
    expect(refs).not.toContain('calls:async');
    expect(refs).not.toContain('calls:helper');
  });

  it('regex literals in handlers do not cost the route its edges', () => {
    expect(refsOf(`router.get('/u/:id', async (req, res) => { if (!/^\\d+/.test(req.params.id)) return res.sendStatus(400); const user = await userService.findById(req.params.id); res.json(user); });\n`)).toContain('calls:userService.findById');
    const chained = expressResolver.extract!('routes.ts', `router.route('/:id').get((req, res) => { if (!/^\\w+/.test(req.params.id)) return bad(res); load(req); }).put(protect, updateThing);\n`);
    expect(chained.nodes.map((n) => n.name)).toEqual(['GET /:id', 'PUT /:id']);
    expect(chained.references.map((r) => `${r.referenceKind}:${r.referenceName}`)).toEqual(expect.arrayContaining(['calls:load', 'references:updateThing']));
    expect(refsOf(`app.get('/echo', (req, res) => { res.send(String(req.query.q).replace(/</g, '&lt;').replace(/>/g, '&gt;')); audit(req); });\n`)).toContain('calls:audit');
  });

  it('a call inside a template interpolation in the body still counts', () => {
    expect(refsOf('router.get(\'/x\', (req, res) => { res.send(`<p>${fmtUser(req.user)}</p>`); });\n')).toContain('calls:fmtUser');
  });

  it('a division that looks like a regex opener does not cost the route', () => {
    expect(refsOf(`router.get('/stats', async (req, res) => { const pct = Math.round((a - b) / total * 100) / 100; res.json({ pct: summarize(pct) }); });\n`)).toContain('calls:summarize');
    const chained = expressResolver.extract!('routes.ts', `router.route('/s').get((req, res) => { const m = Math.floor((t1 - t0) / 1000) / 60; res.json(fmt(m)); }).post(protect, createS);\n`);
    expect(chained.nodes.map((n) => n.name)).toEqual(['GET /s', 'POST /s']);
    expect(chained.references.map((r) => `${r.referenceKind}:${r.referenceName}`)).toEqual(expect.arrayContaining(['calls:fmt', 'references:createS']));
    const tsx = expressResolver.extract!('server.tsx', `app.get('/list', (req, res) => { res.send(renderToString(<ul>{items.map((x) => (<li key={x}>{x}</li>))}</ul>)); audit(req); });\n`).references.map((r) => `${r.referenceKind}:${r.referenceName}`);
    expect(tsx).toEqual(expect.arrayContaining(['calls:renderToString', 'calls:audit']));
  });

  it('a division after `)` with a `/` in a later string does not cost the route', () => {
    const refs = refsOf(`router.get('/s', (req, res) => { res.json({ pct: Math.round(s.done) / s.total, next: '/stats/2' }); audit(req); });\n`);
    expect(refs).toEqual(expect.arrayContaining(['calls:Math.round', 'calls:audit']));
    expect(refsOf(`router.get('/p', (req, res) => { const n = Math.ceil(count(req)) / size; res.redirect('/p/' + n); after(req); });\n`)).toContain('calls:after');
    expect(refsOf(`router.get('/c', cfg(f(a) / 2, 'x/y'), handler);\n`)).toContain('references:handler');
  });

  it('a callback inside middleware, or a middleware that calls `next()`, is not the handler', () => {
    expect(refsOf(`router.get('/admin', authorize(function (user, req) { return user.isAdmin; }), adminCtrl.index);\n`)).toContain('references:index');
    expect(refsOf(`router.get('/referrals', function (req, res, next) { check(); next(); }, getReferrals);\n`)).toContain('references:getReferrals');
    expect(refsOf(`router.get('/cfg', (req, res, next) => { load(); next(); }, overrideConfig);\n`)).toContain('references:overrideConfig');
  });

  it('the last argument decides: middleware before a named handler, guards, factories, generics', () => {
    expect(refsOf(`router.get('/admin', authorize(function (user, req) { return user.isAdmin; }), adminCtrl.index);\n`)).toContain('references:index');
    expect(refsOf(`router.get('/u/:id', function (req, res, next) { User.findById(id, function (err, user) { req.user = user; next(err); }); }, userCtrl.show);\n`)).toContain('references:show');
    expect(refsOf(`router.get('/x', responseTime(function (req, res, time) { stats.timing(req.path, time); }), ctrl.list);\n`)).toContain('references:list');
    expect(refsOf(`router.get('/errors', function (req, res, next) { auth(req); next(); }, getErrors);\n`)).toContain('references:getErrors');
    expect(refsOf(`app.use('/api', function (req, res, next) { if (!ok) return res.sendStatus(403); authorize(req, next); }, apiRouter);\n`)).toContain('references:apiRouter');
    expect(refsOf(`router.post('/g', auth, asyncHandler<Req, Res>(async (req, res) => { doIt(); }));\n`)).toContain('calls:doIt');
    expect(refsOf(`router.post('/d', asyncHandler<Req>(async (req, res) => { saveFile(req.file); }));\n`)).toContain('calls:saveFile');
  });

  it('JSX apostrophes, a trailing options object, a callback before a named handler', () => {
    const tsx = expressResolver.extract!('server.tsx', `app.get('/x', (req, res) => {\n  res.send(render(<p>Don't</p>));\n  res.send(render(<p>won't</p>));\n  load();\n});\n`).references.map((r) => `${r.referenceKind}:${r.referenceName}`);
    expect(tsx).toContain('calls:load');
    expect(refsOf(`router.get('/o', (req, res) => { svc.list(); res.json(1) }, { cache: true });\n`)).toContain('calls:svc.list');
    expect(refsOf(`router.get('/w', withErrors(function onErr(e) { report(e); }, ctrl.list));\n`)).not.toContain('calls:report');
    // An array of handlers: its last element is the handler.
    expect(refsOf(`router.post('/x', auth, [validate, ctrl.create]);\n`)).toEqual(['references:create']);
  });

  it('a regex that ends its line without a semicolon is still a regex', () => {
    expect(refsOf(`router.get('/re', (req, res) => {\n  const re = /[}]/\n  run(re)\n})\n`)).toContain('calls:run');
  });

  it('a backtick in an earlier regex does not cost later routes', () => {
    const refs = refsOf("const esc = (s) => s.replace(/`/g, 'x');\nrouter.get('/d', async (req, res) => { await svc.doThing(); res.end(); });\nrouter.get('/e', ctrl.list);\n");
    expect(refs).toEqual(expect.arrayContaining(['calls:svc.doThing', 'references:list']));
  });

  it('a division after a non-null `!` is not a regex, even with a path string later on the line', () => {
    expect(refsOf(`app.get('/x', (req, res) => {\n const r = done! / total!; res.redirect(r ? '/:id' : '/');\n svc.run(r);\n});\n`)).toContain('calls:svc.run');
    expect(refsOf(`app.get('/y', (req, res) => {\n const r = sum! / count!; const parts = csv.split(/,/);\n svc.run(parts);\n});\n`)).toContain('calls:svc.run');
  });

  it('a method named like a keyword is still a call', () => {
    const refs = refsOf(`router.delete('/users/:id', async (req, res) => { await userService.delete(req.params.id); cache .with(k); });\n`);
    expect(refs).toContain('calls:userService.delete');
    expect(refs).toContain('calls:cache.with');
    expect(refsOf(`router.get('/x', (req, res) => { return of(load()); });\n`)).toContain('calls:of');
  });

  it('chained routes: the handler decides too', () => {
    const refs = refsOf(
      `router.route('/items').get(rateLimit({ keyGenerator: (r) => lookup(r.ip) }), getItem).post(async (req, res) => { createItem(); });\n`
    );
    expect(refs).toContain('references:getItem');
    expect(refs).not.toContain('calls:lookup');
    expect(refs).toContain('calls:createItem');
  });
});
