/**
 * One temp directory per test run, removed when the run ends.
 *
 * Hundreds of suites `mkdtemp` under `os.tmpdir()` and many never delete what
 * they made; each full run left ~150 directories behind, and on hosts where
 * /tmp is RAM-backed that piled up to gigabytes. Pointing TMPDIR at a run
 * directory before any worker starts catches every one of them — including
 * what spawned CLI/MCP processes write — without touching the suites.
 *
 * The run directory sits in the system temp directory; set
 * CODEGRAPH_TEST_TMPDIR to put it elsewhere (e.g. on disk when /tmp is
 * tmpfs). Avoid a parent inside a directory CodeGraph excludes by default
 * (`.cache`, `node_modules`, …): the test projects under it would index
 * nothing. Keep the path short, too: daemon tests put Unix sockets under it,
 * and a socket path over ~100 bytes is refused. Run directories a crashed run
 * left behind are swept once they are a day old.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const STALE_MS = 24 * 60 * 60 * 1000;

export default function setup(): () => void {
  const parent = process.env.CODEGRAPH_TEST_TMPDIR || path.join(os.tmpdir(), 'codegraph-tests');
  fs.mkdirSync(parent, { recursive: true });
  for (const name of fs.readdirSync(parent)) {
    if (!name.startsWith('run-')) continue;
    const dir = path.join(parent, name);
    try {
      if (Date.now() - fs.statSync(dir).mtimeMs > STALE_MS) fs.rmSync(dir, { recursive: true, force: true });
    } catch {
      // another run removed it first
    }
  }

  const runDir = fs.mkdtempSync(path.join(parent, 'run-'));
  const saved = { TMPDIR: process.env.TMPDIR, TMP: process.env.TMP, TEMP: process.env.TEMP };
  // os.tmpdir() reads TMPDIR on POSIX and TMP/TEMP on Windows.
  process.env.TMPDIR = process.env.TMP = process.env.TEMP = runDir;

  return () => {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
    fs.rmSync(runDir, { recursive: true, force: true });
  };
}
