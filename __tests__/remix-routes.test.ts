import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { initGrammars, loadGrammarsForLanguages } from '../src/extraction/grammars';
import {
  remixFileRoutePath,
  usesDefaultFlatRoutes,
  reactRouterFilesResolver,
} from '../src/resolution/frameworks/react-router';
import { routeRoots } from '../src/ui-server/api/route-roots';
import type { ResolutionContext } from '../src/resolution/types';

beforeAll(async () => {
  await initGrammars();
  await loadGrammarsForLanguages(['typescript', 'javascript', 'tsx', 'jsx']);
});
// React Router 7aea711dd1ae2bc5a076d13ff17291829690fa74,
// packages/react-router-fs-routes/flatRoutes.ts: filename parsing and default scan.
describe('default flat filenames', () => {
  it.each([
    ['_index.tsx', '/'],
    ['concerts.trending.tsx', '/concerts/trending'],
    ['concerts.$city.tsx', '/concerts/:city'],
    ['concerts._index.tsx', '/concerts'],
    ['_auth.login.tsx', '/login'],
    ['concerts_.mine.tsx', '/concerts/mine'],
    ['($lang).categories.tsx', '/:lang?/categories'],
    ['(lang).categories.tsx', '/lang?/categories'],
    ['files.$.tsx', '/files/*'],
    ['sitemap[.]xml.tsx', '/sitemap.xml'],
    ['hello[(]world[)].tsx', '/hello(world)'],
    ['weird-url.[_index].tsx', '/weird-url/_index'],
    ['concerts.$city/route.tsx', '/concerts/:city'],
    ['concerts.$city/card.tsx', null],
    ['concerts/$city.tsx', null],
    ['_auth.tsx', null],
    ['.hidden.tsx', null],
    ['about.md', null],
    ['broken[.tsx', null],
    ['parent._index.child.tsx', null],
  ])('%s -> %s', (file, url) => expect(remixFileRoutePath('app/routes/' + file)).toBe(url));
  it('does not apply root conventions to another app directory', () => {
    expect(remixFileRoutePath('packages/web/app/routes/_index.tsx')).toBeNull();
  });
});

const imports = `import {flatRoutes as files} from '@react-router/fs-routes';\n`;
describe('flatRoutes registration', () => {
  it.each([
    'files()',
    'await files()',
    'files(); const unrelated = 1',
    '[...(await files())]',
    '[route("extra","./extra.tsx"), ...files()] satisfies RouteConfig',
  ])('accepts %s', (expression) => {
    expect(usesDefaultFlatRoutes(imports + 'export default ' + expression + ';')).toBe(true);
  });
  it.each([
    '[]',
    'files(options)',
    'files().map(change)',
    '[...(await files())].map(change)',
    '[layout("./layout.tsx", [...files()])]',
    '[files()]',
    'dynamic',
  ])('rejects %s', (expression) => {
    expect(usesDefaultFlatRoutes(imports + 'export default ' + expression + ';')).toBe(false);
  });
  it('ignores type-only, commented, unrelated and unregistered calls', () => {
    expect(usesDefaultFlatRoutes(imports + 'const unused=files();\nexport default [];')).toBe(
      false,
    );
    expect(
      usesDefaultFlatRoutes(
        "import type {flatRoutes} from '@react-router/fs-routes';\nexport default flatRoutes();",
      ),
    ).toBe(false);
    expect(
      usesDefaultFlatRoutes("import {flatRoutes} from 'other';\nexport default flatRoutes();"),
    ).toBe(false);
    expect(usesDefaultFlatRoutes(imports + '// export default files();')).toBe(false);
    expect(
      usesDefaultFlatRoutes(imports + 'const text=`export default files()`;\nexport default [];'),
    ).toBe(false);
  });
  it('requires registration for React Router and rejects route config overrides', () => {
    const files = new Map<string, string>([
      [
        'package.json',
        JSON.stringify({ dependencies: { '@react-router/fs-routes': '*', 'react-router': '*' } }),
      ],
    ]);
    const context = { readFile: (f: string) => files.get(f) ?? null } as ResolutionContext;
    expect(reactRouterFilesResolver.detect(context)).toBe(false);
    files.set('app/routes.ts', imports + 'export default files();');
    expect(reactRouterFilesResolver.detect(context)).toBe(true);
    for (const config of [
      'export default {appDirectory:"src"}',
      'export default {"appDirectory":"src"}',
      'export default {...options}',
      'export default configuration',
    ]) {
      files.set('react-router.config.ts', config);
      expect(reactRouterFilesResolver.detect(context)).toBe(false);
    }
  });
});

