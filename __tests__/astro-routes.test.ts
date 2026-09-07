import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import { astroResolver } from '../src/resolution/frameworks/astro';
import { routeRoots } from '../src/ui-server/api/route-roots';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['astro']);
});

// withastro/astro@9870f95601690d9d98799b6fa78a0bc76165ee06:
// packages/astro/test/fixtures/api-routes/src/pages/binary.dat.ts and
// packages/astro/src/core/routing/create-manifest.ts (page/endpoint extensions).
describe('Astro declared endpoint methods', () => {
  it('reads typed function values, named exports and imported aliases', () => {
    const result = astroResolver.extract!(
      'src/pages/binary.dat.ts',
      `import type { APIRoute } from 'astro';
import {list as read} from '../../server';
export const GET: APIRoute = async () => new Response('binary');
export async function POST(){return new Response('posted')}
export {read as HEAD};
export const ALL = read;
export const prerender = false;`,
    );
    expect(result.nodes.map((n) => n.name)).toEqual([
      'GET /binary.dat',
      'POST /binary.dat',
      'HEAD /binary.dat',
      'ANY /binary.dat',
    ]);
    expect(result.references.map((r) => r.referenceName)).toEqual(['GET', 'POST', 'read', 'read']);
  });
  it.each([
    'const GET = () => new Response();',
    'export default function GET(){return new Response()}',
    'export const GET = dynamic();',
    'export const GET = {handler: handle};',
    'export function getStaticPaths(){return []}',
    'const text = "export function GET(){}";',
    '// export function GET(){}',
    'export {GET} from "../../server";',
    'export interface POST {x: string}',
    'export type GET = () => Response;',
    'export type {GET};',
    'export {type GET};',
  ])('ignores unsupported or non-handler declarations: %s', (source) => {
    expect(astroResolver.extract!('src/pages/api.ts', source).nodes).toEqual([]);
  });
  it.each(['src/pages/api.mjs', 'src/pages/_hidden/api.ts', 'src/server/api.ts'])(
    'excludes %s',
    (file) => {
      expect(astroResolver.extract!(file, 'export function GET(){}').nodes).toEqual([]);
    },
  );
});

