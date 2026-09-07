import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { tanstackRouterResolver } from '../src/resolution/frameworks/tanstack-router';
import { routeRoots } from '../src/ui-server/api/route-roots';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript', 'javascript', 'tsx', 'jsx']);
});
const wrap = (
  options: string,
  factory = 'createFileRoute',
) => `import {createFileRoute${factory === 'createFileRoute' ? '' : ' as ' + factory}} from '@tanstack/react-router';
export const Route = ${factory}('/api/$id')(${options});`;
const extract = (source: string) =>
  tanstackRouterResolver.extract!('src/routes/api.$id.tsx', source);

describe('TanStack Start server declarations', () => {
  it('emits method-qualified routes without a phantom page', () => {
    const result = extract(wrap('{server:{handlers:{GET:getItem,POST:saveItem,ANY:fallback}}}'));
    expect(result.nodes.map((n) => n.name)).toEqual([
      'GET /api/:id',
      'POST /api/:id',
      'ANY /api/:id',
    ]);
    expect(result.references.map((r) => [r.referenceName, r.referenceKind])).toEqual([
      ['getItem', 'references'],
      ['saveItem', 'references'],
      ['fallback', 'references'],
    ]);
  });
  it('retains a mixed page/API route and imported factory alias', () => {
    const result = extract(wrap('{component:Page,server:{handlers:{GET:load}}}', 'fileRoute'));
    expect(result.nodes.map((n) => n.name)).toEqual(['GET /api/:id', '/api/:id']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['load', 'Page']);
  });
  it('reads the official createHandlers form and excludes middleware', () => {
    // TanStack/router@a58e01c604e2d189ef8c8c1ad6ac8747e03aa88c,
    // docs/start/framework/react/guide/server-routes.md:172–186.
    const result = extract(
      wrap(`{server:{handlers:({createHandlers})=>createHandlers({
GET:{middleware:[loggerMiddleware],handler:({request})=>respond(request)},POST:save})}}`),
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['GET /api/:id', 'POST /api/:id']);
    expect(result.references.map((r) => [r.referenceName, r.referenceKind])).toEqual([
      ['respond', 'calls'],
      ['save', 'references'],
    ]);
  });
  it('supports destructured helper aliases, block returns, and method shorthand', () => {
    const result = extract(
      wrap(
        `{server:{handlers:({createHandlers: make})=>{return make({GET(){return respond()},POST:{handler:save}})}}}`,
      ),
    );
    expect(result.nodes.map((n) => n.name)).toEqual(['GET /api/:id', 'POST /api/:id']);
    expect(result.references.map((r) => r.referenceName)).toEqual(['respond', 'save']);
  });
  it.each([
    '{server:{handlers:dynamic}}',
    '{server}',
    '{server:{handlers:{GET:load,...extra}}}',
    '{server:{handlers:{[verb]:load}}}',
    '{server:{handlers:{GET:{...options,handler:load}}}}',
    '{server:{handlers:({createHandlers})=>{const createHandlers=other;return createHandlers({GET:load})}}}',
    '{server:{handlers:()=>createHandlers({GET:load})}}',
    '{server:{handlers:{GET:object.handler}}}',
    '{server:{handlers:{GET:load}},...options}',
  ])('does not fabricate an endpoint or page from unsupported options %s', (options) => {
    expect(extract(wrap(options)).nodes).toEqual([]);
  });
  it('ignores unregistered, shadowed, type-only and unrelated factories', () => {
    for (const source of [
      `import {createFileRoute} from 'other';export const Route=createFileRoute('/x')({server:{handlers:{GET:load}}});`,
      `import {type createFileRoute} from '@tanstack/react-router';export const Route=createFileRoute('/x')({server:{handlers:{GET:load}}});`,
      `import type {createFileRoute} from '@tanstack/react-router';export const Route=createFileRoute('/x')({server:{handlers:{GET:load}}});`,
      `import {createFileRoute} from '@tanstack/react-router';function fn(createFileRoute){const Route=createFileRoute('/x')({server:{handlers:{GET:load}}});}`,
      `import {createFileRoute} from '@tanstack/react-router';const unused=createFileRoute('/x')({server:{handlers:{GET:load}}});`,
    ])
      expect(extract(source).nodes).toEqual([]);
  });
  it.each(['undefined', 'null', 'false'])(
    'does not turn component: %s into a page',
    (component) => {
      expect(
        extract(wrap(`{component:${component},server:{handlers:{GET:load}}}`)).nodes.map(
          (n) => n.name,
        ),
      ).toEqual(['GET /api/:id']);
    },
  );
  it('does not turn RPC functions into public endpoints or callback locals into call targets', () => {
    expect(
      extract(
        `import {createServerFn} from '@tanstack/react-start';export const rpc=createServerFn({method:'POST'}).handler(load);`,
      ).nodes,
    ).toEqual([]);
    expect(
      extract(
        wrap('{server:{handlers:{GET:(load)=>load(),POST:()=>{const save=other;return save()}}}}'),
      ).references,
    ).toEqual([]);
  });
  it('normalizes pathless/group routes but still finds their server methods', () => {
    expect(
      extract(
        wrap('{server:{handlers:{GET:load}}}').replace('/api/$id', '/(_group)/_auth/items_/$id'),
      ).nodes.map((n) => n.name),
    ).toEqual(['GET /items/:id']);
  });
});

