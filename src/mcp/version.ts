/**
 * Resolved MCP build version, computed once at module load.
 *
 * The version string is the rendezvous datum between cooperating daemon and
 * proxy processes: the daemon advertises its version in the hello line, and
 * the proxy refuses to share IPC across a mismatch (falls back to direct
 * mode). Keeping the resolution in one place avoids drift between the CLI
 * `--version` output and the daemon handshake — the CLI reports this value at
 * every version-reporting site (`--version` / `-v` / `-version` / `-V`, the
 * `version` subcommand, and `status --json`) rather than reading
 * `package.json` itself. Managed fork builds append their recorded source
 * revision so two builds of the same package release cannot share a stale
 * daemon.
 *
 * `codegraph upgrade` and installation-change checks compare package releases
 * and use the bare published version.
 *
 * Resolution strategy: read the bundled `package.json` two levels up from
 * this file — same relative position whether we're loaded from `src/mcp/` or
 * the `dist/mcp/` output, since `tsc` preserves the layout. If reading fails
 * (e.g. the package was unpacked oddly), fall back to "0.0.0-unknown" — a
 * sentinel that will never match a real version, so the proxy harmlessly
 * falls back to direct mode.
 */

import * as fs from "fs";
import * as path from "path";

/** The `package.json` of the install this process runs from. */
export const CodeGraphPackageJsonPath = path.join(__dirname, '..', '..', 'package.json');

function readReleaseVersion(): string {
  try {
    const parsed = JSON.parse(fs.readFileSync(CodeGraphPackageJsonPath, 'utf8'));
    if (typeof parsed?.version === 'string' && parsed.version.length > 0) return parsed.version;
  } catch {
    // Fall through to the identity sentinel.
  }
  return '0.0.0-unknown';
}

/** Cached package release for detecting an installation replaced in place. */
export const CodeGraphPackageReleaseVersion = readReleaseVersion();

function readPackageVersion(): string {
  const release = CodeGraphPackageReleaseVersion;
  if (release === '0.0.0-unknown') return release;
  try {
    const buildPath = path.join(__dirname, '..', '..', 'dist', 'build-revision.json');
    if (fs.existsSync(buildPath)) {
      const build = JSON.parse(fs.readFileSync(buildPath, 'utf8'));
      if (!/^[a-f0-9]{40}$/.test(build?.revision ?? '')) return '0.0.0-unknown';
      return `${release.split('+')[0]}+${build.revision}`;
    }
    return release;
  } catch {
    return '0.0.0-unknown';
  }
}

export const CodeGraphPackageVersion = readPackageVersion();

/**
 * Whether `version` is a CodeGraph release older than `than` — the test a
 * launcher applies before it replaces a running daemon (#2335). Plain
 * `MAJOR.MINOR.PATCH` and managed revisions compare: a prerelease, an arbitrary suffix or the
 * "0.0.0-unknown" sentinel is never older. A daemon is replaced only by a
 * strictly newer release, so two installed versions can never take turns
 * stopping each other's daemon.
 */
export function isOlderRelease(version: string, than: string): boolean {
  const a = parseRelease(version);
  const b = parseRelease(than);
  if (!a || !b) return false;
  if (a[0] !== b[0]) return a[0] < b[0];
  if (a[1] !== b[1]) return a[1] < b[1];
  return a[2] < b[2];
}

function parseRelease(version: string): [number, number, number] | null {
  const m = /^(\d+)\.(\d+)\.(\d+)(?:\+[a-f0-9]{40})?$/.exec(version);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
}
