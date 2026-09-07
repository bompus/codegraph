/**
 * Resolved MCP build version, computed once at module load.
 *
 * The version string is the rendezvous datum between cooperating daemon and
 * proxy processes: the daemon advertises its version in the hello line, and
 * the proxy refuses to share IPC across a mismatch (falls back to direct
 * mode). Keeping the resolution in one place avoids drift between the CLI
 * `--version` output (which reads `package.json` directly) and the daemon
 * handshake. Managed fork builds append their recorded source revision so two
 * builds of the same package release cannot share a stale daemon.
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

function readPackageVersion(): string {
  try {
    const pkgPath = path.join(__dirname, "..", "..", "package.json");
    const raw = fs.readFileSync(pkgPath, "utf8");
    const parsed = JSON.parse(raw);
    if (typeof parsed?.version === "string" && parsed.version.length > 0) {
      const buildPath = path.join(__dirname, "..", "..", "dist", "build-revision.json");
      if (fs.existsSync(buildPath)) {
        const build = JSON.parse(fs.readFileSync(buildPath, "utf8"));
        if (!/^[a-f0-9]{40}$/.test(build?.revision ?? "")) return "0.0.0-unknown";
        return `${parsed.version.split("+")[0]}+${build.revision}`;
      }
      return parsed.version;
    }
  } catch {
    // Fall through to sentinel.
  }
  return "0.0.0-unknown";
}

export const CodeGraphPackageVersion = readPackageVersion();
