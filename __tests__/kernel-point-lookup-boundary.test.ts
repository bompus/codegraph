/**
 * The kernel finds the syntax node at a reference or declaration's start
 * position. Where the previous token ends exactly there (`x=1;local function`,
 * `instanceof Foo)$x->bar()`), the node at that position is the token that
 * starts there, not the `;` or `)` before it. Finding the earlier token made a
 * Lua `local` function read as global and hid a PHP `instanceof` guard.
 * The spaced forms are the controls: they resolved the same way before.
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

async function callTargets(files: Record<string, string>, from: string): Promise<string[]> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-point-lookup-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), content);
  const cg = await CodeGraph.init(root, { index: true });
  try {
    return cg
      .getOutgoingEdgesFrom(cg.getNodesInFile(from).map((n) => n.id), ['calls'])
      .map((e) => cg.getNode(e.target)!.qualifiedName)
      .sort();
  } finally {
    cg.close();
  }
}

describe('point lookup at a token boundary', () => {
  it('reads a Lua local function written right after `;` as local', async () => {
    const files = {
      'a.lua': 'x=1;local function helper() return 1 end\nlocal y = 2; local function spaced() return 2 end\nreturn x\n',
      'b.lua': 'local function main()\n  helper()\n  spaced()\nend\nreturn main\n',
    };
    expect(await callTargets(files, 'b.lua')).toEqual([]);
  });

  it('applies a PHP instanceof guard to a call written right after `)`', async () => {
    const files = {
      'a.php': `<?php
class Foo { public function bar() { return 1; } }
class Qux { public function bar() { return 2; } }
function tight($x) { if ($x instanceof Foo)$x->bar(); }
function spaced($x) { if ($x instanceof Foo) $x->bar(); }
`,
    };
    expect(await callTargets(files, 'a.php')).toEqual(['Foo::bar', 'Foo::bar']);
  });
});
