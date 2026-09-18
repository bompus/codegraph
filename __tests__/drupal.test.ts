/**
 * Tests for Drupal framework resolver.
 *
 * Unit tests cover drupalResolver.detect(), extract() (routes + hooks), and resolve().
 * Integration tests use a real CodeGraph instance with a temporary Drupal project layout.
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { CodeGraph } from '../src';
import { initGrammars, loadAllGrammars } from '../src/extraction/grammars';
import { generateNodeId } from '../src/extraction/tree-sitter-helpers';
import { drupalResolver } from '../src/resolution/frameworks/drupal';
import type { ResolutionContext } from '../src/resolution/types';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeContext(
  overrides: Partial<ResolutionContext> = {},
): ResolutionContext {
  return {
    getNodesInFile: () => [],
    getNodesByName: () => [],
    getNodesByQualifiedName: () => [],
    getNodesByKind: () => [],
    fileExists: () => false,
    readFile: () => null,
    getProjectRoot: () => '/project',
    getAllFiles: () => [],
    getNodesByLowerName: () => [],
    getImportMappings: () => [],
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// detect()
// ---------------------------------------------------------------------------

describe('drupalResolver.detect', () => {
  it('returns true when composer.json has a drupal/ dependency', () => {
    const ctx = makeContext({
      readFile: (f) =>
        f === 'composer.json'
          ? JSON.stringify({
              require: {
                'drupal/core-recommended': '~10.5',
                'drush/drush': '^13',
              },
            })
          : null,
    });
    expect(drupalResolver.detect(ctx)).toBe(true);
  });

  it('returns true when drupal/ dependency is in require-dev', () => {
    const ctx = makeContext({
      readFile: (f) =>
        f === 'composer.json'
          ? JSON.stringify({ 'require-dev': { 'drupal/core': '^10' } })
          : null,
    });
    expect(drupalResolver.detect(ctx)).toBe(true);
  });

  it('returns false when composer.json has no drupal/ dependencies', () => {
    const ctx = makeContext({
      readFile: (f) =>
        f === 'composer.json'
          ? JSON.stringify({
              require: { 'laravel/framework': '^10', php: '>=8.1' },
            })
          : null,
    });
    expect(drupalResolver.detect(ctx)).toBe(false);
  });

  it('returns false when composer.json is absent', () => {
    const ctx = makeContext({ readFile: () => null });
    expect(drupalResolver.detect(ctx)).toBe(false);
  });

  it('returns false when composer.json is malformed JSON', () => {
    const ctx = makeContext({ readFile: () => '{ bad json' });
    expect(drupalResolver.detect(ctx)).toBe(false);
  });

  it('returns true for a contrib module with empty require (composer name/type)', () => {
    const ctx = makeContext({
      readFile: (f) =>
        f === 'composer.json'
          ? JSON.stringify({
              name: 'drupal/admin_toolbar',
              type: 'drupal-module',
              require: {},
            })
          : null,
    });
    expect(drupalResolver.detect(ctx)).toBe(true);
  });

  it('returns true via the *.info.yml fallback when composer.json is absent', () => {
    const ctx = makeContext({
      readFile: () => null,
      getAllFiles: () => [
        'mymodule/mymodule.info.yml',
        'mymodule/mymodule.routing.yml',
      ],
    });
    expect(drupalResolver.detect(ctx)).toBe(true);
  });

  it('returns false for a stray *.info.yml with no Drupal PHP/route file', () => {
    const ctx = makeContext({
      readFile: () => null,
      getAllFiles: () => ['some/unrelated.info.yml'],
    });
    expect(drupalResolver.detect(ctx)).toBe(false);
  });
});

describe('drupalResolver.claimsReference', () => {
  it('claims FQCN handler refs and hook names the pre-filter would drop', () => {
    expect(drupalResolver.claimsReference!('\\Drupal\\m\\Form\\SettingsForm')).toBe(true);
    expect(drupalResolver.claimsReference!('\\Drupal\\m\\Controller\\C:setNoJsCookie')).toBe(true);
    expect(drupalResolver.claimsReference!('hook_form_alter')).toBe(true);
  });

  it('does not claim ordinary identifiers or entity-handler dotted refs', () => {
    expect(drupalResolver.claimsReference!('someHelperFunction')).toBe(false);
    expect(drupalResolver.claimsReference!('comment.default')).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// extract() — routing.yml
// ---------------------------------------------------------------------------

describe('drupalResolver.extract — routing.yml', () => {
  const routing = `
mymodule.example:
  path: '/mymodule/example'
  defaults:
    _controller: '\\Drupal\\mymodule\\Controller\\MyController::build'
    _title: 'Example page'
  requirements:
    _permission: 'access content'
`;

  it('emits a route node for each YAML route', () => {
    const { nodes } = drupalResolver.extract!(
      'mymodule/mymodule.routing.yml',
      routing,
    );
    expect(nodes).toHaveLength(1);
    expect(nodes[0]!.kind).toBe('route');
    expect(nodes[0]!.name).toBe('/mymodule/example');
  });

  it('sets qualifiedName to filePath::routeName', () => {
    const { nodes } = drupalResolver.extract!(
      'mymodule/mymodule.routing.yml',
      routing,
    );
    expect(nodes[0]!.qualifiedName).toBe(
      'mymodule/mymodule.routing.yml::mymodule.example',
    );
  });

  it('emits a references edge to the controller FQCN', () => {
    const { references } = drupalResolver.extract!(
      'mymodule/mymodule.routing.yml',
      routing,
    );
    expect(references).toHaveLength(1);
    expect(references[0]!.referenceName).toBe(
      '\\Drupal\\mymodule\\Controller\\MyController::build',
    );
    expect(references[0]!.referenceKind).toBe('references');
  });

  it('emits a references edge to a _form handler', () => {
    const src = `
mymodule.settings_form:
  path: '/admin/config/mymodule'
  defaults:
    _form: '\\Drupal\\mymodule\\Form\\SettingsForm'
    _title: 'MyModule settings'
  requirements:
    _permission: 'administer site configuration'
`;
    const { nodes, references } = drupalResolver.extract!(
      'mymodule/mymodule.routing.yml',
      src,
    );
    expect(nodes).toHaveLength(1);
    expect(references[0]!.referenceName).toBe(
      '\\Drupal\\mymodule\\Form\\SettingsForm',
    );
  });

  it('handles multiple routes in one file', () => {
    const src = `
mod.page_one:
  path: '/page-one'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\PageController::one'
  requirements:
    _permission: 'access content'

mod.page_two:
  path: '/page-two'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\PageController::two'
  requirements:
    _permission: 'access content'
`;
    const { nodes, references } = drupalResolver.extract!(
      'mod/mod.routing.yml',
      src,
    );
    expect(nodes).toHaveLength(2);
    expect(nodes.map((n) => n.name)).toContain('/page-one');
    expect(nodes.map((n) => n.name)).toContain('/page-two');
    expect(references).toHaveLength(2);
  });

  it('skips commented-out lines', () => {
    const src = `
mod.page:
  path: '/page'
  defaults:
    #_controller: '\\Drupal\\mod\\Controller\\Old::build'
    _controller: '\\Drupal\\mod\\Controller\\New::build'
  requirements:
    _permission: 'access content'
`;
    const { references } = drupalResolver.extract!('mod/mod.routing.yml', src);
    expect(references).toHaveLength(1);
    expect(references[0]!.referenceName).toContain('New');
  });

  it('includes HTTP methods in the route node name when present', () => {
    const src = `
mod.api:
  path: '/api/resource'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\ApiController::get'
  methods: [GET, POST]
  requirements:
    _permission: 'access content'
`;
    const { nodes } = drupalResolver.extract!('mod/mod.routing.yml', src);
    expect(nodes[0]!.name).toContain('GET');
    expect(nodes[0]!.name).toContain('POST');
  });

  it('returns empty result for non-routing-yml files', () => {
    const { nodes, references } = drupalResolver.extract!(
      'mymodule.module',
      '<?php\n',
    );
    // Module files go through hook detection, not route extraction
    expect(nodes).toHaveLength(0);
  });

  it('returns empty result for files with no valid routes', () => {
    const { nodes, references } = drupalResolver.extract!(
      'some.routing.yml',
      '# empty\n',
    );
    expect(nodes).toHaveLength(0);
    expect(references).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// extract() — hook detection in .module files
// ---------------------------------------------------------------------------

describe('drupalResolver.extract — hook detection', () => {
  it('detects hook implementation via docblock (Strategy A)', () => {
    const src = `<?php

/**
 * Implements hook_form_alter().
 */
