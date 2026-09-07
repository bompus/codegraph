import { afterEach, describe, expect, it } from "vitest";
import { spawn, ChildProcessWithoutNullStreams } from "child_process";
import {
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "fs";
import { tmpdir } from "os";
import { join, resolve } from "path";
import { createInterface } from "readline";
import { rmTempDir } from "./rm-temp";

const launcher = resolve(__dirname, "../dist/mcp/refresh-launcher.js");
const fixture = resolve(__dirname, "fixtures/refresh-server.cjs");
const A = "a".repeat(40);
const B = "b".repeat(40);
type Message = {
  id?: string | number;
  method?: string;
  result?: any;
  error?: { message: string };
  params?: any;
};
const cleanups: Array<() => Promise<void>> = [];

async function waitFor<T>(
  read: () => T | undefined,
  diagnostic = "condition",
  timeout = 9000,
): Promise<T> {
  const start = Date.now();
  while (Date.now() - start < timeout) {
    const value = read();
    if (value !== undefined) return value;
    await new Promise((done) => setTimeout(done, 15));
  }
  throw new Error(`Timed out waiting for ${diagnostic}`);
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function exited(child: ChildProcessWithoutNullStreams): Promise<void> {
  await waitFor(
    () => (child.exitCode !== null || child.signalCode !== null ? true : undefined),
    "process exit",
  );
}

async function start({
  realCodeGraph = false,
  options = {},
  retainedPipes = false,
  daemon = false,
  initialize = true,
  env = {},
}: {
  realCodeGraph?: boolean;
  options?: Record<string, unknown>;
  retainedPipes?: boolean;
  daemon?: boolean;
  initialize?: boolean;
  env?: Record<string, string>;
} = {}) {
  const directory = mkdtempSync(join(tmpdir(), "codegraph-refresh-"));
  let cwd = directory;
  mkdirSync(join(directory, "dist", "bin"), { recursive: true });
  if (realCodeGraph) {
    const root = resolve(__dirname, "..");
    cpSync(join(root, "dist"), join(directory, "dist"), { recursive: true });
    cpSync(join(root, "src", "extraction", "wasm"), join(directory, "dist", "extraction", "wasm"), {
      recursive: true,
    });
    copyFileSync(
      join(root, "src", "db", "schema.sql"),
      join(directory, "dist", "db", "schema.sql"),
    );
    copyFileSync(join(root, "package.json"), join(directory, "package.json"));
    symlinkSync(join(root, "node_modules"), join(directory, "node_modules"), "junction");
    cwd = join(directory, "project");
    mkdirSync(cwd);
    writeFileSync(join(cwd, "proof.js"), "export function refreshedProof() { return 42; }\n");
    const { CodeGraph } = await import("../src");
    const cg = await CodeGraph.init(cwd);
    await cg.indexAll();
    cg.close();
  } else copyFileSync(fixture, join(directory, "dist", "bin", "codegraph.js"));
  const deploy = (revision: string, options: Record<string, unknown> = {}): void => {
    writeFileSync(join(directory, "dist", "fixture.json"), JSON.stringify(options));
    writeFileSync(join(directory, "dist", "build-revision.json"), JSON.stringify({ revision }));
  };
  deploy(A, options);
  const parentProgram = `
    const { spawn } = require("child_process");
    const child = spawn(process.execPath, process.argv.slice(1), { stdio: "inherit" });
    process.stderr.write("LAUNCHER_PID=" + child.pid + "\\n");
    process.stdin.on("end", () => process.exit(0));
    setInterval(() => {}, 1000);
  `;
  const command = retainedPipes
    ? ["-e", parentProgram, launcher, directory]
    : [launcher, directory];
  const child = spawn(process.execPath, command, {
    cwd,
    stdio: ["pipe", "pipe", "pipe"],
    env: {
      ...process.env,
      CODEGRAPH_TELEMETRY: "0",
      CODEGRAPH_NO_UPDATE_CHECK: "1",
      CODEGRAPH_NO_DAEMON: daemon ? "" : "1",
      CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS: "100",
      CODEGRAPH_HOST_PPID: "",
      CODEGRAPH_PPID_POLL_MS: "50",
      ...env,
    },
  });
  const messages: Message[] = [];
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });
  child.stdin.on("error", () => {});
  const lines = createInterface({ input: child.stdout });
  lines.on("line", (line) => messages.push(JSON.parse(line)));
  const send = (message: Message): void => {
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\n");
  };
  const response = (id: string | number) =>
    waitFor(
      () => messages.find((message) => message.id === id && !message.method),
      `response ${id}: ${stderr}`,
    );
  const call = async (id: string | number, name = "status") => {
    send({ id, method: "tools/call", params: { name, arguments: {} } });
    return response(id);
  };
  cleanups.push(async () => {
    child.stdin.end();
    try {
      await exited(child);
    } finally {
      if (child.exitCode === null && child.signalCode === null) child.kill();
      const pids = [...stderr.matchAll(/(?:FIXTURE|LAUNCHER)_PID=(\d+)/g)].map((match) =>
        Number(match[1]),
      );
      const log = join(cwd, ".codegraph", "daemon.log");
      if (daemon && existsSync(log)) {
        for (const match of readFileSync(log, "utf8").matchAll(/Listening on .*?\(pid (\d+)/g))
          pids.push(Number(match[1]));
      }
      for (const pid of pids) if (alive(pid)) process.kill(pid);
      lines.close();
      await rmTempDir(directory);
    }
  });
  if (initialize) {
    send({
      id: "init",
      method: "initialize",
      params: {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "refresh-proof", version: "1" },
      },
    });
    expect((await response("init")).result.serverInfo.version).toBe(`1.6.0+${A}`);
    send({ method: "notifications/initialized" });
    send({ id: "tools", method: "tools/list" });
    await response("tools");
  }
  return { child, directory, deploy, messages, send, response, call, stderr: () => stderr };
}

