/**
 * An Express handler written inline as `function (req, res) {…}` — the form
 * Express's own examples use — is the route's body just like an arrow is:
 * its calls are the route's, so Steps draws what `POST /login` does instead
 * of a lone box. The handler is the LAST argument; an inline middleware
 * before a named handler is not it, and a trailing comma changes nothing.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { expressResolver } from '../src/resolution/frameworks/express';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-express-fnexpr-'));
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'app', private: true, dependencies: { express: '^4.0.0' } }),
    'app.js': `const express = require('express');
const assert = require('node:assert');
const logger = require('morgan');
const app = express();
const router = express.Router();

function authenticate(name, pass, fn) {
  fn(null, { name });
}

function restrict(req, res, next) {
  next();
}

function loadUser(id) {
  return { id };
}

function listPosts() {
  return [];
}

function wrap(fn) {
  return fn;
}

function showPost(req, res) {
  res.send('post');
}

class Presenter {}
app.get('/construct', (req, res) => { new Presenter(); });
app.get('/shadowed-calls', (req, res) => { { const loadUser = () => 1; loadUser(); } res.send(loadUser(1)); });

app.get('/external-assert', function(req, res) { assert(true); logger('dev'); loadUser(1); });
app.get('/local-assert', function(req, res) { function assert(value) { return value; } assert(true); loadUser(1); });

app.get('/wrapped-default', wrap((req, res, next = function () { listPosts(); }) => { res.send(loadUser(req.params.id)); }));
app.get('/function-default', function (req, res, next = () => listPosts()) { res.send(loadUser(req.params.id)); });

app.get('/default-next', (req, res, next = function () { listPosts(); }) => { res.send(loadUser(req.params.id)); });
router.route('/default-chain').get((req, res, next = function () { listPosts(); }) => { res.send(loadUser(req.params.id)); });

app.post('/login', function (req, res, next) {
  authenticate(req.body.username, req.body.password, function (err, user) {
    res.redirect('/');
  });
});

app.get('/restricted', restrict, function named(req, res) {
  res.send(loadUser(req.params.id));
});

router.get(
  '/posts',
  wrap(async function (req, res) {
    res.json(listPosts());
  }),
);

app.get('/post/:id', (req, res, next) => next(), showPost);

router.route('/users/:id').get(function (req, res) {
  res.json(loadUser(req.params.id));
});
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

const fromRoute = (name: string, kind: string) => {
  const route = cg.getNodesByKind('route').find((r) => r.name === name);
  expect(route, name).toBeDefined();
  return cg.getOutgoingEdgesFrom([route!.id]).filter((e) => e.kind === kind).map((e) => cg.getNode(e.target)!.name);
};

describe('Express function-expression handlers', () => {
  it('retains constructor calls and both lexical targets of a repeated name', () => {
    expect(fromRoute('GET /construct', 'instantiates')).toContain('Presenter');
    const route = cg.getNodesByKind('route').find((node) => node.name === 'GET /shadowed-calls')!;
    const targets = cg.getOutgoingEdgesFrom([route.id]).filter((edge) => edge.kind === 'calls').map((edge) => edge.target);
    const declarations = cg.getNodesInFile('app.js').filter((node) => node.name === 'loadUser' && ['function', 'constant', 'variable'].includes(node.kind));
    expect(declarations).toHaveLength(2);
    for (const node of declarations) expect.soft(targets).toContain(node.id);
  });

  it('keeps JSX inside an inline TSX handler', () => {
    const extracted = expressResolver.extract!('app.tsx', `import express from 'express';
const app = express();
function jsxHelper() { return 1; }
app.get('/jsx', (req, res) => { jsxHelper(); res.send(<div />); });
`);
    expect(extracted.references.map((ref) => ref.referenceName)).toContain('jsxHelper');
  });

  it('refuses external require variables and preserves a local callable shadow', () => {
    expect.soft(fromRoute('GET /external-assert', 'calls')).not.toContain('assert');
    expect.soft(fromRoute('GET /external-assert', 'calls')).not.toContain('logger');
    expect.soft(fromRoute('GET /external-assert', 'calls')).toContain('loadUser');
    const localAssert = cg.getNodesInFile('app.js').find((node) => node.name === 'assert' && node.kind === 'function')!;
    expect(localAssert).toBeDefined();
    const route = cg.getNodesByKind('route').find((node) => node.name === 'GET /local-assert')!;
    const targets = cg.getOutgoingEdgesFrom([route.id]).filter((edge) => edge.kind === 'calls').map((edge) => edge.target);
    expect.soft(targets).toContain(localAssert.id);
    const requireAssert = cg.getNodesInFile('app.js').find((node) => node.name === 'assert' && ['constant', 'variable'].includes(node.kind))!;
    expect.soft(targets).not.toContain(requireAssert.id);
    expect.soft(fromRoute('GET /local-assert', 'calls')).toContain('loadUser');
  });

  it('uses the outer arrow body instead of a function in a default parameter', () => {
    for (const route of ['GET /default-next', 'GET /default-chain', 'GET /wrapped-default', 'GET /function-default']) {
      expect.soft(fromRoute(route, 'calls')).toContain('loadUser');
      expect.soft(fromRoute(route, 'calls')).not.toContain('listPosts');
    }
  });

  it('lend the route their calls', () => {
    expect(fromRoute('POST /login', 'calls')).toContain('authenticate');
    expect(fromRoute('GET /restricted', 'calls')).toContain('loadUser');
    expect(fromRoute('GET /users/:id', 'calls')).toContain('loadUser');
  });

  it('are found inside a wrapper call, past a trailing comma', () => {
    expect(fromRoute('GET /posts', 'calls')).toContain('listPosts');
  });

  it('leave a named last argument the handler, even after an inline middleware', () => {
    expect(fromRoute('GET /post/:id', 'references')).toContain('showPost');
    expect(fromRoute('GET /post/:id', 'calls')).toEqual([]);
  });
});
