/**
 * A Vue Options API component's own method is reached as `this.m()` inside
 * that component. `this.showToast()` in a component that gets `showToast`
 * from a plugin or mixin outside the repository is not another component's
 * `showToast`, whichever name-matching arm looks at it.
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

describe('Vue Options API methods and name matching', () => {
  it("never link another component's method to a this-call", async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-vue-options-fuzzy-'));
    roots.push(root);
    const files: Record<string, string> = {
      'src/components/A.vue': `<template><button @click="save">Save</button></template>
<script>
export default {
  methods: {
    save() {
      this.showToast('saved')
    },
  },
}
</script>
`,
      'src/components/B.vue': `<template><div /></template>
<script>
export default {
  methods: {
    showToast(message) {
      return message
    },
  },
}
</script>
`,
    };
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
      fs.writeFileSync(path.join(root, rel), content);
    }
    const cg = await CodeGraph.init(root, { index: true });
    try {
      const showToast = cg.getNodesInFile('src/components/B.vue').find((n) => n.kind === 'method' && n.name === 'showToast');
      expect(showToast).toBeDefined();
      const fromA = cg
        .getIncomingEdges(showToast!.id)
        .filter((e) => e.kind === 'calls')
        .map((e) => cg.getNode(e.source)!.filePath);
      expect(fromA).not.toContain('src/components/A.vue');
    } finally {
      cg.close();
    }
  });
});
