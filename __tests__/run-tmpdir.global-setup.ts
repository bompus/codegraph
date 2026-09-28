/**
 * One temp directory per test run, removed when the run ends.
 *
 * Hundreds of suites `mkdtemp` under `os.tmpdir()` and many never delete what
 * they made; each full run left ~150 directories behind, and on hosts where
 * /tmp is RAM-backed that piled up to gigabytes. Pointing TMPDIR at a run
 * directory before any worker starts catches every one of them — including
 * what spawned CLI/MCP processes write — without touching the suites.
 *
 * The run directory (`cgt-XXXXXX`) sits in the system temp directory; set
 * CODEGRAPH_TEST_TMPDIR to put it elsewhere (e.g. on disk when /tmp is
 * tmpfs). Avoid a parent inside a directory CodeGraph excludes by default
 * (`.cache`, `node_modules`, …): the test projects under it would index
 * nothing. Daemon tests put Unix sockets under it, and a socket path over
 * ~100 bytes is refused, so a run directory too long for that is reached
 * through a short `/tmp/cgt-*` symlink instead: socket paths stay short and
 * the files still land on the parent's disk. Run directories a crashed run
 * left behind are swept once they are a day old.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

const STALE_MS = 24 * 60 * 60 * 1000;
// The longest run directory whose sockets still fit: the daemon's fallback
// socket is `<run>/fXXXXXX/codegraph-<16 hex>.sock`, 40 bytes past the run
// directory, and the limit is 104 bytes on macOS.
const SOCKET_SAFE_RUN_DIR = 60;
const ALIAS_PARENT = '/tmp';

/** A short symlink to `runDir` when its sockets would not fit, else null. */
function socketSafeAlias(runDir: string): string | null {
  if (process.platform === 'win32' || runDir.length <= SOCKET_SAFE_RUN_DIR) return null;
  // Links a crashed run left behind point at run directories since swept.
  for (const name of fs.readdirSync(ALIAS_PARENT)) {
    if (!name.startsWith('cgt-')) continue;
    const link = path.join(ALIAS_PARENT, name);
    try {
      if (fs.lstatSync(link).isSymbolicLink() && !fs.existsSync(link)) fs.unlinkSync(link);
    } catch {
      // another run removed it first
    }
  }
  const link = path.join(ALIAS_PARENT, path.basename(runDir));
  fs.symlinkSync(runDir, link);
  return link;
}

export default function setup(): () => void {
  // Directly in the parent, with short names: daemon tests bind Unix sockets
  // under it, and every level of nesting eats into the ~100-byte path limit.
  const parent = process.env.CODEGRAPH_TEST_TMPDIR || os.tmpdir();
  fs.mkdirSync(parent, { recursive: true });
  for (const name of fs.readdirSync(parent)) {
    if (!name.startsWith('cgt-')) continue;
    const dir = path.join(parent, name);
    try {
      if (Date.now() - fs.statSync(dir).mtimeMs > STALE_MS) fs.rmSync(dir, { recursive: true, force: true });
    } catch {
      // another run removed it first
    }
  }

  const runDir = fs.mkdtempSync(path.join(parent, 'cgt-'));
  const alias = socketSafeAlias(runDir);
  const saved = { TMPDIR: process.env.TMPDIR, TMP: process.env.TMP, TEMP: process.env.TEMP };
  // os.tmpdir() reads TMPDIR on POSIX and TMP/TEMP on Windows.
  process.env.TMPDIR = process.env.TMP = process.env.TEMP = alias ?? runDir;

  return () => {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
    if (alias) fs.rmSync(alias, { force: true });
    fs.rmSync(runDir, { recursive: true, force: true });
  };
}
