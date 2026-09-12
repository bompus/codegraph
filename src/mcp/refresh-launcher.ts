/**
 * Persistent, opt-in MCP launcher. Run the compiled file with a managed
 * deployment directory followed by the usual CodeGraph arguments. It preserves
 * host stdio while replacing compatible children at idle request boundaries.
 * Existing launch paths do not use this entry point.
 */
import { EARLY_PPID } from "./early-ppid";
import { spawn, ChildProcessWithoutNullStreams } from "child_process";
import { readFileSync } from "fs";
import { join, resolve } from "path";
import { createInterface } from "readline";
import { isDeepStrictEqual } from "util";
import { parseHostPpid, parsePpidPollMs, supervisionLostReason } from "./ppid-watchdog";
import { HOST_PPID_ENV } from "../extraction/node-runtime-flags";
import { WRITER_LOCK_DEFER_ENV } from "./writer-lock";
import { armStartupHandshakeTimeout } from "./startup-handshake";

type Id = string | number | null;
type Message = {
  jsonrpc?: string;
  id?: Id;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: unknown;
};
type Pending = { resolve(value: unknown): void; reject(error: Error): void; timer: NodeJS.Timeout };

function parse(line: string): Message | null {
  try {
    const value = JSON.parse(line);
    return value && typeof value === "object" && !Array.isArray(value) ? value : null;
  } catch {
    return null;
  }
}

function revisionAt(directory: string): string {
  const value = JSON.parse(readFileSync(join(directory, "dist", "build-revision.json"), "utf8"));
  if (!/^[a-f0-9]{40}$/.test(value.revision)) throw new Error("Missing valid deployment revision");
  return value.revision;
}

function contract(value: unknown): unknown {
  const result = value as { serverInfo?: Record<string, unknown> } | undefined;
  return result && { ...result, serverInfo: { ...result.serverInfo, version: undefined } };
}

class Backend {
  readonly child: ChildProcessWithoutNullStreams;
  private readonly pending = new Map<Id, Pending>();
  private serial = 0;
  private failure: Error | null = null;
  private stopping: Promise<void> | null = null;
  onMessage: (line: string, message: Message | null) => void = () => {};
  onFailure: (error: Error) => void = () => {};

  constructor(
    readonly revision: string,
    directory: string,
    args: string[],
  ) {
    this.child = spawn(
      process.execPath,
      [
        "--disable-warning=ExperimentalWarning",
        join(directory, "dist", "bin", "codegraph.js"),
        ...args,
      ],
      {
        stdio: ["pipe", "pipe", "pipe"],
        env: {
          ...process.env,
          [HOST_PPID_ENV]: String(parseHostPpid(process.env[HOST_PPID_ENV]) ?? EARLY_PPID),
          // This child may start while the one it replaces still serves, and
          // holds the writer lock. It succeeds that child rather than
          // competing with it (#1740).
          [WRITER_LOCK_DEFER_ENV]: "1",
        },
      },
    );
    this.child.stderr.pipe(process.stderr, { end: false });
    this.child.once("error", (error) => this.fail(error));
    this.child.stdin.on("error", (error) => this.fail(error));
    this.child.once("close", () => this.fail(new Error("CodeGraph child exited")));
    const lines = createInterface({ input: this.child.stdout });
    lines.on("line", (line) => {
      const message = parse(line);
      const waiting =
        message && message.method === undefined && message.id !== undefined
          ? this.pending.get(message.id)
          : undefined;
      if (waiting && message) {
        this.pending.delete(message.id!);
        clearTimeout(waiting.timer);
        if (message.error !== undefined) waiting.reject(new Error(JSON.stringify(message.error)));
        else waiting.resolve(message.result);
      } else {
        this.onMessage(line, message);
      }
    });
  }

  get alive(): boolean {
    return this.failure === null && !this.stopping;
  }

  fail(error: Error): void {
    if (this.failure) return;
    this.failure = error;
    for (const request of this.pending.values()) {
      clearTimeout(request.timer);
      request.reject(error);
    }
    this.pending.clear();
    this.onFailure(error);
  }

