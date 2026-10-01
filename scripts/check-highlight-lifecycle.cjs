#!/usr/bin/env node
/** Verify the production process adapter on the current OS without loading native grammars. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const ts = require('typescript');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-highlight-lifecycle-'));
let completed = false;
try {
  const source = fs.readFileSync(path.join(__dirname, '../src/ui-server/highlight/bounded-tokenize.ts'), 'utf8');
  const output = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  fs.writeFileSync(path.join(root, 'bounded-tokenize.js'), output);
  fs.writeFileSync(path.join(root, 'tokenize-worker.js'), `
const fs = require('node:fs');
const path = require('node:path');
process.on('message', message => {
  fs.appendFileSync(path.join(__dirname, 'children.jsonl'), JSON.stringify({ pid: process.pid, text: message.text }) + '\\n');
  if (message.text === 'busy') { while (true) {} }
  process.send({ id: message.id, result: { spans: [], grammars: [message.text] } });
});
process.on('disconnect', () => process.exit(0));
`);
  fs.writeFileSync(path.join(root, 'probe.cjs'), `
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { tokenizeBounded, stopHighlightWorker } = require('./bounded-tokenize.js');
(async () => {
  const stalled = tokenizeBounded('busy', 'typescript', 1500);
  const queued = tokenizeBounded('queued', 'typescript', 1500);
  assert.deepEqual(await stalled, { timedOut: true });
  assert.deepEqual(await queued, { timedOut: true });
  const first = JSON.parse(fs.readFileSync(path.join(__dirname, 'children.jsonl'), 'utf8').trim().split('\\n')[0]);
  assert.equal(first.text, 'busy');
  assert.throws(() => process.kill(first.pid, 0), error => error.code === 'ESRCH');
  assert.deepEqual(await tokenizeBounded('fresh', 'typescript', 1500), { result: { spans: [], grammars: ['fresh'] } });
  const last = JSON.parse(fs.readFileSync(path.join(__dirname, 'children.jsonl'), 'utf8').trim().split('\\n').at(-1));
  assert.notEqual(last.pid, first.pid);
  await stopHighlightWorker();
  assert.throws(() => process.kill(last.pid, 0), error => error.code === 'ESRCH');
  console.log(JSON.stringify({ platform: process.platform, node: process.version, timedOutAndQueuedSettled: true, restarted: true, shutdownReaped: true }));
})().catch(async error => {
  console.error(error);
  await stopHighlightWorker();
  process.exitCode = 1;
});
`);
  // The supervisor checks a completion marker: an unreferenced child must not let
  // the probe exit successfully before its awaited teardown has finished.
  const stdout = execFileSync(process.execPath, [path.join(root, 'probe.cjs')], { encoding: 'utf8', timeout: 15_000 });
  const result = JSON.parse(stdout.trim());
  assert.equal(result.platform, process.platform);
  assert.equal(result.timedOutAndQueuedSettled, true);
  assert.equal(result.restarted, true);
  assert.equal(result.shutdownReaped, true);
  console.log(JSON.stringify(result));
  completed = true;
} finally {
  const children = path.join(root, 'children.jsonl');
  if (!completed && fs.existsSync(children)) {
    for (const line of fs.readFileSync(children, 'utf8').trim().split('\n').filter(Boolean)) {
      const { pid } = JSON.parse(line);
      try { process.kill(pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
  }
  fs.rmSync(root, { recursive: true, force: true, maxRetries: 5 });
}
