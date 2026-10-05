const fs = require('node:fs');
const { spawn, spawnSync } = require('node:child_process');
const writer = require(process.argv[2]);
const root = process.argv[3];
const operation = process.argv[4];
const record = (pid, mode = 'daemon') => ({ pid, mode, startedAt: 1, ready: false });

if (operation === 'hold') {
  const path = require('node:path');
  const kernel = require(path.join(path.dirname(process.argv[2]), '../extraction/kernel/loader.js')).requireKernel();
  const held = kernel.tryWriterMutationLock(`${writer.getWriterPidPath(root)}.mutation.lock`);
  if (!held) process.exit(2);
  process.send('held');
  process.on('message', () => { held.release(); process.exit(0); });
} else if (operation === 'retire') {
  (async () => {
    writer.tryAcquireWriterLock(root, 'direct');
    const holder = spawn(process.execPath, [__filename, process.argv[2], root, 'hold'], { stdio: ['ignore', 'ignore', 'inherit', 'ipc'] });
    try {
      await new Promise((resolve, reject) => {
        holder.once('message', resolve);
        holder.once('error', reject);
        holder.once('exit', () => reject(new Error('holder exited before readiness')));
      });
      writer.releaseWriterLock(root);
      const exited = new Promise(resolve => holder.once('exit', resolve));
      holder.send('release');
      await exited;
      const deadline = Date.now() + 2000;
      while (writer.readWriterLock(root) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
      const released = writer.readWriterLock(root) === null;
      const contender = spawnSync(process.execPath, [__filename, process.argv[2], root, 'reacquire'], { encoding: 'utf8', timeout: 10000 });
      if (contender.status !== 0) throw new Error(contender.stderr);
      process.stdout.write(JSON.stringify({ released, ...JSON.parse(contender.stdout) }));
    } finally {
      if (holder.exitCode === null) holder.kill('SIGKILL');
    }
  })().catch(error => { console.error(error); process.exitCode = 1; });
} else if (operation === 'crash') {
  const path = require('node:path');
  const kernel = require(path.join(path.dirname(process.argv[2]), '../extraction/kernel/loader.js')).requireKernel();
  const held = kernel.tryWriterMutationLock(`${writer.getWriterPidPath(root)}.mutation.lock`);
  if (!held) process.exit(2);
  process.stdout.write(JSON.stringify({ held: true }));
  process.exit(0); // Deliberately bypass release/finalizers.
} else if (operation === 'reacquire') {
  const result = writer.tryAcquireWriterLock(root, 'direct');
  process.stdout.write(JSON.stringify({ acquired: result.kind === 'acquired' }));
} else if (operation === 'competitor') {
  const from = Number(process.argv[5]);
  const claimed = writer.swapWriterLock(root, from, record(process.pid, 'handover'));
  const promoted = claimed && writer.swapWriterLock(root, process.pid, record(333));
  process.stdout.write(JSON.stringify({ claimed, promoted }));
} else {
  const file = writer.getWriterPidPath(root);
  const from = operation === 'acquire' ? 999999999 : process.pid;
  fs.writeFileSync(file, JSON.stringify(record(from)));
  const boundary = operation === 'release' || operation === 'acquire' ? 'unlinkSync' : 'renameSync';
  const original = fs[boundary];
  let paused = false;
  let competitor;
  let acquired;
  fs[boundary] = (...args) => {
    const target = boundary === 'renameSync' ? args[1] : args[0];
    if (target === file && !paused) {
      paused = true;
      if (operation === 'successor') {
        writer.releaseWriterLock(root);
        acquired = writer.tryAcquireWriterLock(root, 'fallback').kind;
      } else {
        const child = spawnSync(process.execPath, [__filename, process.argv[2], root, 'competitor', String(from)], { encoding: 'utf8', timeout: 10000 });
        if (child.status !== 0) throw new Error(child.stderr || `actor exited ${child.status}`);
        competitor = JSON.parse(child.stdout);
      }
    }
    return original(...args);
  };
  let result;
  if (operation === 'swap' || operation === 'successor') result = writer.swapWriterLock(root, from, record(operation === 'successor' ? 333 : 222));
  else if (operation === 'ready') writer.markWriterReady(root);
  else if (operation === 'release') writer.releaseWriterLock(root);
  else result = writer.tryAcquireWriterLock(root, 'fallback').kind;
  process.stdout.write(JSON.stringify({ paused, competitor, acquired, result, info: writer.readWriterLock(root), pid: process.pid }));
}
