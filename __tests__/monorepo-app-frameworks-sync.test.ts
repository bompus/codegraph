/**
 * A scoped sync keeps each file-based router inside its own app, as the full
 * index does (monorepo-app-frameworks.test.ts): re-indexing one file of the
 * Next.js app must not hand it to the Expo app's router.
 */
import { describe, it, expect, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterAll(() => {
  for (const r of roots.splice(0)) fs.rmSync(r, { recursive: true, force: true });
});

describe('file-based routers in a monorepo, after a scoped sync', () => {
  it('a changed Next.js layout stays out of Expo Router', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-app-frameworks-sync-'));
    roots.push(root);
    const files: Record<string, string> = {
      'package.json': JSON.stringify({ name: 'mono', private: true, workspaces: ['apps/*'] }),
      'apps/mobile/package.json': JSON.stringify({ name: 'mobile', dependencies: { expo: '*', 'expo-router': '*', react: '*' } }),
      'apps/mobile/app/index.tsx': 'export default function Home() { return null; }\n',
      'apps/web/package.json': JSON.stringify({ name: 'web', dependencies: { next: '*', react: '*' } }),
      'apps/web/app/layout.tsx': 'export default function RootLayout({ children }: { children: unknown }) { return children; }\n',
      'apps/web/app/page.tsx': 'export default function Page() { return null; }\n',
    };
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
      fs.writeFileSync(path.join(root, rel), content);
    }
    const cg = await CodeGraph.init(root, { index: true });
    try {
      const webRoutes = () => cg.getNodesByKind('route').filter((n) => n.filePath.startsWith('apps/web/')).map((n) => n.name).sort();
      expect(webRoutes()).not.toContain('/layout');
      fs.writeFileSync(
        path.join(root, 'apps/web/app/layout.tsx'),
        'export default function RootLayout({ children }: { children: unknown }) { return [children]; }\n',
      );
      await cg.sync({ paths: ['apps/web/app/layout.tsx'] });
      expect(webRoutes()).not.toContain('/layout');
      expect(webRoutes()).toContain('/');
    } finally {
      cg.close();
    }
  });
});
