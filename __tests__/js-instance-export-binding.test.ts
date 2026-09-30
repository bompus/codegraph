/**
 * A method call on an exported instance (`export default new Client()`,
 * `export const store = new Store()`) lands on the class the exporting module
 * binds to that name, never on any project class that shares it.
 *
 * - A class the module imports from a package (`import { Client } from 'pg'`)
 *   is not in the project: `api.query()` links nowhere, not to an unrelated
 *   `class Client { query() }` elsewhere.
 * - A class imported from a project file is that file's class, even when
 *   another file declares a class with the same name and method.
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

const FILES: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'app', dependencies: { pg: '^8.0.0' } }),
  'src/graphql/client.ts': `export class Client {
  query() { return 1; }
}
export class Store {
  notify() { return 1; }
}
`,
  'src/db.ts': `import { Client } from 'pg';
export default new Client();
`,
  'src/events.ts': `import { Store } from 'pg';
export const store = new Store();
`,
  'src/aaa/ApiClient.ts': `export class ApiClient {
  fetchUsers() { return []; }
}
export class Cache {
  get() { return 1; }
}
`,
  'src/v2/client.ts': `export class ApiClient {
  fetchUsers() { return []; }
}
export class Cache {
  get() { return 2; }
}
`,
  'src/api.ts': `import { ApiClient } from './v2/client';
export default new ApiClient();
`,
  'src/cache.ts': `import { Cache } from './v2/client';
export const cache = new Cache();
`,
  'src/main.ts': `import db from './db';
import { store } from './events';
import api from './api';
import { cache } from './cache';

export function main() {
  db.query();
  store.notify();
  api.fetchUsers();
  cache.get();
}
`,
};

describe('JS/TS: a method on an exported instance', () => {
  it('lands on the class the exporting module binds, or nowhere', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-js-instance-export-'));
    roots.push(root);
    for (const [rel, content] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
      fs.writeFileSync(path.join(root, rel), content);
    }
    const cg = await CodeGraph.init(root, { index: true });
    try {
      const from = cg.getNodesByName('main').find((n) => n.kind === 'function');
      expect(from).toBeDefined();
      const calls = cg
        .getOutgoingEdgesFrom([from!.id], ['calls'])
        .map((e) => cg.getNode(e.target))
        .map((n) => `${n?.filePath}:${n?.qualifiedName}`)
        .sort();
      // Imported from a package: not the project's same-named class.
      expect(calls).not.toContain('src/graphql/client.ts:Client::query');
      expect(calls).not.toContain('src/graphql/client.ts:Store::notify');
      // Imported from a project file: that file's class, not the one listed first.
      expect(calls).toContain('src/v2/client.ts:ApiClient::fetchUsers');
      expect(calls).not.toContain('src/aaa/ApiClient.ts:ApiClient::fetchUsers');
      expect(calls).toContain('src/v2/client.ts:Cache::get');
      expect(calls).not.toContain('src/aaa/ApiClient.ts:Cache::get');
    } finally {
      cg.close();
    }
  });
});
