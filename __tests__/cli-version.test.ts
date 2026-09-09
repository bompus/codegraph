/**
 * Tests for the `codegraph version` affordances.
 *
 * The version should be reachable however a user reaches for it — the bare
 * `version` subcommand, lowercase `-v`, single-dash `-version`, plus
 * commander's stock `--version` / `-V`. All of them print the build's
 * reported identity and nothing else.
 *
 * "Reported identity" is CodeGraphPackageVersion, not package.json's version:
 * a managed fork build stamps its source revision into
 * `dist/build-revision.json`, and the CLI must surface it, because the
 * deployment guards compare the version string against the deployed build and
 * the running daemon. Two builds of the same npm release are otherwise
 * indistinguishable. In a plain source checkout (no stamp) the two are equal,
 * which is why the spelling tests below compare against the helper's answer
 * rather than package.json — comparing against package.json would pass here
 * and silently permit the regression in the builds that care.
 *
 * Exercised end-to-end against the built binary (same approach as
 * status-json.test.ts) so the spellings survive future CLI refactors.
 */

import { describe, it, expect } from "vitest";
import { execFileSync } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

const BIN = path.resolve(__dirname, "../dist/bin/codegraph.js");
const VERSION_MODULE = path.resolve(__dirname, "../dist/mcp/version.js");

/**
 * What this build reports itself as — read from the one resolver rather than
 * recomputed here, so the test cannot drift from the rule it is pinning.
 * Equals package.json's version in a source checkout; carries a `+<revision>`
 * suffix in a managed build.
 */
const REPORTED_VERSION = execFileSync(
  process.execPath,
  ["-e", "console.log(require(process.argv[1]).CodeGraphPackageVersion)", VERSION_MODULE],
  { encoding: "utf8" },
).trim();