describe('file-route apps through indexing', () => {
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
  it.each([false, true])(
    'refreshes existing pages when only configuration changes (scoped=%s)',
    async (scoped) => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-remix-config-'));
      write(
        'package.json',
        JSON.stringify({ dependencies: { 'react-router': '7', '@react-router/dev': '7' } }),
      );
      write('app/routes.ts', imports + 'export default [];');
      write(
        'app/routes/about.tsx',
        'function Unused(){return <Outlet/>;} export default function About(){return <div/>;}',
      );
      cg = await CodeGraph.init(dir, { index: true });
      expect(cg.getNodesByKind('route')).toHaveLength(0);
      write('app/routes.ts', imports + 'export default files();');
      const enabled = await cg.sync(scoped ? { paths: ['app/routes.ts'] } : undefined);
      expect(enabled.filesModified).toBe(2);
      expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/about']);
      write('app/routes.ts', imports + 'export default [];');
      await cg.sync(scoped ? { paths: ['app/routes.ts'] } : undefined);
      expect(cg.getNodesByKind('route')).toHaveLength(0);
      write('app/routes.ts', imports + 'export default files();');
      await cg.sync();
      cg.close();
      fs.unlinkSync(path.join(dir, 'app/routes.ts'));
      cg = await CodeGraph.open(dir);
      await cg.sync(scoped ? { paths: ['app/routes.ts'] } : undefined);
      expect(cg.getNodesByKind('route')).toHaveLength(0);
    },
  );
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'indexes file conventions in fresh compiled workers',
    () => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-remix-workers-'));
      write('package.json', JSON.stringify({ dependencies: { '@remix-run/react': '2' } }));
      write('app/routes/_index.tsx', 'export default function Home(){return <div/>;}');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});
(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});
const route=cg.getNodesByKind('route')[0];
console.log(JSON.stringify([route.name,cg.getOutgoingEdges(route.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
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
      expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', ['Home']]);
    },
  );
  it.each(['remix', 'framework'])(
    '%s binds pages and optional navigation, excluding resources/layouts/colocation',
    async (mode) => {
      dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-remix-'));
      write(
        'package.json',
        JSON.stringify({
          dependencies:
            mode === 'remix'
              ? { '@remix-run/react': '2', '@remix-run/dev': '2' }
              : { 'react-router': '7', '@react-router/dev': '7', '@react-router/fs-routes': '7' },
        }),
      );
      if (mode === 'framework') {
        write(
          'app/routes.ts',
          imports +
            `import {route} from '@react-router/dev/routes';\nexport default [...(await files()), route('extra','./extra.tsx')];`,
        );
        write('app/extra.tsx', 'export default function Extra(){return <div/>;}');
      }
      for (const file of [
        '_index',
        'concerts.$city',
        '_auth.login',
        '($lang).categories',
        '(lang).static',
        'folder/route',
      ])
        write(`app/routes/${file}.tsx`, 'export default function Page(){return <div/>;}');
      write('app/routes/_auth.tsx', 'export default function Layout(){return <Outlet/>;}');
      write('app/routes/concerts.tsx', 'export default function Layout(){return <Outlet/>;}');
      write('app/routes/health.ts', 'export function loader(){return new Response("ok");}');
      write('app/routes/folder/card.tsx', 'export default function Card(){return <div/>;}');
      write(
        'app/nav.ts',
        `import {redirect} from '${mode === 'remix' ? '@remix-run/react' : 'react-router'}';
export function city(){return redirect('/concerts/paris')}
export function lang(){return redirect('/en/categories')}
export function noLang(){return redirect('/categories')}
export function fixed(){return redirect('/lang/static')}
export function noFixed(){return redirect('/static')}`,
      );
      cg = await CodeGraph.init(dir, { index: true });
      const routes = cg.getNodesByKind('route');
      expect(routes.map((n) => n.name).sort()).toEqual(
        [
          '/',
          '/concerts/:city',
          '/login',
          '/:lang?/categories',
          '/lang?/static',
          '/folder',
          ...(mode === 'framework' ? ['/extra'] : []),
        ].sort(),
      );
      const roots = routeRoots(cg, routes);
      expect(roots.size).toBe(routes.length);
      for (const [fn, url] of [
        ['city', '/concerts/:city'],
        ['lang', '/:lang?/categories'],
        ['noLang', '/:lang?/categories'],
        ['fixed', '/lang?/static'],
        ['noFixed', '/lang?/static'],
      ]) {
        const from = cg.getNodesByKind('function').find((n) => n.name === fn)!;
        expect(cg.getOutgoingEdges(from.id)).toContainEqual(
          expect.objectContaining({
            kind: 'navigates',
            target: routes.find((n) => n.name === url)!.id,
          }),
        );
      }
      write('app/routes/new.tsx', 'export default function NewPage(){return <div/>;}');
      await cg.sync();
      expect(cg.getNodesByKind('route').some((n) => n.name === '/new')).toBe(true);
      fs.unlinkSync(path.join(dir, 'app/routes/new.tsx'));
      await cg.sync();
      expect(cg.getNodesByKind('route').some((n) => n.name === '/new')).toBe(false);
    },
  );
});
