/**
 * A controller with many routes is versioned in one pass over the file.
 *
 * Each route looked for its own decorator by re-counting the file's newlines
 * once per decorator, so a controller with a thousand routes cost seconds in
 * the post-extract step (about 0.8 s for 800 routes across two controllers).
 */
import { describe, it, expect } from 'vitest';
import { nestjsResolver } from '../src/resolution/frameworks/nestjs';
import type { ResolutionContext } from '../src/resolution/types';
import type { Node } from '../src/types';

const MAIN = `import { NestFactory } from '@nestjs/core';
import { VersioningType } from '@nestjs/common';
async function bootstrap() {
  const app = await NestFactory.create(AppModule);
  app.enableVersioning({ defaultVersion: '1', type: VersioningType.URI });
  app.setGlobalPrefix('api');
}
`;

function controller(routes: number): string {
  let src = `import { Controller, Get, Post, Version } from '@nestjs/common';\n@Controller('wide')\nexport class WideController {\n`;
  for (let i = 0; i < routes; i++) {
    src += `  @Version('2')\n  @${i % 2 ? 'Post' : 'Get'}('r${i}')\n  handler${i}() {}\n`;
  }
  return src + '}\n';
}

describe('NestJS: versioning a controller with many routes', () => {
  it('names every route in a large controller without a per-route rescan of the file', () => {
    const routes = 1500;
    const files = new Map([
      ['apps/api/src/main.ts', MAIN],
      ['apps/api/src/wide.controller.ts', controller(routes)],
    ]);
    const nodes = new Map<string, Node[]>();
    for (const [file, content] of files) nodes.set(file, nestjsResolver.extract!(file, content).nodes);
    const context = {
      getAllFiles: () => [...files.keys()],
      getNodesInFile: (file: string) => nodes.get(file) ?? [],
      getNodesByName: () => [],
      readFile: (file: string) => files.get(file) ?? null,
      fileContains: (file: string, needle: string) => files.get(file)?.includes(needle) ?? false,
    } as unknown as ResolutionContext;

    const started = performance.now();
    const updates = nestjsResolver.postExtract!(context);
    const elapsed = performance.now() - started;

    expect(updates).toHaveLength(routes);
    const names = updates.map((n) => n.name);
    expect(names).toContain('GET /api/v2/wide/r0');
    expect(names).toContain('POST /api/v2/wide/r1499');
    // The pass takes a few milliseconds; the per-route rescan took seconds at this size.
    expect(elapsed).toBeLessThan(1000);
  });
});