describe('Start routes through indexing and sync', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  it('binds imported handlers and inline calls, preserves page roots, and tracks edits/deletion', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-start-'));
    write(
      'package.json',
      JSON.stringify({
        dependencies: { '@tanstack/react-start': '*', '@tanstack/react-router': '*' },
      }),
    );
    write(
      'src/handlers.ts',
      'export function load(){return 1;} export function respond(){return 2;}',
    );
    write(
      'src/routes/api.$id.tsx',
      `import {load,respond} from '../handlers';
${wrap('{component:Page,server:{middleware:[ignored],handlers:({createHandlers})=>createHandlers({GET:load,POST:{handler:()=>respond()}})}}')}
function Page(){return <div/>;}`,
    );
    write(
      'src/nav.tsx',
      `import {redirect} from '@tanstack/react-router';export function go(){return redirect({to:'/api/$id'});}`,
    );
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual(['/api/:id', 'GET /api/:id', 'POST /api/:id']);
    const roots = routeRoots(cg, routes);
    expect(roots.get(routes.find((n) => n.name === 'GET /api/:id')!.id)?.node.name).toBe('load');
    expect(roots.get(routes.find((n) => n.name === '/api/:id')!.id)?.node.name).toBe('Page');
    const go = cg.getNodesByKind('function').find((n) => n.name === 'go')!;
    expect(cg.getOutgoingEdges(go.id)).toContainEqual(
      expect.objectContaining({
        kind: 'navigates',
        target: routes.find((n) => n.name === '/api/:id')!.id,
      }),
    );
    const post = routes.find((n) => n.name === 'POST /api/:id')!;
    const respond = cg.getNodesByKind('function').find((n) => n.name === 'respond')!;
    expect(cg.getOutgoingEdges(post.id)).toContainEqual(
      expect.objectContaining({ kind: 'calls', target: respond.id }),
    );
    write(
      'src/routes/api.$id.tsx',
      `import {load} from '../handlers';\n${wrap('{server:{handlers:{DELETE:load}}}')}`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['DELETE /api/:id']);
    fs.unlinkSync(path.join(dir, 'src/routes/api.$id.tsx'));
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.each([false, true])('discovers Start after initial indexing (scoped=%s)', async (scoped) => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-start-new-'));
    write('package.json', '{}');
    write('src/handler.ts', 'export function load(){return 1;}');
    cg = await CodeGraph.init(dir, { index: true });
    write(
      'package.json',
      JSON.stringify({
        dependencies: { '@tanstack/react-start': '*', '@tanstack/react-router': '*' },
      }),
    );
    write(
      'src/routes/api.$id.ts',
      `import {load} from '../handler';\n${wrap('{server:{handlers:{GET:load}}}')}`,
    );
    await cg.sync(scoped ? { paths: ['src/routes/api.$id.ts'] } : {});
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name)).toEqual(['GET /api/:id']);
    expect(routeRoots(cg, routes).get(routes[0].id)?.node.name).toBe('load');
  });
});
