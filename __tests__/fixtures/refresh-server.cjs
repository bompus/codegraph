// Small stdio server for the launcher's process-lifecycle contract tests.
const fs = require("fs");
const path = require("path");
const readline = require("readline");
const dist = path.dirname(__dirname);
const { revision } = JSON.parse(fs.readFileSync(path.join(dist, "build-revision.json"), "utf8"));
const options = JSON.parse(fs.readFileSync(path.join(dist, "fixture.json"), "utf8"));
const send = (message) =>
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\n");
const result = (id, value) => send({ id, result: value });
let initialized = false;
let initializeParams;
let hostQuestion;
process.stderr.write(`FIXTURE_PID=${process.pid}\n`);
const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    initializeParams = message.params;
    if (options.exit) process.exit(7);
    if (options.hang) return;
    if (options.askDuringInitialize) send({ id: "preflight-question", method: "roots/list" });
    result(message.id, {
      protocolVersion: options.protocol || "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "codegraph", version: `1.6.0+${options.wrongRevision || revision}` },
      instructions: "fixture instructions",
    });
  } else if (message.method === "notifications/initialized") {
    initialized = true;
  } else if (message.method === "tools/list") {
    if (options.crashOnTools) process.exit(9);
    setTimeout(
      () =>
        result(message.id, {
          tools: [{ name: options.tool || "status", inputSchema: { type: "object" } }],
        }),
      options.toolsDelay || 0,
    );
  } else if (message.method === "tools/call") {
    const name = message.params.name;
    if (name === "crash") process.exit(9);
    const value = { revision, pid: process.pid, initialized, initializeParams };
    const reply = () =>
      result(message.id, {
        content: [{ type: "text", text: JSON.stringify(value) }],
        structuredContent: value,
      });
    if (name === "slow") {
      send({
        method: "notifications/progress",
        params: { progressToken: message.id, progress: 0 },
      });
      setTimeout(reply, 350);
    } else if (name === "arm-question") {
      reply();
      setTimeout(() => send({ id: "background-question", method: "roots/list" }), 150);
    } else if (name === "ask") {
      hostQuestion = { id: message.id, value };
      send({ id: "backend-question", method: "roots/list" });
    } else reply();
  } else if (message.id === "backend-question" && hostQuestion) {
    result(hostQuestion.id, {
      content: [],
      structuredContent: { ...hostQuestion.value, roots: message.result },
    });
    hostQuestion = null;
  } else if (message.id !== undefined && message.method) {
    result(message.id, {});
  }
});
lines.on("close", () => {
  if (options.ignoreClose) setInterval(() => {}, 1000);
  else process.exit(0);
});