function run(args: string[]): string {
  return execFileSync(process.execPath, [BIN, ...args], {
    encoding: "utf-8",
    // Skip the daemon and the wasm-flag re-exec so the command resolves in a
    // single fast process (no graph work happens for a version print anyway).
    env: { ...process.env, CODEGRAPH_NO_DAEMON: "1", CODEGRAPH_WASM_RELAUNCHED: "1" },
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

describe("codegraph version affordances", () => {
  it("distinguishes managed builds of the same release and caches the loaded identity", () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "codegraph-build-version-"));
    try {
      const moduleDir = path.join(root, "dist", "mcp");
      fs.mkdirSync(moduleDir, { recursive: true });
      fs.copyFileSync(
        path.resolve(__dirname, "../dist/mcp/version.js"),
        path.join(moduleDir, "version.js"),
      );
      fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ version: "1.6.0" }));
      const modulePath = path.join(moduleDir, "version.js");
      const stamp = path.join(root, "dist", "build-revision.json");
      const readVersion = () =>
        execFileSync(
          process.execPath,
          ["-e", "console.log(require(process.argv[1]).CodeGraphPackageVersion)", modulePath],
          { encoding: "utf8" },
        ).trim();
      expect(readVersion()).toBe("1.6.0");
      fs.writeFileSync(stamp, JSON.stringify({ revision: "a".repeat(40) }));
      expect(readVersion()).toBe(`1.6.0+${"a".repeat(40)}`);
      const cached = execFileSync(
        process.execPath,
        [
          "-e",
          'const m=require(process.argv[1]); require("fs").writeFileSync(process.argv[2], JSON.stringify({revision:"b".repeat(40)})); console.log(m.CodeGraphPackageVersion)',
          modulePath,
          stamp,
        ],
        { encoding: "utf8" },
      ).trim();
      expect(cached).toBe(`1.6.0+${"a".repeat(40)}`);
      expect(readVersion()).toBe(`1.6.0+${"b".repeat(40)}`);
      fs.writeFileSync(stamp, JSON.stringify({ revision: "not-a-commit" }));
      expect(readVersion()).toBe("0.0.0-unknown");
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it("reports a managed build's stamped revision through the CLI, not the bare package version", () => {
    // The regression this pins: the CLI used to print packageJson.version
    // directly, so `codegraph --version` could not say WHICH build of a
    // release was running — the exact thing the fork's deployment guards
    // compare. Proving that end-to-end needs a stamped install tree, and
    // stamping this repo's own dist/ would race every other suite that spawns
    // the binary (status-json's version assertion among them).
    //
    // So build a throwaway install root instead. Only dist/bin and dist/mcp
    // are real copies — those are the files that derive the install root from
    // __dirname, and Node resolves __dirname through symlinks to the real
    // path, which would send them back to this repo. The rest of dist/ is
    // linked, so the sandbox costs a few MB and well under a second.
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "codegraph-stamped-cli-"));
    const realDist = path.resolve(__dirname, "../dist");
    const originalStampPath = path.join(realDist, "build-revision.json");
    const readOriginalStamp = () =>
      fs.existsSync(originalStampPath) ? fs.readFileSync(originalStampPath) : null;
    const originalStamp = readOriginalStamp();
    const linkType = process.platform === "win32" ? "junction" : "dir";
    try {
      fs.mkdirSync(path.join(root, "dist"));
      for (const dir of ["bin", "mcp"]) {
        fs.cpSync(path.join(realDist, dir), path.join(root, "dist", dir), { recursive: true });
      }
      for (const entry of fs.readdirSync(realDist, { withFileTypes: true })) {
        // The sandbox starts unstamped even when the updater stamped the parent build.
        if (["bin", "mcp", "build-revision.json"].includes(entry.name)) continue;
        const from = path.join(realDist, entry.name);
        const to = path.join(root, "dist", entry.name);
        if (entry.isDirectory()) fs.symlinkSync(from, to, linkType);
        else fs.copyFileSync(from, to);
      }
      // Resolves `commander` and friends by the usual upward walk.
      fs.symlinkSync(path.resolve(__dirname, "../node_modules"), path.join(root, "node_modules"), linkType);
      fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ version: "1.6.0" }));

      const sandboxBin = path.join(root, "dist", "bin", "codegraph.js");
      const stamp = path.join(root, "dist", "build-revision.json");
      const version = (args: string[]) =>
        execFileSync(process.execPath, [sandboxBin, ...args], {
          encoding: "utf-8",
          env: { ...process.env, CODEGRAPH_NO_DAEMON: "1", CODEGRAPH_WASM_RELAUNCHED: "1" },
          stdio: ["ignore", "pipe", "pipe"],
        }).trim();

      // Every spelling has to agree — they are three separate call sites in
      // the CLI (the pre-parse intercept, commander's .version(), and the
      // `version` subcommand) and each one was printing the bare version.
      const spellings = ["version", "-v", "-version", "--version", "-V"];

      for (const spelling of spellings) expect(version([spelling])).toBe("1.6.0");

      const revision = "c".repeat(40);
      fs.writeFileSync(stamp, JSON.stringify({ revision }));
      for (const spelling of spellings) expect(version([spelling])).toBe(`1.6.0+${revision}`);

      // status --json is the machine-readable surface a deployment guard reads,
      // so it carries the revision too.
      expect(JSON.parse(version(["status", "--json", root])).version).toBe(`1.6.0+${revision}`);

      // A corrupt stamp reports the sentinel rather than a plausible-looking
      // version: the guard's revision matcher then fails loudly instead of
      // trusting a build that cannot identify itself.
      fs.writeFileSync(stamp, JSON.stringify({ revision: "not-a-commit" }));
      expect(version(["--version"])).toBe("0.0.0-unknown");

      // Preserve the parent build's stamp byte-for-byte, or its absence.
      expect(readOriginalStamp()).toEqual(originalStamp);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  for (const spelling of ["version", "-v", "-version", "--version", "-V"]) {
    it(`\`codegraph ${spelling}\` prints exactly the reported version`, () => {
      expect(run([spelling])).toBe(REPORTED_VERSION);
    });
  }

  it("lists the `version` subcommand in --help", () => {
    expect(run(["--help"])).toContain("version");
  });

  it("`codegraph help` prints usage and the command list", () => {
    const out = run(["help"]);
    expect(out).toContain("Usage: codegraph");
    expect(out).toContain("Commands:");
  });

  it("hides the internal `serve` command from --help", () => {
    // `serve --mcp` is the stdio entry point an AI agent launches for itself,
    // not a human command — it must not appear in the listing. (It stays fully
    // invocable; the mcp-initialize suite covers that the agent path works.)
    expect(run(["--help"])).not.toMatch(/^\s+serve\b/m);
  });

  it("a trailing `-v` is still the subcommand's --verbose, not the version intercept", () => {
    // A fresh temp dir outside any indexed project: `index -v` parses `-v` as
    // the index command's --verbose, then short-circuits at "not initialized"
    // and exits non-zero. The point is it must NOT print the bare version,
    // which would mean the top-level intercept swallowed a subcommand flag.
    const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "codegraph-version-test-"));
    let combined = "";
    try {
      combined = execFileSync(process.execPath, [BIN, "index", "-v", tempDir], {
        encoding: "utf-8",
        env: { ...process.env, CODEGRAPH_NO_DAEMON: "1", CODEGRAPH_WASM_RELAUNCHED: "1" },
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (err: unknown) {
      const e = err as { stdout?: string; stderr?: string };
      combined = `${e.stdout ?? ""}${e.stderr ?? ""}`;
    } finally {
      fs.rmSync(tempDir, { recursive: true, force: true });
    }
    expect(combined.trim()).not.toBe(REPORTED_VERSION);
    expect(combined).toContain("not initialized");
  });
});
