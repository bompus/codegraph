import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CodeGraph } from '../src';
import { routeRoots } from '../src/ui-server/api/route-roots';

describe('Analog default page routes', () => {
  let cg: CodeGraph | undefined;
  let dir: string;
  const write = (file: string, source: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), source);
  };
  const page = (file: string, name: string) =>
    write(
      'src/app/pages/' + file + '.page.ts',
      `import {Component} from '@angular/core'; @Component({template:'page'}) export default class ${name} {}`,
    );
  const setup = () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-analog-'));
    write(
      'package.json',
      JSON.stringify({
        dependencies: { '@analogjs/router': '2.7.1', '@analogjs/platform': '2.7.1' },
      }),
    );
    write(
      'vite.config.ts',
      `import {defineConfig} from 'vite'; import analog from '@analogjs/platform'; export default defineConfig({plugins:[analog()]});`,
    );
    write(
      'src/app/app.config.ts',
      `import {provideFileRouter} from '@analogjs/router'; export const appConfig = {providers:[provideFileRouter()]};`,
    );
  };
  afterEach(() => {
    cg?.close();
    cg = undefined;
    if (dir) fs.rmSync(dir, { recursive: true, force: true });
  });
  // analogjs/analog@0896a7eaaa2acf26443ca184bc1dd9aa1a06f4d6,
  // apps/analog-app/src/app/pages/products.[productId].page.ts and (auth).page.ts.
  it('composes pinned filename semantics and links exact page classes', async () => {
    setup();
    for (const [file, name] of [
      ['(home)', 'Home'],
      ['products', 'Products'],
      ['products.[productId]', 'ProductDetailsComponent'],
      ['(auth)', 'AuthLayoutPageComponent'],
      ['(auth)/login', 'Login'],
      ['admin', 'AdminLayout'],
      ['admin/index', 'AdminIndex'],
      ['admin/users.[id]', 'User'],
      ['[...slug]', 'CatchAll'],
      ['_private', 'Private'],
    ])
      page(file!, name!);
    write(
      'src/app/pages/(auth)/sign-up.page.ts',
      "import { Component } from '@angular/core';\n\n@Component({\n  template: ` <h2>SignUp</h2> `,\n})\nexport default class SignupPageComponent {}\n",
    );
    write('src/other.ts', 'export class ProductDetailsComponent {}');
    cg = await CodeGraph.init(dir, { index: true });
    const routes = cg.getNodesByKind('route');
    expect(routes.map((n) => n.name).sort()).toEqual([
      '/',
      '/*',
      '/_private',
      '/admin',
      '/admin/users/:id',
      '/login',
      '/products',
      '/products/:productId',
      '/sign-up',
    ]);
    const root = routeRoots(cg, routes).get(
      routes.find((n) => n.name === '/products/:productId')!.id,
    )!.node;
    expect([root.name, root.filePath]).toEqual([
      'ProductDetailsComponent',
      'src/app/pages/products.[productId].page.ts',
    ]);
    expect(
      routeRoots(cg, routes).get(routes.find((n) => n.name === '/sign-up')!.id)!.node.name,
    ).toBe('SignupPageComponent');
  });
  it('refreshes layouts and registration changes after reopening', async () => {
    setup();
    page('parent', 'Parent');
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/parent']);
    cg.close();
    cg = await CodeGraph.open(dir);
    page('parent/child', 'Child');
    await cg.sync({ paths: ['src/app/pages/parent/child.page.ts'] });
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/parent/child']);
    fs.unlinkSync(path.join(dir, 'src/app/pages/parent/child.page.ts'));
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/parent']);
    write('src/app/app.config.ts', 'export const appConfig = {providers:[]};');
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
    write(
      'src/app/app.config.ts',
      `import {provideFileRouter as files} from '@analogjs/router'; export const providers=[files()];`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/parent']);
    write(
      'vite.config.ts',
      `import analog from '@analogjs/platform'; export default {root:'custom',plugins:[analog()]};`,
    );
    await cg.sync();
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.each([
    [
      'vite.config.ts',
      `import analog from '@analogjs/platform'; export default {plugins:[analog()],plugins:[]};`,
    ],
    [
      'src/app/app.config.ts',
      `import {provideFileRouter} from '@analogjs/router'; for (const provideFileRouter of [()=>0]) provideFileRouter();`,
    ],
    ['vite.config.ts', `import analog from 'other'; export default {plugins:[analog()]};`],
    [
      'vite.config.ts',
      `import analog from '@analogjs/platform'; export default {plugins:[analog({additionalPagesDirs:['custom']})]};`,
    ],
    [
      'vite.config.ts',
      `import analog from '@analogjs/platform'; export default makeConfig(analog());`,
    ],
    [
      'src/app/app.config.ts',
      `import {provideFileRouter} from 'other'; export const providers=[provideFileRouter()];`,
    ],
    [
      'src/app/app.config.ts',
      `import type {provideFileRouter} from '@analogjs/router'; export const providers=[provideFileRouter()];`,
    ],
    [
      'src/app/app.config.ts',
      `import {provideFileRouter} from '@analogjs/router'; function wrapper(provideFileRouter){return provideFileRouter()}`,
    ],
    [
      'src/app/app.config.ts',
      `import {provideFileRouter} from '@analogjs/router'; false && provideFileRouter();`,
    ],
    [
      'src/app/app.config.ts',
      `import {provideFileRouter,withExtraRoutes} from '@analogjs/router'; export const providers=[provideFileRouter(withExtraRoutes([{path:'other'}]))];`,
    ],
  ])('ignores unsupported activation: %s %s', async (file, source) => {
    setup();
    page('index', 'Home');
    write(file!, source!);
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it('excludes metadata, optional catchalls, nondefault and anonymous pages', async () => {
    setup();
    page('[[...slug]]', 'Optional');
    write(
      'src/app/pages/meta.page.ts',
      `export const routeMeta={redirectTo:'/'}; export default class Meta {}`,
    );
    write('src/app/pages/named.page.ts', `export class Named {}`);
    write('src/app/pages/anonymous.page.ts', `export default class {}`);
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route')).toEqual([]);
  });
  it.each([false, true])('discovers newly introduced Analog (scoped=%s)', async (scoped) => {
    setup();
    write('package.json', '{}');
    page('index', 'Home');
    cg = await CodeGraph.init(dir, { index: true });
    expect(cg.getNodesByKind('route')).toEqual([]);
    write('package.json', JSON.stringify({ dependencies: { '@analogjs/router': '2.0.0' } }));
    write(
      'src/app/app.config.ts',
      `import {provideFileRouter as routes} from '@analogjs/router'; export const providers=[routes()];`,
    );
    await cg.sync(scoped ? { paths: ['src/app/app.config.ts'] } : undefined);
    expect(cg.getNodesByKind('route').map((n) => n.name)).toEqual(['/']);
  });
  it.runIf(fs.existsSync(path.resolve('dist/index.js')))(
    'uses fresh compiled parse and store workers',
    () => {
      setup();
      page('index', 'Home');
      const script = `const {CodeGraph}=require(${JSON.stringify(path.resolve('dist/index.js'))});(async()=>{const cg=await CodeGraph.init(${JSON.stringify(dir)},{index:true});const r=cg.getNodesByKind('route')[0];console.log(JSON.stringify([r.name,cg.getOutgoingEdges(r.id).filter(e=>e.kind==='references').map(e=>cg.getNode(e.target)?.name)]));cg.close();})().catch(e=>{console.error(e);process.exit(1)});`;
      const output = execFileSync(process.execPath, ['-e', script], {
        encoding: 'utf8',
        timeout: 60000,
        env: {
          ...process.env,
          CODEGRAPH_PARSE_WORKERS: '2',
          CODEGRAPH_PARALLEL_RESOLVE_MIN: '1',
          CODEGRAPH_RESOLVE_WORKERS: '2',
        },
      });
      expect(JSON.parse(output.trim().split('\n').at(-1)!)).toEqual(['/', ['Home']]);
    },
  );
});