  send(line: string): void {
    if (!this.alive) throw this.failure ?? new Error("CodeGraph child is stopping");
    this.child.stdin.write(line + "\n");
  }

  request(method: string, params?: unknown): Promise<unknown> {
    return new Promise((resolveRequest, reject) => {
      const id = `refresh:${++this.serial}`;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Replacement ${method} timed out`));
      }, 5000);
      this.pending.set(id, { resolve: resolveRequest, reject, timer });
      try {
        this.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(error);
      }
    });
  }

  stop(): Promise<void> {
    if (this.stopping) return this.stopping;
    this.stopping = new Promise((done) => {
      if (this.child.exitCode !== null || this.child.signalCode !== null || !this.child.pid) {
        done();
        return;
      }
      const timer = setTimeout(() => this.child.kill("SIGKILL"), 1000);
      this.child.once("close", () => {
        clearTimeout(timer);
        done();
      });
      this.child.stdin.end();
    });
    return this.stopping;
  }
}

export async function runRefreshLauncher(directory: string, args: string[]): Promise<void> {
  const children = new Set<Backend>();
  const inflight = new Map<Id, string>();
  const serverRequests = new Set<Id>();
  let initialize: Message | null = null;
  let initialized: Message | null = null;
  let sessionResult: unknown;
  let toolsResult: unknown;
  let incompatibleRevision: string | null = null;
  let closing = false;
  const write = (line: string): void => {
    if (!closing) process.stdout.write(line + "\n");
  };
  const report = (message: string): void => {
    process.stderr.write(`[CodeGraph refresh] ${message}\n`);
  };
  const errorReply = (id: Id, message: string): void => {
    write(JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32603, message } }));
  };
  const create = (revision: string): Backend => {
    const backend = new Backend(revision, directory, args);
    children.add(backend);
    backend.child.once("close", () => children.delete(backend));
    return backend;
  };
  let active = create(revisionAt(directory));
  const attach = (backend: Backend): void => {
    backend.onMessage = (line, message) => {
      if (backend !== active) return;
      if (message?.id !== undefined) {
        if (message.method !== undefined) serverRequests.add(message.id);
        else {
          const method = inflight.get(message.id);
          inflight.delete(message.id);
          if (message.error === undefined) {
            if (method === "initialize") sessionResult = message.result;
            if (method === "tools/list") toolsResult = message.result;
          }
        }
      }
      write(line);
    };
    backend.onFailure = (error) => {
      if (backend !== active) return;
      for (const id of inflight.keys())
        errorReply(
          id,
          `${error.message}; ${sessionResult && toolsResult ? "request was not replayed" : "reconnect host to finish tool discovery"}`,
        );
      inflight.clear();
      serverRequests.clear();
    };
  };
  attach(active);

  const refresh = async (): Promise<void> => {
    if (
      !initialize ||
      !initialized ||
      !sessionResult ||
      !toolsResult ||
      inflight.size ||
      serverRequests.size
    )
      return;
    let revision: string;
    try {
      revision = revisionAt(directory);
    } catch (error) {
      report(`Keeping current child: ${String(error)}`);
      return;
    }
    if (active.alive && (revision === active.revision || revision === incompatibleRevision)) return;
    const candidate = create(revision);
    // No host messages have been routed here yet. A replacement requiring a
    // host interaction during preflight cannot be switched transparently.
    candidate.onMessage = (_line, message) => {
      if (message?.method && message.id !== undefined)
        candidate.fail(new Error("Replacement requires host interaction"));
    };
    try {
      const result = await candidate.request("initialize", initialize.params);
      const version = (result as { serverInfo?: { version?: string } })?.serverInfo?.version;
      if (typeof version !== "string" || !version.endsWith(`+${revision}`))
        throw new Error("Replacement build revision mismatch");
      if (!isDeepStrictEqual(contract(result), contract(sessionResult))) {
        incompatibleRevision = revision;
        throw new Error("Session contract changed; reconnect host");
      }
      candidate.send(JSON.stringify(initialized));
      const tools = await candidate.request("tools/list");
      if (!isDeepStrictEqual(tools, toolsResult)) {
        incompatibleRevision = revision;
        throw new Error("Tool definitions changed; reconnect host");
      }
      if (closing || inflight.size || serverRequests.size || !candidate.alive)
        throw new Error("Session no longer idle");
      if (revisionAt(directory) !== revision)
        throw new Error("Deployment changed during replacement startup");
      const previous = active;
      active = candidate;
      attach(active);
      incompatibleRevision = null;
      report(`Serving revision ${revision}`);
      await previous.stop();
    } catch (error) {
      report(`Keeping current child: ${String(error)}`);
      await candidate.stop();
    }
  };

  const dispatch = async (line: string): Promise<void> => {
    if (closing) return;
    const message = parse(line);
    if (message?.method === "tools/call") await refresh();
    if (closing) return;
    if (message?.method === "initialize") initialize = message;
    if (message?.method === "notifications/initialized" || message?.method === "initialized")
      initialized = message;
    if (message?.id !== undefined && message.method !== undefined)
      inflight.set(message.id, message.method);
    try {
      active.send(line);
    } catch (error) {
      if (message?.id !== undefined && message.method !== undefined) {
        inflight.delete(message.id);
        errorReply(
          message.id,
          `${String(error)}${sessionResult && toolsResult ? "" : "; reconnect host to finish tool discovery"}`,
        );
      }
    }
  };

  const lines = createInterface({ input: process.stdin });
  let queued = Promise.resolve();
  lines.on("line", (line) => {
    const message = parse(line);
    // A child can ask the host a question while a candidate is starting.
    // Its answer must not wait behind that preflight in the client queue.
    if (message?.id !== undefined && message.method === undefined) {
      if (serverRequests.delete(message.id)) {
        try {
          active.send(line);
        } catch {
          /* child failure already reported */
        }
      }
      return;
    }
    queued = queued.then(() => dispatch(line)).catch((error) => report(String(error)));
  });
  await new Promise<void>((done) => {
    let watchdog: NodeJS.Timeout | undefined;
    const shutdown = (): void => {
      if (closing) return;
      closing = true;
      clearInterval(watchdog);
      disarmStartup();
      lines.close();
      process.stdin.pause();
      for (const child of children) child.fail(new Error("Host disconnected"));
      void Promise.all([...children].map((child) => child.stop())).then(() => done());
    };
    const disarmStartup = armStartupHandshakeTimeout(shutdown);
    lines.once("close", shutdown);
    process.stdin.once("error", shutdown);
    process.stdout.once("error", shutdown);
    process.once("SIGTERM", shutdown);
    process.once("SIGINT", shutdown);
    const pollMs = parsePpidPollMs(process.env.CODEGRAPH_PPID_POLL_MS);
    if (pollMs > 0) {
      watchdog = setInterval(() => {
        const reason = supervisionLostReason({
          originalPpid: EARLY_PPID,
          currentPpid: process.ppid,
          hostPpid: parseHostPpid(process.env[HOST_PPID_ENV]),
          isAlive: (pid) => {
            try {
              process.kill(pid, 0);
              return true;
            } catch {
              return false;
            }
          },
        });
        if (reason) shutdown();
      }, pollMs);
      watchdog.unref();
    }
  });
  await queued;
}

if (require.main === module) {
  const [directory, ...args] = process.argv.slice(2);
  if (!directory) {
    process.stderr.write(
      "Usage: refresh-launcher <managed-deployment-directory> [CodeGraph arguments]\n",
    );
    process.exitCode = 1;
  } else {
    runRefreshLauncher(resolve(directory), args.length ? args : ["serve", "--mcp"]).catch(
      (error) => {
        process.stderr.write(`[CodeGraph refresh] ${String(error)}\n`);
        process.exitCode = 1;
      },
    );
  }
}