function mymodule_form_alter(&$form, $form_state, $form_id) {
  // ...
}
`;
    const { references } = drupalResolver.extract!(
      'web/modules/custom/mymodule/mymodule.module',
      src,
    );
    const hookRef = references.find(
      (r) => r.referenceName === 'hook_form_alter',
    );
    expect(hookRef).toBeDefined();
    expect(hookRef!.referenceKind).toBe('references');
  });

  it('detects hook implementation via name pattern (Strategy B)', () => {
    const src = `<?php

function mymodule_views_data() {
  return [];
}
`;
    const { references } = drupalResolver.extract!(
      'web/modules/custom/mymodule/mymodule.module',
      src,
    );
    const hookRef = references.find(
      (r) => r.referenceName === 'hook_views_data',
    );
    expect(hookRef).toBeDefined();
  });

  it('does not emit a hook ref for non-hook helper functions', () => {
    // 'other_module_helper' doesn't start with 'mymodule_', so no hook ref
    const src = `<?php
function other_module_helper() {}
`;
    const { references } = drupalResolver.extract!(
      'web/modules/custom/mymodule/mymodule.module',
      src,
    );
    expect(references).toHaveLength(0);
  });

  it('detects hooks in .install files', () => {
    const src = `<?php
/**
 * Implements hook_schema().
 */
