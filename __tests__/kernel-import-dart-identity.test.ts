import { afterEach, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph | undefined;
function write(files: Record<string, string>): void {
  for (const [file, text] of Object.entries(files)) {
    const target = path.join(root, file);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, text);
  }
}
afterEach(() => {
  cg?.close();
  cg = undefined;
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

it('parks matching missing includes in both files, then retries both when the header appears', async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-import-identity-'));
  write({ 'a.c': '#include "inc/shared.h"\n', 'b.c': '#include "inc/shared.h"\n' });
  cg = await CodeGraph.init(root, { index: true });
  const targets = (file: string) => cg!.getOutgoingEdgesFrom(cg!.getNodesInFile(file).map(n => n.id), ['imports'])
    .map(e => cg!.getNode(e.target)!);
  for (const file of ['a.c', 'b.c']) expect(targets(file)).toEqual([]);
  write({ 'inc/shared.h': 'int shared(void);\n' });
  await cg.sync();
  for (const file of ['a.c', 'b.c']) {
    expect(targets(file).map(n => [n.kind, n.filePath])).toEqual([['file', 'inc/shared.h']]);
  }
});

const cases = [
  {
    name: 'an arrow body string opener does not extend parameter scope',
    files: { 'lib/main.dart': "void text() {}\nString wrap(String text) => '(' + text + ')';\nvoid run() { text(); }\n" },
    from: 'run', kind: 'calls', target: 'text', targetFile: 'lib/main.dart',
  },
  {
    name: 'a loop body string opener does not extend loop-local scope',
    files: { 'lib/main.dart': "void item() {}\nvoid run(List<String> items) { for (final item in items) print('['); item(); }\n" },
    from: 'run', kind: 'calls', target: 'item', targetFile: 'lib/main.dart',
  },
  {
    name: 'an initializer string opener does not extend constructor parameter scope',
    files: { 'lib/main.dart': "void text() {}\nclass Box { final String value; Box(String text): value = '{' + text + '}'; }\nvoid run() { text(); }\n" },
    from: 'run', kind: 'calls', target: 'text', targetFile: 'lib/main.dart',
  },
  {
    name: 'a multiline imported receiver resolves on its own source line',
    files: {
      'lib/main.dart': "import 'helper.dart' as kit;\nvoid run() { /* 🦊 */ kit\n  .helper(); }\n",
      'lib/helper.dart': 'void helper() {}\n',
    },
    from: 'run', kind: 'calls', target: 'helper', targetFile: 'lib/helper.dart',
  },
  {
    name: 'a multiline local receiver shadows the imported prefix',
    files: {
      'lib/main.dart': "import 'helper.dart' as kit;\nvoid run(Object kit) { /* 🦊 */ kit\n  .helper(); }\n",
      'lib/helper.dart': 'void helper() {}\n',
    },
    from: 'run', kind: 'calls', target: 'helper', targetFile: 'lib/helper.dart', absent: true,
  },
  {
    name: 'prefixed instance chain preserves the constructor library',
    files: {
      'lib/main.dart': "import 'package:kit/kit.dart' as kit;\nclass Widget { void touch() {} }\nvoid run() { kit.Widget().touch(); }\n",
      'packages/kit/lib/kit.dart': 'class Widget { Widget(); void touch() {} }\n',
    },
    from: 'run', kind: 'calls', target: 'Widget::touch', targetFile: 'packages/kit/lib/kit.dart',
  },
  {
    name: 'prefixed factory return preserves the declared type library',
    files: {
      'lib/main.dart': "import 'package:kit/kit.dart' as kit;\nclass Widget { void touch() {} }\nvoid run() { kit.make().touch(); }\n",
      'packages/kit/lib/kit.dart': 'class Widget { void touch() {} }\nWidget make() => Widget();\n',
    },
    from: 'run', kind: 'calls', target: 'Widget::touch', targetFile: 'packages/kit/lib/kit.dart',
  },
  {
    name: 'callback parameter shadows the visible factory during chain inference',
    files: {
      'lib/main.dart': 'class Wrong { void touch() {} }\nclass Other {}\nWrong make() => Wrong();\nvoid run(Other Function() make) { make().touch(); }\n',
    },
    from: 'run', kind: 'calls', target: 'Wrong::touch', targetFile: 'lib/main.dart', absent: true,
  },
  {
    name: 'an adjacent unnamed extension does not hide the preceding class',
    files: {
      'lib/main.dart': "import 'package:kit/kit.dart' as kit;\nvoid run() { kit.Widget(); }\n",
      'packages/kit/lib/kit.dart': 'class Widget { Widget(); }\nextension on Widget {}\n',
    },
    from: 'run', kind: 'instantiates', target: 'Widget', targetFile: 'packages/kit/lib/kit.dart',
  },
  {
    name: 'metadata string delimiters do not consume following imports',
    files: {
      'lib/main.dart': "@Deprecated('(')\nlibrary;\nimport 'helper.dart';\nvoid run() { helper(); }\n",
      'lib/helper.dart': 'void helper() {}\n',
    },
    from: 'run', kind: 'calls', target: 'helper', targetFile: 'lib/helper.dart',
  },
  {
    name: 'annotation arguments can start on the following line',
    files: {
      'lib/main.dart': "import 'annotation.dart';\n@Mark\n(flag: true)\nclass Item {}\n",
      'lib/annotation.dart': 'class Mark { const Mark({bool flag = false}); }\n',
    },
    from: 'Item', kind: 'decorates', target: 'Mark', targetFile: 'lib/annotation.dart',
  },
];

describe('native Dart declaration identity and source boundaries', () => {
  it.each(cases)('$name', async c => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-dart-identity-'));
    write({ 'pubspec.yaml': 'name: app\n', 'packages/kit/pubspec.yaml': 'name: kit\n', ...c.files });
    cg = await CodeGraph.init(root, { index: true });
    const source = cg.getNodesInFile('lib/main.dart').find(n => n.name === c.from)!;
    expect(source).toBeDefined();
    const targets = cg.getOutgoingEdgesFrom([source.id]).filter(e => e.kind === c.kind)
      .map(e => cg!.getNode(e.target)!).map(n => `${n.filePath}:${n.qualifiedName}`);
    if (c.absent) expect(targets).not.toContain(`${c.targetFile}:${c.target}`);
    else {
      expect(targets).toContain(`${c.targetFile}:${c.target}`);
      if (c.target === 'Widget::touch') expect(targets).not.toContain('lib/main.dart:Widget::touch');
    }
  });
});

