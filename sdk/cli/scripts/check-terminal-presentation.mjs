import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Run production parser, chat request builder and entire App submit handler.
// Hook doubles make state observable; no Ink rendering, network or Runtime runs.
function load(relative, dependencies = {}) {
  const source = readFileSync(new URL(`../src/${relative}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, {
    fileName: relative,
    compilerOptions: { target: ts.ScriptTarget.ES2023, module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true },
    reportDiagnostics: true,
  });
  assert.deepEqual(compiled.diagnostics, []);
  const exports = {};
  vm.runInNewContext(compiled.outputText, {
    exports, process: { env: {} },
    require(id) {
      assert.ok(Object.hasOwn(dependencies, id), `Unexpected dependency: ${id}`);
      return dependencies[id];
    },
  });
  return exports;
}
const events = load("types/events.ts");
const problem=load("remote/problem.ts");
const transport = load("remote/client.ts", {
  "./problem.js":problem,
  "node:crypto": { randomUUID }, "node:http": {}, "node:https": {},
  "../types/events.js": events, "../version.js": { CLI_VERSION: "fixture" },
});
const chat = load("remote/chat.ts");
function host(frames, deferred = false) {
  const states = [], callbacks = [], requests = [], updates = [];
  function flush() { while (updates.length) updates.shift()(); }
  const react = {
    useState(initial) {
      const index = states.push(initial) - 1;
      return [initial, (next) => {
        const update = () => { states[index] = typeof next === "function" ? next(states[index]) : next; };
        if (deferred) updates.push(update); else update();
      }];
    },
    useCallback(callback) { callbacks.push(callback); return callback; },
    useRef(current) { return { current }; },
    useMemo(factory) { return factory(); },
  };
  const dependencies = {
    react,
    "react/jsx-runtime": { jsx: () => null, jsxs: () => null },
    ink: { useApp: () => ({ exit() { throw new Error("Unexpected exit"); } }), useInput() {} },
    "ink-text-input": {},
    "./types/events.js": events,
    "./remote/client.js": { GatewayClient: class {
      async *streamSSE(path, body) {
        requests.push({ path, body });
        for (const frame of frames) {
          const event = transport.parseSSEEvent(null, [JSON.stringify(frame)]);
          yield event;
          if (transport.isTerminalEvent(event)) break;
        }
      }
    } },
    "./remote/session_client.js": { GatewaySessionClient: class {} },
    "./remote/chat.js": chat,
    "./agent/slash-commands.js": {}, "./memory.js": {},
    "./mcp/manager.js": { MCPManager: class { servers = []; } },
    "./skills.js": {}, "./project-memory.js": { loadProjectMemory: () => "" },
    "./agents.js": {}, "node:fs": {}, "./version.js": { CLI_VERSION: "fixture" },
  };
  load("app.tsx", dependencies).App({ config: { base_url: "https://gateway.example", timeout: 30 } });
  return {
    requests,
    submit: callbacks[2],
    get messages() { flush(); return JSON.parse(JSON.stringify(states[0])); },
    get busy() { flush(); return states[2]; },
  };
}
function frame(event_type, data = {}) { return { event_type, data }; }
for (const prefix of ["", "Partial answer"]) {
  for (const [event, data, expected] of [
    ["run_error", { message: "Run failed" }, /Error: Run failed/],
    ["error", { error: { message: "Provider failed" } }, /Error: Provider failed/],
    ["cancelled", { reason: "user_stop" }, /Cancelled: user_stop/],
  ]) {
    test(`${event} is visible after ${prefix ? "partial text" : "no text"}`, async () => {
      const h = host([
        ...(prefix ? [frame("text_delta", prefix)] : []),
        frame(event, data), frame("text_delta", "Must not appear"),
      ]);
      await h.submit("Question");
      assert.equal(h.requests.length, 1);
      assert.equal(h.requests[0].path, "/api/v1/assistant/chat/stream");
      assert.equal(h.messages.length, 2);
      assert.match(h.messages[1].content, expected);
      assert.ok(h.messages[1].content.startsWith(prefix));
      assert.doesNotMatch(h.messages[1].content, /No response received|\[object Object\]|Must not appear/);
      assert.equal(h.busy, false);
    });
  }
}
for (const [event, expected] of [["error", /Error: Unknown/], ["run_error", /Error: Unknown/], ["cancelled", /Cancelled\./]]) {
  test(`${event} without details still has an explicit outcome`, async () => {
    const h = host([frame(event)]);
    await h.submit("Question");
    assert.match(h.messages[1].content, expected);
    assert.equal(h.requests.length, 1);
  });
}
test("normal text completion is unchanged", async () => {
  const h = host([frame("text_delta", "Answer"), frame("done")]);
  await h.submit("Question");
  assert.deepEqual(h.messages, [{ role: "user", content: "Question" }, { role: "assistant", content: "Answer" }]);
});
test("nonterminal empty stream retains the existing diagnostic", async () => {
  const h = host([]);
  await h.submit("Question");
  assert.match(h.messages[1].content, /No response received/);
});

for (const deferred of [false, true]) {
  for (const prefix of ["", "Partial answer"]) {
    for (const [event, data, expected] of [
      ["run_error", { status: "cancelled" }, /Cancelled\./],
      ["run_error", { status: "cancelled", terminal_reason: "interrupted" }, /Cancelled: interrupted/],
      ["run_error", { message: "Run failed" }, /Error: Run failed/],
      ["cancelled", { reason: "user_stop" }, /Cancelled: user_stop/],
    ]) {
      test(`${event} ${JSON.stringify(data)} with ${deferred ? "deferred" : "immediate"} updates and ${prefix ? "partial text" : "no text"}`, async () => {
        const h = host([...(prefix ? [frame("text_delta", prefix)] : []), frame(event, data)], deferred);
        await h.submit("Question");
        assert.equal(h.messages.length, 2);
        assert.ok(h.messages[1].content.startsWith(prefix));
        assert.match(h.messages[1].content, expected);
        assert.doesNotMatch(h.messages[1].content, /No response received|Error: Unknown/);
        assert.equal(h.requests.length, 1);
        assert.equal(h.busy, false);
      });
    }
    for (const event of ["done", "run_finished"]) {
      test(`${event} with ${deferred ? "deferred" : "immediate"} updates and ${prefix ? "text" : "empty success"}`, async () => {
        const h = host([...(prefix ? [frame("text_delta", prefix)] : []), frame(event, { status: "succeeded" })], deferred);
        await h.submit("Question");
        assert.equal(h.messages[1].content, prefix || "Completed without text output.");
        assert.equal(h.requests.length, 1);
        assert.equal(h.busy, false);
      });
    }
  }
  test(`empty success state is reset before next unterminated stream (${deferred})`, async () => {
    const frames = [frame("done")];
    const h = host(frames, deferred);
    await h.submit("First");
    assert.equal(h.messages[1].content, "Completed without text output.");
    frames.length = 0;
    await h.submit("Second");
    assert.match(h.messages[3].content, /No response received/);
    assert.equal(h.requests.length, 2);
  });
}
