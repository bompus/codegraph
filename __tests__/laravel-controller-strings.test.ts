/** Route strings preserve controller identity across namespace and resource forms. */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

const controller = (ns: string, name: string, methods: string[]) => `<?php

namespace App\\Http\\Controllers\\${ns};

class ${name}
{
${methods.map((m) => `    public function ${m}() { return 1; }`).join('\n')}
}
`;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-laravel-strings-'));
  const files: Record<string, string> = {
    'artisan': '#!/usr/bin/env php\n',
    'composer.json': JSON.stringify({ require: { 'laravel/framework': '^10.0' } }),
    'routes/admin.php': `<?php

use Illuminate\\Support\\Facades\\Route;

Route::get('uploads/{id}/inline', 'Common\\Uploads@inline')->name('inline');
Route::delete('uploads/{id}', 'Common\\Uploads@destroy');
Route::get('portal/uploads/{id}', 'Portal\\Uploads@inline');
Route::resource('companies', 'Common\\Companies', ['middleware' => ['dropzone']]);
Route::get('absolute', '\\App\\Http\\Controllers\\Common\\Uploads@inline');
Route::get('module', 'Modules\\Billing\\Http\\Controllers\\Payments@show');
Route::get('module-uploads', 'Modules\\Billing\\Http\\Controllers\\Common\\Uploads@inline');
Route::get('missing-namespace', 'Missing\\Companies@index');
Route::resource('missing-companies', 'Missing\\Companies', ['only' => ['index']]);
Route::get('ambiguous', 'Uploads@inline');
Route::get('tuple-portal', [\\App\\Http\\Controllers\\Portal\\Uploads::class, 'inline']);
Route::resource('class-uploads', \\App\\Http\\Controllers\\Common\\Uploads::class, ['only' => ['index']]);
Route::get('missing-action', 'Common\\Companies@destroy');
Route::apiResource('api-companies', 'Common\\Companies', ['only' => ['index']]);
Route::resource('class-companies', \\App\\Http\\Controllers\\Common\\Companies::class, ['only' => ['index']]);
`,
    'app/Http/Controllers/Common/Uploads.php': controller('Common', 'Uploads', ['inline', 'destroy']),
    'app/Http/Controllers/Portal/Uploads.php': controller('Portal', 'Uploads', ['inline']),
    'app/Http/Controllers/Common/Companies.php': controller('Common', 'Companies', ['index', 'store']).trimEnd() + ' class Decoy { public function destroy() {} }',
    'Modules/Billing/Http/Controllers/Common/Uploads.php': '<?php namespace Modules\\Billing\\Http\\Controllers\\Common; class Uploads { public function inline() {} }',
    'Modules/Billing/Http/Controllers/Payments.php': '<?php namespace Modules\\Billing\\Http\\Controllers; class Payments { public function show() {} }',
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

const servedBy = (routeName: string) => {
  const route = cg.getNodesByKind('route').find((r) => r.name === routeName);
  expect(route, routeName).toBeDefined();
  return cg.getOutgoingEdgesFrom([route!.id]).map((e) => cg.getNode(e.target)!).map((n) => `${n.name} ${n.filePath}`);
};

describe('Laravel string controllers', () => {
  it('resolve without a Controller suffix, by their namespace path', () => {
    expect(servedBy('GET /uploads/{id}/inline')).toEqual(['inline app/Http/Controllers/Common/Uploads.php']);
    expect(servedBy('DELETE /uploads/{id}')).toEqual(['destroy app/Http/Controllers/Common/Uploads.php']);
    expect(servedBy('GET /portal/uploads/{id}')).toEqual(['inline app/Http/Controllers/Portal/Uploads.php']);
  });

  it('keeps fully qualified App and module controller paths', () => {
    expect(servedBy('GET /absolute')).toEqual(['inline app/Http/Controllers/Common/Uploads.php']);
    expect(servedBy('GET /module')).toEqual(['show Modules/Billing/Http/Controllers/Payments.php']);
    expect(servedBy('GET /module-uploads')).toEqual(['inline Modules/Billing/Http/Controllers/Common/Uploads.php']);
    expect(servedBy('GET /tuple-portal')).toEqual(['inline app/Http/Controllers/Portal/Uploads.php']);
  });

  it('declines missing namespaces, ambiguous short names and another class’s method', () => {
    expect(servedBy('GET /missing-namespace')).toEqual([]);
    expect(servedBy('resource:missing-companies')).toEqual([]);
    expect(servedBy('GET /ambiguous')).toEqual([]);
    expect(servedBy('GET /missing-action')).toEqual([]);
  });

  it('read a resource controller named by string, past its options', () => {
    expect(servedBy('resource:companies')).toEqual(['Companies app/Http/Controllers/Common/Companies.php']);
    expect(servedBy('resource:api-companies')).toEqual(['Companies app/Http/Controllers/Common/Companies.php']);
    expect(servedBy('resource:class-companies')).toEqual(['Companies app/Http/Controllers/Common/Companies.php']);
    expect(servedBy('resource:class-uploads')).toEqual(['Uploads app/Http/Controllers/Common/Uploads.php']);
  });
});