function mymodule_schema() {
  return [];
}
`;
    const { references } = drupalResolver.extract!(
      'web/modules/custom/mymodule/mymodule.install',
      src,
    );
    const hookRef = references.find((r) => r.referenceName === 'hook_schema');
    expect(hookRef).toBeDefined();
  });

  it('detects hooks in .theme files', () => {
    const src = `<?php
/**
 * Implements hook_preprocess_node().
 */
function mytheme_preprocess_node(&$variables) {}
`;
    const { references } = drupalResolver.extract!(
      'web/themes/custom/mytheme/mytheme.theme',
      src,
    );
    const hookRef = references.find(
      (r) => r.referenceName === 'hook_preprocess_node',
    );
    expect(hookRef).toBeDefined();
  });

  it('does not duplicate refs when both docblock and name pattern match', () => {
    // Strategy A matches first and adds to docblockMatched set;
    // Strategy B skips already-matched functions.
    const src = `<?php
/**
 * Implements hook_form_alter().
 */
function mymodule_form_alter(&$form, $form_state, $form_id) {}
`;
    const { references } = drupalResolver.extract!(
      'web/modules/custom/mymodule/mymodule.module',
      src,
    );
    const hookRefs = references.filter(
      (r) => r.referenceName === 'hook_form_alter',
    );
    expect(hookRefs).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// extract() — Drupal 11 #[Hook] attribute detection
// ---------------------------------------------------------------------------

describe('drupalResolver.extract — #[Hook] attribute detection', () => {
  const HOOK_FILE = 'web/modules/custom/my_module/src/Hook/MyHooks.php';

  it('emits a hook ref from a method-level #[Hook] attribute', () => {
    const src = `<?php
namespace Drupal\\my_module\\Hook;

use Drupal\\Core\\Hook\\Attribute\\Hook;

class MyHooks {
  #[Hook('user_cancel')]
  public function userCancel(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_user_cancel');
    expect(ref).toBeDefined();
    expect(ref!.referenceKind).toBe('references');
    // The method node starts at its first `#[` line (line 7), not `function`.
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', 'userCancel', 7),
    );
  });

  it('pinpoints the method named by a class-level method: arg', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook('user_login', method: 'onLogin')]