afterEach(async () => {
  while (cleanups.length) await cleanups.pop()!();
});

describe("isolated MCP refresh launcher", () => {
  it.each([false, true])(
    "serves real CodeGraph after replacement without reconnecting (daemon=%s)",
    async (daemon) => {
      const server = await start({ realCodeGraph: true, daemon });
      const before = await server.call("before", "codegraph_status");
      expect(before.result.isError).not.toBe(true);
      expect(before.result.content[0].text).toContain(`1.6.0+${A}`);
      server.deploy(B);
      const after = await server.call("after", "codegraph_status");
      expect(after.result.isError).not.toBe(true);
      expect(after.result.content[0].text, server.stderr()).toContain(`1.6.0+${B}`);
      server.send({
        id: "explore",
        method: "tools/call",
        params: { name: "codegraph_explore", arguments: { query: "refreshedProof", maxFiles: 1 } },
      });
      const explored = await server.response("explore");
      expect(explored.result.isError).not.toBe(true);
      expect(explored.result.content[0].text).toContain("return 42");
      expect(server.messages.filter((message) => message.id === "init")).toHaveLength(1);
      expect(server.child.exitCode).toBeNull();
    },
    30000,
  );

  it("replaces the child on the same client connection, preserving IDs and initialization", async () => {
    const server = await start();
    const before = (await server.call(1)).result.structuredContent;
    server.deploy(B);
    const after = (await server.call("refresh:1")).result.structuredContent;
    expect(before.revision).toBe(A);
    expect(after.revision).toBe(B);
    expect(after.pid).not.toBe(before.pid);
    expect(after.initialized).toBe(true);
    expect(after.initializeParams).toEqual(before.initializeParams);
    expect(server.messages.filter((message) => message.id === "init")).toHaveLength(1);
    expect(server.messages.filter((message) => message.id === "refresh:1")).toHaveLength(1);
    expect(alive(before.pid)).toBe(false);
    expect(server.child.exitCode).toBeNull();
  });

  it.each([
    ["tools change", { tool: "different" }, "Tool definitions changed"],
    ["protocol change", { protocol: "different" }, "Session contract changed"],
    ["wrong build", { wrongRevision: A }, "build revision mismatch"],
    ["startup crash", { exit: true }, "child exited"],
    [
      "host interaction during preflight",
      { askDuringInitialize: true },
      "requires host interaction",
    ],
  ])("keeps the old child when replacement fails: %s", async (_name, options, reason) => {
    const server = await start();
    const before = (await server.call(1)).result.structuredContent;
    server.deploy(B, options);
    const after = (await server.call(2)).result.structuredContent;
    expect(after.pid).toBe(before.pid);
    expect(after.revision).toBe(A);
    expect(server.stderr()).toContain(reason);
    const launches = server.stderr().match(/FIXTURE_PID=/g)?.length;
    await server.call(3);
    expect(server.stderr().match(/FIXTURE_PID=/g)?.length).toBe(
      reason.includes("changed") ? launches : launches! + 1,
    );
  });

  it("bounds replacement handshake time and continues with the old child", async () => {
    const server = await start();
    server.deploy(B, { hang: true });
    expect((await server.call(2)).result.structuredContent.revision).toBe(A);
    expect(server.stderr()).toContain("initialize timed out");
  }, 12000);

  it("retries a revision after transient replacement startup failure", async () => {
    const server = await start();
    server.deploy(B, { exit: true });
    expect((await server.call(1)).result.structuredContent.revision).toBe(A);
    server.deploy(B);
    expect((await server.call(2)).result.structuredContent.revision).toBe(B);
  });

  it("retries after the old child becomes busy during candidate preflight", async () => {
    const server = await start();
    await server.call("arm", "arm-question");
    server.deploy(B, { toolsDelay: 350 });
    server.send({ id: "during", method: "tools/call", params: { name: "status" } });
    await waitFor(() => server.messages.find((message) => message.id === "background-question"));
    expect((await server.response("during")).result.structuredContent.revision).toBe(A);
    server.send({ id: "background-question", result: { roots: [] } });
    expect((await server.call("after")).result.structuredContent.revision).toBe(B);
  });

  it("rejects a deployment changed during preflight and retries its final revision", async () => {
    const server = await start();
    server.deploy(B, { toolsDelay: 350 });
    server.send({ id: 1, method: "tools/call", params: { name: "status" } });
    await waitFor(() => (server.stderr().match(/FIXTURE_PID=/g)?.length === 2 ? true : undefined));
    const C = "c".repeat(40);
    server.deploy(C);
    expect((await server.response(1)).result.structuredContent.revision).toBe(A);
    expect((await server.call(2)).result.structuredContent.revision).toBe(C);
  });

  it("requires explicit reconnect after a crash before tool discovery finishes", async () => {
    const server = await start({ options: { crashOnTools: true } });
    expect((await server.response("tools")).error?.message).toContain("reconnect host");
    expect((await server.call(1)).error?.message).toContain("reconnect host");
  });

  it("reaps the launcher and child after host death even with retained pipes", async () => {
    const server = await start({ retainedPipes: true });
    const pid = (await server.call(1)).result.structuredContent.pid;
    const launcherPid = Number(server.stderr().match(/LAUNCHER_PID=(\d+)/)?.[1]);
    expect(alive(launcherPid)).toBe(true);
    server.child.kill("SIGKILL");
    await waitFor(() => (!alive(pid) && !alive(launcherPid) ? true : undefined), "orphan cleanup");
  });

  it("reaps a never-initialized launcher when retained pipes and a blind watchdog cannot", async () => {
    const server = await start({
      initialize: false,
      env: { CODEGRAPH_PPID_POLL_MS: "0", CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS: "500" },
    });
    const pid = await waitFor(() => {
      const match = server.stderr().match(/FIXTURE_PID=(\d+)/);
      return match ? Number(match[1]) : undefined;
    });
    await exited(server.child);
    expect(server.child.exitCode).toBe(0);
    expect(alive(pid)).toBe(false);
  });

  it("keeps an initialized idle session alive beyond the startup timeout", async () => {
    const server = await start({
      env: { CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS: "500" },
    });
    await new Promise((done) => setTimeout(done, 700));
    expect((await server.call(1)).result.structuredContent.revision).toBe(A);
  });

  it("bounds child shutdown even when the child ignores EOF", async () => {
    const server = await start({ options: { ignoreClose: true } });
    const pid = (await server.call(1)).result.structuredContent.pid;
    server.child.stdin.end();
    await exited(server.child);
    expect(alive(pid)).toBe(false);
  });

  it("keeps serving if the revision marker is missing during deployment", async () => {
    const server = await start();
    rmSync(join(server.directory, "dist", "build-revision.json"));
    expect((await server.call(2)).result.structuredContent.revision).toBe(A);
    server.deploy(B);
    expect((await server.call(3)).result.structuredContent.revision).toBe(B);
  });

  it("finishes busy work on the old child before switching at a later request", async () => {
    const server = await start();
    server.send({ id: "slow", method: "tools/call", params: { name: "slow" } });
    await waitFor(() =>
      server.messages.find((message) => message.method === "notifications/progress"),
    );
    server.deploy(B);
    expect((await server.call("concurrent")).result.structuredContent.revision).toBe(A);
    expect((await server.response("slow")).result.structuredContent.revision).toBe(A);
    expect((await server.call("after")).result.structuredContent.revision).toBe(B);
  });

  it("relays server requests and host responses, then refreshes after they finish", async () => {
    const server = await start();
    server.send({ id: 1, method: "tools/call", params: { name: "ask" } });
    await waitFor(() => server.messages.find((message) => message.id === "backend-question"));
    server.deploy(B);
    server.send({ id: "backend-question", result: { roots: [] } });
    expect((await server.response(1)).result.structuredContent.roots).toEqual({ roots: [] });
    expect((await server.call(2)).result.structuredContent.revision).toBe(B);
  });

  it("reports interrupted calls without replay and restarts for the next request", async () => {
    const server = await start();
    const interrupted = await server.call(1, "crash");
    expect(interrupted.error?.message).toContain("not replayed");
    const recovered = await server.call(2);
    expect(recovered.result.structuredContent.revision).toBe(A);
    expect(server.messages.filter((message) => message.id === 1)).toHaveLength(1);
  });

  it("closes both children and queued requests when the host disconnects during preflight", async () => {
    const server = await start();
    server.deploy(B, { hang: true });
    server.send({ id: 1, method: "tools/call", params: { name: "status" } });
    server.send({ id: 2, method: "tools/call", params: { name: "status" } });
    await waitFor(() => (server.stderr().match(/FIXTURE_PID=/g)?.length === 2 ? true : undefined));
    server.child.stdin.end();
    await exited(server.child);
    const pids = [...server.stderr().matchAll(/FIXTURE_PID=(\d+)/g)].map((match) =>
      Number(match[1]),
    );
    expect(pids).toHaveLength(2);
    expect(pids.every((pid) => !alive(pid))).toBe(true);
  });
});
