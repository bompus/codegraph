/**
 * A PHP call through a receiver nothing typed takes the one project method of
 * that name only when the receiver is named after its class — or after a
 * class that inherits it (`$page->save()` → Page extends Entity). Guzzle's
 * tests' PSR-7 `$response->getHeaderLine()` went to a test double's method,
 * symfony/console's `$e->getMessage()` to a progress bar's.
 *
 * This fork is stricter: a PHP member call needs receiver evidence (a declared
 * or inferred type, an `instanceof` branch, an import), and a parameter or
 * local nothing types gets no method-name guess at all (the Phase 2b evidence
 * gate, docs/design/resolution-binding-model-plan.md). None of the three calls
 * below resolves by name, so the wrong `getMessage` edge stays absent and the
 * two receiver-named guesses upstream keeps are not made either.
 *
 * The test records that decision rather than guarding the port: it passes on
 * the kernel before upstream #2152 was ported too, because the evidence gate
 * already refuses these calls and the ported PHP arm of strategy 3 is dormant.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-php-receiver-'));
  const files: Record<string, string> = {
    'src/Models/Entity.php': `<?php
namespace App\\Models;

abstract class Entity
{
    public function save(): bool { return true; }
}
`,
    'src/Models/Page.php': `<?php
namespace App\\Models;

class Page extends Entity
{
}
`,
    'src/Helper/ProgressBar.php': `<?php
namespace App\\Helper;

class ProgressBar
{
    public function getMessage(): string { return ''; }
}
`,
    'src/Uploads/UserAvatars.php': `<?php
namespace App\\Uploads;

class UserAvatars
{
    public function assignToUser($user): void {}
}
`,
    'src/Controller.php': `<?php
namespace App;

function handle($page, $avatars, $user)
{
    try {
        $page->save();
        $avatars->assignToUser($user);
    } catch (\\Exception $e) {
        return $e->getMessage();
    }
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

describe('PHP calls through an untyped receiver', () => {
  it('never take a method by name alone', () => {
    const ids = cg.getNodesInFile('src/Controller.php').map((n) => n.id);
    const targets = cg
      .getOutgoingEdgesFrom(ids)
      .filter((e) => e.kind === 'calls')
      .map((e) => cg.getNode(e.target)!.qualifiedName)
      .sort();
    expect(targets).not.toContain('App\\Helper::ProgressBar::getMessage');
    expect(targets).toEqual([]);
  });
});