describe('Astro routes through indexing', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  it('binds exact components and endpoint handlers, navigation and sync', async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-astro-routes-'));
    write('package.json', JSON.stringify({ dependencies: { astro: '5' } }));
    write(
      'src/pages/index.astro',
      `---
function forward(){return Astro.redirect('/blog/first')}
function external(){return Astro.redirect('https://other.test/hidden')}
---
<a href="/about">About</a><a href={'/blog/first'}>Post</a>
<a data-href="/hidden">Data</a><a title='href="/hidden"'>Title</a>
<div title='<a href="/hidden">'>Attribute</div>
{"<a href='/hidden'>String</a>"}
<a href="https://other.test/hidden">External</a><a href="//other.test/hidden">Protocol</a>
<!-- <a href="/hidden">Comment</a> -->
<script>const fake = '<a href="/hidden">Fake</a>';</script>`,
    );
    write('src/pages/about.astro', '<h1>About</h1>');
    write('src/pages/blog/[slug].astro', '<h1>Post</h1>');
    write('src/pages/hidden.astro', '<h1>Hidden</h1>');
    write('src/pages/_layout.astro', '<slot/>');
    write('src/pages/_hidden/page.astro', '<h1>Excluded</h1>');
    write('src/components/index.astro', '<h1>Unrelated same name</h1>');
    write('src/server.ts', 'export function list(){return new Response("ok")}');
    write(
      'src/pages/api/items.ts',
      `import {list as getItems} from '../../server';
export const GET = getItems;
export function POST(){return getItems()}`,
    );
    write('src/pages/rss.xml.js', 'export function GET(){return new Response("rss")}');
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual(
      [
        '/',
        '/about',
        '/blog/:slug',
        '/hidden',
        'GET /api/items',
        'POST /api/items',
        'GET /rss.xml',
      ].sort(),
    );
    const roots = routeRoots(cg, routes);
    for (const route of routes.filter((n) => n.language === 'astro')) {
      const component = cg.getNodesByKind('component').find((n) => n.filePath === route.filePath)!;
      expect(roots.get(route.id)?.node.id).toBe(component.id);
    }
    const get = routes.find((n) => n.name === 'GET /api/items')!;
    const list = cg.getNodesByKind('function').find((n) => n.name === 'list')!;
    expect(cg.getOutgoingEdges(get.id)).toContainEqual(
      expect.objectContaining({ target: list.id, kind: 'references' }),
    );
    expect(routes.find((n) => n.name === 'GET /rss.xml')!.language).toBe('javascript');
    const home = cg
      .getNodesByKind('component')
      .find((n) => n.filePath === 'src/pages/index.astro')!;
    const destinations = cg
      .getOutgoingEdges(home.id)
      .filter((e) => e.kind === 'navigates')
      .map((e) => cg!.getNode(e.target)?.name)
      .sort();
    expect(destinations).toEqual(['/about', '/blog/:slug']);
    const external = cg.getNodesByKind('function').find((n) => n.name === 'external')!;
    expect(cg.getOutgoingEdges(external.id).filter((e) => e.kind === 'navigates')).toEqual([]);
    const forward = cg.getNodesByKind('function').find((n) => n.name === 'forward')!;
    expect(cg.getOutgoingEdges(forward.id)).toContainEqual(
      expect.objectContaining({
        kind: 'navigates',
        target: routes.find((n) => n.name === '/blog/:slug')!.id,
      }),
    );
    write('src/pages/new.astro', '<h1>New</h1>');
    await cg.sync();
    expect(cg.getNodesByKind('route').some((n) => n.name === '/new')).toBe(true);
    write(
      'src/pages/api/items.ts',
      `import {list} from '../../server'; export const DELETE = list;`,
    );
    await cg.sync({ paths: ['src/pages/api/items.ts'] });
    expect(
      cg
        .getNodesByKind('route')
        .filter((n) => n.filePath.endsWith('items.ts'))
        .map((n) => n.name),
    ).toEqual(['DELETE /api/items']);
    fs.unlinkSync(path.join(dir, 'src/pages/new.astro'));
    await cg.sync();
    expect(cg.getNodesByKind('route').some((n) => n.name === '/new')).toBe(false);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'binds endpoints and pages in fresh compiled workers',
    () => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-astro-workers-'));
      write('package.json', JSON.stringify({ dependencies: { astro: '5' } }));
      write('src/pages/index.astro', '<a href="/">Home</a>');
      write('src/pages/api.ts', 'export function GET(){return new Response("ok")}');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});
(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});
console.log(JSON.stringify(cg.getNodesByKind('route').map(r=>[r.name,cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]).sort()));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
      const output = execFileSync(process.execPath, ['-e', script], {
        encoding: 'utf8',
        timeout: 60000,
        env: {
          ...process.env,
          CODEGRAPH_PARALLEL_RESOLVE_MIN: '1',
          CODEGRAPH_RESOLVE_WORKERS: '2',
          CODEGRAPH_PARSE_WORKERS: '2',
        },
      });
      expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual([
        ['/', ['index']],
        ['GET /api', ['GET']],
      ]);
    },
  );
  it.each([false, true])('discovers newly introduced Astro routes (scoped=%s)', async (scoped) => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-astro-new-'));
    write('package.json', '{}');
    write('src/plain.ts', 'export function plain(){}');
    cg = await CodeGraph.init(dir, { index: true });
    write('src/pages/index.astro', '<h1>Home</h1>');
    await cg.sync(scoped ? { paths: ['src/pages/index.astro'] } : undefined);
    const route = cg.getNodesByKind('route')[0]!;
    expect(route.name).toBe('/');
    expect(routeRoots(cg, [route]).get(route.id)?.node.filePath).toBe('src/pages/index.astro');
  });
});
