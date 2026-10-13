import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
import { canonical } from './canonical';

const [repo, corpus, out] = process.argv.slice(2);
if (!repo || !corpus || !out) throw new Error('usage: prepare-nest.ts REPO CORPUS OUTPUT_DIR');
const now = 1700000000000;
function context(files: Record<string, string>, manifests: Record<string, string> = {}) {
  files = Object.fromEntries(Object.entries(files).filter(([p]) => p !== 'package.json').sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0));
  const nodes: any[] = [];
  for (const [filePath, content] of Object.entries(files)) {
    const source = ts.createSourceFile(filePath, content, ts.ScriptTarget.Latest, true);
    function visit(n: ts.Node) {
      if (ts.isClassDeclaration(n) && n.name) {
        const startLine = source.getLineAndCharacterOfPosition(n.getStart(source)).line + 1;
        nodes.push({ id: `class:${filePath}:${startLine}:${n.name.text}`, kind: 'class', name: n.name.text,
          qualifiedName: `${filePath}::${n.name.text}`, filePath, startLine,
          endLine: source.getLineAndCharacterOfPosition(n.end).line + 1, startColumn: 0, endColumn: 0,
          language: /\.tsx?$/.test(filePath) ? 'typescript' : 'javascript', updatedAt: now });
      }
      ts.forEachChild(n, visit);
    }
    visit(source);
  }
  const refs: any[] = [];
  for (const node of nodes) {
    for (const filePath of [node.filePath, `${path.posix.dirname(node.filePath)}/consumer.ts`, 'unrelated/consumer.ts', 'legacy.txt']) {
      refs.push({fromNodeId: `caller:${refs.length}`, referenceName: node.name, referenceKind: 'references',
        line: 1, column: 0, filePath, language: 'typescript'});
    }
  }
  for (const referenceName of ['MissingService', 'MissingModule', 'plain']) {
    refs.push({fromNodeId: `caller:${refs.length}`, referenceName, referenceKind: 'references', line: 1, column: 0, filePath: 'missing.ts', language: 'typescript'});
  }
  return { files, manifests, nodes, refs, now };
}
const cases: any[] = [];
for (const test of ['nestjs-global-prefix.test.ts', 'nestjs-provider-same-file.test.ts']) {
  const content = fs.readFileSync(path.join(repo, '__tests__', test), 'utf8');
  const source = ts.createSourceFile(test, content, ts.ScriptTarget.Latest, true);
  const constants: Record<string, unknown> = { JSON };
  for (const statement of source.statements) {
    if (ts.isVariableStatement(statement)) for (const d of statement.declarationList.declarations) {
      if (ts.isIdentifier(d.name) && d.initializer && ts.isNoSubstitutionTemplateLiteral(d.initializer)) constants[d.name.text] = d.initializer.text;
    }
  }
  function visit(n: ts.Node) {
    if (ts.isObjectLiteralExpression(n) && n.properties.length > 0 && n.properties.every((p) => ts.isPropertyAssignment(p) && ts.isStringLiteral(p.name) && /\.(ts|js|json)$/.test(p.name.text))) {
      const files = vm.runInNewContext(`(${n.getText(source)})`, constants) as Record<string, string>;
      const manifests = files['package.json'] ? {'package.json': files['package.json']} : {};
      cases.push({name: `${test}:${n.getStart(source)}`, context: context(files, manifests)});
      return;
    }
    ts.forEachChild(n, visit);
  }
  visit(source);
}
fs.writeFileSync(path.join(out, 'nest-fixtures.input.json'), canonical(cases));

const edges = JSON.parse(fs.readFileSync(path.join(repo, 'rust-pilot/harness/edge-cases.json'), 'utf8'));
const edgeCases = edges.map((c: any) => ({name: c.name, context: context(c.files, c.manifests)}));
fs.writeFileSync(path.join(out, 'nest-edges.input.json'), canonical(edgeCases));
const selected: Record<string, string> = {};
const manifests: Record<string, string> = {};
let scanned = 0;
function walk(dir: string) {
  for (const entry of fs.readdirSync(dir, {withFileTypes: true}).sort((a, b) => a.name < b.name ? -1 : 1)) {
    const absolute = path.join(dir, entry.name);
    if (entry.isDirectory()) { if (entry.name !== '.git' && entry.name !== '.codegraph') walk(absolute); }
    else if (entry.isFile() && (entry.name.endsWith('.ts') || entry.name === 'package.json')) {
      const rel = path.relative(corpus, absolute).split(path.sep).join('/');
      const content = fs.readFileSync(absolute, 'utf8');
      if (entry.name === 'package.json') manifests[rel] = content;
      else {
        scanned++;
        if (/(?:\bfrom\s*['"]@nestjs\/|\bimport\s*['"]@nestjs\/|\brequire\s*\(\s*['"]@nestjs\/|@(?:Controller|Module|Injectable|Catch|Resolver|Query|Mutation|Subscription|WebSocketGateway|SubscribeMessage|MessagePattern|EventPattern|Get|Post|Put|Patch|Delete|Head|Options|All|Inject|UseGuards|Version)\s*\()/.test(content)) selected[rel] = content;
      }
    }
  }
}
walk(corpus);
fs.writeFileSync(path.join(out, `nest-${path.basename(corpus)}.input.json`), canonical([{name: path.basename(corpus), context: context(selected, manifests)}]));
console.log(JSON.stringify({fixtures: cases.length, edgeCases: edgeCases.length, scannedTsFiles: scanned, selectedCorpus: Object.keys(selected).length}));
