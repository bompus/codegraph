// Preload for sync-ab.mjs --proxies (Node only): total bytes allocated over the whole
// command, from V8's sampling heap profiler with collected objects kept. Writes
// {"allocatedMiB": N} to ALLOC_TOTAL_OUT at exit.
const fs = require('node:fs');
const inspector = require('node:inspector');
const out = process.env.ALLOC_TOTAL_OUT;
const session = new inspector.Session();
session.connect();
session.post('HeapProfiler.startSampling', {
  samplingInterval: 4096,
  includeObjectsCollectedByMajorGC: true,
  includeObjectsCollectedByMinorGC: true,
});
process.on('exit', () => {
  // Same-thread inspector sessions answer synchronously, so this runs before exit.
  session.post('HeapProfiler.stopSampling', (error, result) => {
    if (error) return fs.writeFileSync(out, JSON.stringify({ error: String(error) }));
    let total = 0;
    const walk = (node) => {
      total += node.selfSize;
      for (const child of node.children) walk(child);
    };
    walk(result.profile.head);
    fs.writeFileSync(out, JSON.stringify({ allocatedMiB: Math.round((total / 2 ** 20) * 10) / 10 }));
  });
});
