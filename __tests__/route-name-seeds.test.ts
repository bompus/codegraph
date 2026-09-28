/**
 * A route the query spells out seeds its route node. Route names start with a
 * method or `/`, which the literal predicate rejects, so without this seed a
 * route reached explore only through FTS: on a Fastify monorepo the route
 * file for `GET /explorer/questions/:questionId` never got a source section.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';

let dir: string;
let cg: CodeGraph;

const seededNames = (query: string): string[] =>
  cg.findLiteralSeedIds(query).map((id) => {
    const n = cg.getNode(id);
    return `${n?.kind} ${n?.name}`;
  });

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-route-seeds-'));
  fs.mkdirSync(path.join(dir, 'src'));
  fs.writeFileSync(path.join(dir, 'package.json'), '{"name":"shop","dependencies":{"express":"^4.0.0"}}');
  fs.writeFileSync(
    path.join(dir, 'src', 'orders.ts'),
    [
      "import express from 'express';",
      'const router = express.Router();',
      "router.get('/users/:id/orders', (req, res) => res.json([]));",
      "router.post('/users/:id/orders', (req, res) => res.json({}));",
      'export default router;',
    ].join('\n'),
  );
  cg = CodeGraph.initSync(dir);
  await cg.indexAll();
}, 60_000);

afterAll(() => {
  cg?.destroy();
  if (dir && fs.existsSync(dir)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('route-name seeds', () => {
  it('a method and path in a question seed that route alone', () => {
    expect(seededNames('What does GET /users/:id/orders return?')).toEqual(['route GET /users/:id/orders']);
  });

  it('a bare path seeds the route under each method', () => {
    expect(seededNames('who serves /users/:id/orders').sort())
      .toEqual(['route GET /users/:id/orders', 'route POST /users/:id/orders']);
  });

  it('a path no route has seeds nothing', () => {
    expect(seededNames('GET /users/:id/invoices')).toEqual([]);
  });
});