// Keep the external fixture in the test's own parent directory.
async function checkExternalDirective(mode: 'parent-path' | 'symlink'): Promise<void> {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-dart-uri-boundary-'));
  write({
    'project/pubspec.yaml': 'name: app\n',
    'project/lib/secret.dart': 'void secret() {}\n',
    'project/lib/main.dart': `import 'package:app/${mode === 'parent-path' ? '../../escape.dart' : 'escape.dart'}';\nvoid run() { secret(); }\n`,
    'escape.dart': "export 'package:app/secret.dart';\n",
  });
  if (mode === 'symlink') fs.symlinkSync(path.join(root, 'escape.dart'), path.join(root, 'project/lib/escape.dart'));
  cg = await CodeGraph.init(path.join(root, 'project'), { index: true });
  const source = cg.getNodesInFile('lib/main.dart').find(n => n.name === 'run')!;
  const targets = cg.getOutgoingEdgesFrom([source.id], ['calls']).map(e => cg!.getNode(e.target)!);
  expect(targets.map(n => `${n.filePath}:${n.name}`)).not.toContain('lib/secret.dart:secret');
}
it('refuses Dart package directives outside the indexed root through parent paths', () => checkExternalDirective('parent-path'));
it.runIf(process.platform !== 'win32')('refuses Dart package directives outside the indexed root through symlinks', () => checkExternalDirective('symlink'));