class LoginHooks {
  public function onLogin(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_user_login');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', 'onLogin', 6),
    );
  });

  it('pinpoints __invoke for a class-level attribute with no method arg', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook('user_logout')]
class LogoutHooks {
  public function __invoke(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_user_logout');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', '__invoke', 6),
    );
  });

  it('falls back to the class node when no impl method is determinable', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook('theme')]
class ThemeHook {
  public function helper(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_theme');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'class', 'ThemeHook', 4),
    );
  });

  it('emits one ref per #[Hook] on repeatable attributes', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

class C {
  #[Hook('form_alter')]
  #[Hook('form_FORM_ID_alter')]
  public function alter(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const names = references.map((r) => r.referenceName);
    expect(names).toContain('hook_form_alter');
    expect(names).toContain('hook_form_FORM_ID_alter');
    expect(references).toHaveLength(2);
    // Both hang off the same method node — the block starts at the FIRST `#[`.
    const methodId = generateNodeId(HOOK_FILE, 'method', 'alter', 5);
    expect(references.every((r) => r.fromNodeId === methodId)).toBe(true);
  });

  it('finds Hook inside a comma-separated attribute group', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

class C {
  #[SomeOther, Hook('node_presave'), Another('x')]
  public function presave(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_node_presave');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', 'presave', 5),
    );
  });

  it('accepts the fully-qualified \\Drupal\\Core\\Hook\\Attribute\\Hook name', () => {
    const src = `<?php
class C {
  #[\\Drupal\\Core\\Hook\\Attribute\\Hook('user_login')]
  public function m(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_user_login');
    expect(ref).toBeDefined();
  });

  it('reads the hook name from the hook: named arg', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

class C {
  #[Hook(hook: 'user_presave')]
  public function named(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_user_presave');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', 'named', 5),
    );
  });

  it('handles multi-line argument lists', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook(
  'entity_insert',
)]
class InsertHooks {
  public function __invoke(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_entity_insert');
    expect(ref).toBeDefined();
    expect(ref!.fromNodeId).toBe(
      generateNodeId(HOOK_FILE, 'method', '__invoke', 8),
    );
  });

  it('does not match Hook-lookalike attributes', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\LegacyHook;
use Drupal\\Core\\Hook\\Attribute\\RemoveHook;
use Drupal\\Core\\Hook\\Attribute\\ReorderHook;

class C {
  #[LegacyHook('user_cancel')]
  public function a(): void {}

  #[RemoveHook('user_cancel')]
  public function b(): void {}

  #[ReorderHook('user_cancel')]
  public function c(): void {}

  #[\\Drupal\\hux\\Attribute\\Hook('user_cancel')]
  public function d(): void {}

  #[Drupal\\hux\\Attribute\\Hook('user_cancel')]
  public function e(): void {}

  public function plain(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    expect(references).toHaveLength(0);
  });

  it('does not treat bare Hook as core when hux imports the name', () => {
    const src = `<?php
use Drupal\\hux\\Attribute\\Hook;

class C {
  #[Hook('user_cancel')]
  public function a(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    expect(references).toHaveLength(0);
  });

  it('resolves bare Hook through an alias of the core import', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook as CoreHook;

class C {
  #[CoreHook('node_insert')]
  public function ins(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    const ref = references.find((r) => r.referenceName === 'hook_node_insert');
    expect(ref).toBeDefined();
  });

  it('ignores attribute-shaped text in comments and strings', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

// #[Hook('fake_one')]
/* #[Hook('fake_two')] */
$s = '#[Hook("fake_three")]';
$doc = <<<EOT
#[Hook('fake_four')]
EOT;

class C {
  #[Hook('real_hook')]
  public function a(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    expect(references.map((r) => r.referenceName)).toEqual(['hook_real_hook']);
  });

  it('ignores #[Hook] on promoted constructor params', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

class C {
  public function __construct(
    #[Hook('param_hook')]
    protected Foo $foo,
  ) {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    expect(references).toHaveLength(0);
  });

  it('skips non-literal hook names', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

class C {
  #[Hook(self::HOOK_NAME)]
  public function constArg(): void {}
}
`;
    const { references } = drupalResolver.extract!(HOOK_FILE, src);
    expect(references).toHaveLength(0);
  });

  it('emits a function-kind ref for an attributed procedural function, deduped', () => {
    const src = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook('cron')]
function mymodule_cron() {}
`;
    const file = 'web/modules/custom/mymodule/mymodule.module';
    const { references } = drupalResolver.extract!(file, src);
    const refs = references.filter((r) => r.referenceName === 'hook_cron');
    // Strategy B name-matching would also fire on `mymodule_cron` — the
    // attribute ref must win (and not duplicate) on the `#[`-line node id.
    expect(refs).toHaveLength(1);
    expect(refs[0]!.fromNodeId).toBe(
      generateNodeId(file, 'function', 'mymodule_cron', 4),
    );
  });

  it('keeps docblock matching when an attribute sits before the function', () => {
    const src = `<?php
/**
 * Implements hook_user_cancel().
 */
#[\\Drupal\\Core\\Hook\\Attribute\\LegacyHook('user_cancel')]
function mymodule_user_cancel() {}
`;
    const file = 'web/modules/custom/mymodule/mymodule.module';
    const { references } = drupalResolver.extract!(file, src);
    const refs = references.filter(
      (r) => r.referenceName === 'hook_user_cancel',
    );
    expect(refs).toHaveLength(1);
    // Node id uses the `#[` line — the kernel starts the decl there.
    expect(refs[0]!.fromNodeId).toBe(
      generateNodeId(file, 'function', 'mymodule_user_cancel', 5),
    );
  });
});

// ---------------------------------------------------------------------------
// resolve()
// ---------------------------------------------------------------------------

describe('drupalResolver.resolve', () => {
  it('resolves a _controller FQCN with ::method to the method node', () => {
    const methodNode = {
      id: 'method:abc123',
      kind: 'method' as const,
      name: 'build',
      qualifiedName: 'MyController::build',
      filePath: 'web/modules/custom/mymodule/src/Controller/MyController.php',
      language: 'php' as const,
      startLine: 10,
      endLine: 20,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const classNode = {
      id: 'class:def456',
      kind: 'class' as const,
      name: 'MyController',
      qualifiedName: 'MyController',
      filePath: 'web/modules/custom/mymodule/src/Controller/MyController.php',
      language: 'php' as const,
      startLine: 5,
      endLine: 30,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesByName: (name) => (name === 'MyController' ? [classNode] : []),
      getNodesInFile: () => [classNode, methodNode],
    });
    const ref = {
      fromNodeId: 'route:x',
      referenceName: '\\Drupal\\mymodule\\Controller\\MyController::build',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'mymodule.routing.yml',
      language: 'yaml' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved).not.toBeNull();
    expect(resolved!.targetNodeId).toBe('method:abc123');
    expect(resolved!.confidence).toBeGreaterThanOrEqual(0.85);
  });

  it('resolves a _form FQCN (no ::method) to the class node', () => {
    const classNode = {
      id: 'class:form123',
      kind: 'class' as const,
      name: 'SettingsForm',
      qualifiedName: 'SettingsForm',
      filePath: 'web/modules/custom/mymodule/src/Form/SettingsForm.php',
      language: 'php' as const,
      startLine: 1,
      endLine: 50,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesByName: (name) => (name === 'SettingsForm' ? [classNode] : []),
    });
    const ref = {
      fromNodeId: 'route:x',
      referenceName: '\\Drupal\\mymodule\\Form\\SettingsForm',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'mymodule.routing.yml',
      language: 'yaml' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved).not.toBeNull();
    expect(resolved!.targetNodeId).toBe('class:form123');
  });

  it('returns null when the target class cannot be found', () => {
    const ctx = makeContext({ getNodesByName: () => [] });
    const ref = {
      fromNodeId: 'route:x',
      referenceName: '\\Drupal\\mymodule\\Controller\\Missing::method',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'mymodule.routing.yml',
      language: 'yaml' as const,
    };
    expect(drupalResolver.resolve(ref, ctx)).toBeNull();
  });

  it('resolves a single-colon controller-service ref (Class:method)', () => {
    const methodNode = {
      id: 'method:nojs1',
      kind: 'method' as const,
      name: 'setNoJsCookie',
      qualifiedName: 'BigPipeController::setNoJsCookie',
      filePath: 'core/modules/big_pipe/src/Controller/BigPipeController.php',
      language: 'php' as const,
      startLine: 10,
      endLine: 20,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const classNode = {
      id: 'class:nojs2',
      kind: 'class' as const,
      name: 'BigPipeController',
      qualifiedName: 'BigPipeController',
      filePath: 'core/modules/big_pipe/src/Controller/BigPipeController.php',
      language: 'php' as const,
      startLine: 5,
      endLine: 30,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesByName: (name) => (name === 'BigPipeController' ? [classNode] : []),
      getNodesInFile: () => [classNode, methodNode],
    });
    const ref = {
      fromNodeId: 'route:x',
      referenceName: '\\Drupal\\big_pipe\\Controller\\BigPipeController:setNoJsCookie',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'big_pipe.routing.yml',
      language: 'yaml' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved).not.toBeNull();
    expect(resolved!.targetNodeId).toBe('method:nojs1');
  });

  it('resolves hook_X to a #[Hook]-attributed method under src/Hook/', () => {
    const hookFile = 'web/modules/custom/my_module/src/Hook/MyHooks.php';
    const hookSrc = `<?php
namespace Drupal\\my_module\\Hook;

use Drupal\\Core\\Hook\\Attribute\\Hook;

class MyHooks {
  #[Hook('user_cancel')]
  public function userCancel(): void {}
}
`;
    const methodNode = {
      id: 'method:hook1',
      kind: 'method' as const,
      name: 'userCancel',
      qualifiedName: 'MyHooks::userCancel',
      filePath: hookFile,
      language: 'php' as const,
      startLine: 7,
      endLine: 9,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const classNode = {
      id: 'class:hook2',
      kind: 'class' as const,
      name: 'MyHooks',
      qualifiedName: 'MyHooks',
      filePath: hookFile,
      language: 'php' as const,
      startLine: 6,
      endLine: 10,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesInFile: (f) => (f === hookFile ? [classNode, methodNode] : []),
      getNodesByKind: () => [], // no procedural *_user_cancel function
      getAllFiles: () => [hookFile, 'web/modules/custom/my_module/my_module.module'],
      readFile: (f) => (f === hookFile ? hookSrc : null),
    });
    const ref = {
      fromNodeId: 'method:src',
      referenceName: 'hook_user_cancel',
      referenceKind: 'references' as const,
      line: 7,
      column: 0,
      filePath: hookFile,
      language: 'php' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved).not.toBeNull();
    expect(resolved!.targetNodeId).toBe('method:hook1');
    expect(resolved!.confidence).toBe(0.75);
    expect(resolved!.resolvedBy).toBe('framework');
  });

  it('resolves hook_X to a class-level attribute impl via method: arg', () => {
    const hookFile = 'web/modules/custom/my_module/src/Hook/Login.php';
    const hookSrc = `<?php
use Drupal\\Core\\Hook\\Attribute\\Hook;

#[Hook('user_login', method: 'onLogin')]
class LoginHooks {
  public function onLogin(): void {}
}
`;
    const methodNode = {
      id: 'method:login1',
      kind: 'method' as const,
      name: 'onLogin',
      qualifiedName: 'LoginHooks::onLogin',
      filePath: hookFile,
      language: 'php' as const,
      startLine: 5,
      endLine: 6,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const classNode = {
      id: 'class:login2',
      kind: 'class' as const,
      name: 'LoginHooks',
      qualifiedName: 'LoginHooks',
      filePath: hookFile,
      language: 'php' as const,
      startLine: 4,
      endLine: 7,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesInFile: (f) => (f === hookFile ? [classNode, methodNode] : []),
      getNodesByKind: () => [],
      getAllFiles: () => [hookFile],
      readFile: (f) => (f === hookFile ? hookSrc : null),
    });
    const ref = {
      fromNodeId: 'method:src',
      referenceName: 'hook_user_login',
      referenceKind: 'references' as const,
      line: 4,
      column: 0,
      filePath: hookFile,
      language: 'php' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved).not.toBeNull();
    expect(resolved!.targetNodeId).toBe('method:login1');
  });

  it('prefers a procedural *_X function over attribute impls when both exist', () => {
    const funcNode = {
      id: 'function:proc1',
      kind: 'function' as const,
      name: 'mymodule_user_cancel',
      qualifiedName: 'mymodule_user_cancel',
      filePath: 'web/modules/custom/mymodule/mymodule.module',
      language: 'php' as const,
      startLine: 3,
      endLine: 8,
      startColumn: 0,
      endColumn: 0,
      updatedAt: 0,
    };
    const ctx = makeContext({
      getNodesByKind: (kind) => (kind === 'function' ? [funcNode] : []),
      getAllFiles: () => [],
    });
    const ref = {
      fromNodeId: 'function:other',
      referenceName: 'hook_user_cancel',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'x.module',
      language: 'php' as const,
    };
    const resolved = drupalResolver.resolve(ref, ctx);
    expect(resolved!.targetNodeId).toBe('function:proc1');
  });

  it('returns null for hook_X when no procedural or attribute impl exists', () => {
    const ctx = makeContext({ getNodesByKind: () => [], getAllFiles: () => [] });
    const ref = {
      fromNodeId: 'x',
      referenceName: 'hook_nonexistent_hook',
      referenceKind: 'references' as const,
      line: 1,
      column: 0,
      filePath: 'x.module',
      language: 'php' as const,
    };
    expect(drupalResolver.resolve(ref, ctx)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// End-to-end integration test
// ---------------------------------------------------------------------------

beforeAll(async () => {
  await initGrammars();
  await loadAllGrammars();
});

describe('Drupal end-to-end — route node linked to controller method', () => {
  let tmpDir: string | undefined;
  afterEach(() => {
    if (tmpDir) fs.rmSync(tmpDir, { recursive: true, force: true });
    tmpDir = undefined;
  });

  it('creates a route→controller edge from routing.yml to PHP class', async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-drupal-'));

    // Minimal composer.json to trigger Drupal detection
    fs.writeFileSync(
      path.join(tmpDir, 'composer.json'),
      JSON.stringify({ require: { 'drupal/core-recommended': '~10.5' } }),
    );

    // Module directory structure
    const modDir = path.join(tmpDir, 'web', 'modules', 'custom', 'my_module');
    fs.mkdirSync(path.join(modDir, 'src', 'Controller'), { recursive: true });

    // routing.yml
    fs.writeFileSync(
      path.join(modDir, 'my_module.routing.yml'),
      [
        'my_module.hello:',
        "  path: '/hello'",
        '  defaults:',
        "    _controller: '\\Drupal\\my_module\\Controller\\HelloController::build'",
        "    _title: 'Hello'",
        '  requirements:',
        "    _permission: 'access content'",
      ].join('\n') + '\n',
    );

    // PHP controller
    fs.writeFileSync(
      path.join(modDir, 'src', 'Controller', 'HelloController.php'),
      [
        '<?php',
        'namespace Drupal\\my_module\\Controller;',
        'use Drupal\\Core\\Controller\\ControllerBase;',
        'class HelloController extends ControllerBase {',
        '  public function build() {',
        "    return ['#markup' => 'Hello'];",
        '  }',
        '}',
      ].join('\n') + '\n',
    );

    const cg = CodeGraph.initSync(tmpDir);
    await cg.indexAll();

    // Route node must exist
    const routes = cg.getNodesByKind('route');
    expect(routes.length).toBeGreaterThan(0);
    const route = routes.find((n) => n.name.includes('/hello'));
    expect(route).toBeDefined();

    // Controller method must be indexed
    const methods = cg.getNodesByKind('method');
    const buildMethod = methods.find((n) => n.name === 'build');
    expect(buildMethod).toBeDefined();

    // Edge: route → build method (or class fallback)
    const edges = cg.getOutgoingEdges(route!.id);
    expect(edges.length).toBeGreaterThan(0);

    cg.close();
  });
});
