import fs from 'node:fs';
import { nestjsResolver } from '../../src/resolution/frameworks/nestjs';
import { canonical } from './canonical';
import type { Node } from '../../src/types';
import type { ResolutionContext } from '../../src/resolution/types';
const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const results = cases.map((test: any) => {
  const { files, manifests, refs, now } = test.context;
  let nodes: Node[] = test.context.nodes;
  Date.now = () => now;
  const extracts: Record<string, any> = {};
  for (const [filePath, content] of Object.entries(files)) {
    const out = nestjsResolver.extract!(filePath, content as string)!;
    extracts[filePath] = out;
    nodes = nodes.concat(out.nodes);
  }
  const ctx = {
    getAllFiles: () => Object.keys(files),
    readFile: (p: string) => files[p] ?? manifests[p] ?? null,
    fileExists: (p: string) => p in files || p in manifests,
    fileContains: (p: string, needle: string) => (files[p] ?? '').includes(needle),
    getNodesByName: (name: string) => nodes.filter((n) => n.name === name),
    getNodesInFile: (p: string) => nodes.filter((n) => n.filePath === p),
  } as ResolutionContext;
  const detect = nestjsResolver.detect(ctx);
  const resolve = refs.map((r: any) => nestjsResolver.resolve(r, ctx));
  const postExtract = nestjsResolver.postExtract!(ctx);
  for (const update of postExtract) {
    const at = nodes.findIndex((n) => n.id === update.id);
    if (at >= 0) nodes[at] = update;
  }
  return {name: test.name, extract: extracts, detect, resolve, postExtract, postAgain: nestjsResolver.postExtract!(ctx)};
});
process.stdout.write(canonical(results));
