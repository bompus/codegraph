/**
 * A PHP `$this->m()` whose class hierarchy reaches a class outside the
 * repository may still mean a repository trait that outside class uses
 * (upstream's Orchestra/Laravel case in php-this-hierarchy.test.ts). Nothing
 * in the repository binds that trait to the call, though, so such a match is
 * a guess: it never outranks a method the hierarchy does bind, and it never
 * reaches the trusted confidence range.
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

async function project(files: Record<string, string>): Promise<CodeGraph> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-php-unseen-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  return CodeGraph.init(root, { index: true });
}

const calls = (cg: CodeGraph, file: string, method: string) => {
  const from = cg.getNodesInFile(file).find((n) => n.kind === 'method' && n.name === method)!;
  return cg
    .getOutgoingEdges(from.id)
    .filter((e) => e.kind === 'calls')
    .map((e) => ({
      target: cg.getNode(e.target)!.qualifiedName,
      confidence: (e.metadata as { confidence?: number } | undefined)?.confidence ?? 1,
    }));
};

const files = {
  'tests/TestCase.php': `<?php
namespace Tests;
use Illuminate\\Foundation\\Testing\\TestCase as BaseTestCase;
abstract class TestCase extends BaseTestCase {
}
`,
  'app/Support/Cacheable.php': `<?php
namespace App\\Support;
trait Cacheable {
  public function get($key) { return null; }
}
`,
  'tests/Feature/UserTest.php': `<?php
namespace Tests\\Feature;
use Tests\\TestCase;
class UserTest extends TestCase {
  public function testHome() {
    $this->get('/home');
  }
}
`,
};

describe('PHP $this calls past an ancestor outside the repository', () => {
  it('do not link an unused repository trait at trusted confidence', async () => {
    const cg = await project(files);
    try {
      const edges = calls(cg, 'tests/Feature/UserTest.php', 'testHome').filter((e) => e.target === 'App\\Support::Cacheable::get');
      for (const e of edges) expect(e.confidence).toBeLessThan(0.8);
    } finally {
      cg.close();
    }
  });

  it('prefer a trait the hierarchy uses over one it does not', async () => {
    const cg = await project({
      ...files,
      'tests/Concerns/MakesHttpRequests.php': `<?php
namespace Tests\\Concerns;
trait MakesHttpRequests {
  public function get($uri) { return null; }
}
`,
      'tests/TestCase.php': `<?php
namespace Tests;
use Illuminate\\Foundation\\Testing\\TestCase as BaseTestCase;
use Tests\\Concerns\\MakesHttpRequests;
abstract class TestCase extends BaseTestCase {
  use MakesHttpRequests;
}
`,
    });
    try {
      const edges = calls(cg, 'tests/Feature/UserTest.php', 'testHome');
      expect(edges.map((e) => e.target)).toEqual(['Tests\\Concerns::MakesHttpRequests::get']);
      expect(edges[0]!.confidence).toBeGreaterThanOrEqual(0.8);
    } finally {
      cg.close();
    }
  });
});
