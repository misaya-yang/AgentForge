import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Execute the complete production module, exposing only its private command
// handler for this test. UI/other-command imports stay inert; no server is bound.
function loadHandler() {
  const source = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const modules = new Set([
    "react", "react/jsx-runtime", "ink", "ink-text-input", "./types/events.js",
    "./remote/client.js", "./remote/session_client.js", "./remote/chat.js", "./agent/slash-commands.js",
    "./memory.js", "./mcp/manager.js", "./skills.js", "./project-memory.js",
    "./agents.js", "node:fs", "./version.js",
  ]);
  const compiled = ts.transpileModule(`${source}\nexport { handleAsyncCommand };`, {
    fileName: "app.tsx",
    compilerOptions: {
      target: ts.ScriptTarget.ES2023,
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
      esModuleInterop: true,
    },
    reportDiagnostics: true,
  });
  assert.deepEqual(compiled.diagnostics, []);
  const exports = {};
  vm.runInNewContext(compiled.outputText, {
    exports,
    require(id) {
      assert.ok(modules.has(id), `Unexpected production dependency: ${id}`);
      return Object.freeze({});
    },
  });
  return exports.handleAsyncCommand;
}
const handle = loadHandler();
const ID = "93675f88-dab2-4496-bcde-b3c3460392d6";
const message = (content, role = "assistant") => ({ role, content });

function host(respond) {
  const requests = [];
  const notices = [];
  let session = "previous-session";
  let messages = [message("Previous conversation", "user")];
  let sessionUpdates = 0;
  let messageUpdates = 0;
  return {
    requests, notices,
    get session() { return session; },
    get messages() { return JSON.parse(JSON.stringify(messages)); },
    get updates() { return [sessionUpdates, messageUpdates]; },
    command(result) {
      const request = async (...args) => { requests.push(args); return respond(...args); };
      return handle(result, { request }, {
        list: () => request("GET", "/api/v1/assistant/sessions?limit=20"),
        history: (id) => request("GET", `/api/v1/assistant/sessions/${encodeURIComponent(id)}/history`),
      }, {}, (notice) => notices.push(notice), (id) => { session = id; sessionUpdates++; },
      (value) => { messages = value; messageUpdates++; }, () => {});
    },
    resume(id = ID) { return this.command({ action: "resume", resumeSessionId: id }); },
  };
}
function historyHost(data, id = ID) {
  return host((method, path) => {
    assert.equal(method, "GET");
    assert.equal(path, `/api/v1/assistant/sessions/${encodeURIComponent(id)}/history`);
    return data;
  });
}
function unchanged(h) {
  assert.equal(h.session, "previous-session");
  assert.deepEqual(h.messages, [message("Previous conversation", "user")]);
  assert.deepEqual(h.updates, [0, 0]);
  assert.equal(h.requests.length, 1);
}

test("Gateway history displays complete copyable IDs for resume", async () => {
  const other = "93675f88-dab2-4496-bcde-b3c3460392d7";
  const h = host((method, path) => {
    assert.equal(method, "GET");
    assert.equal(path, "/api/v1/assistant/sessions?limit=20");
    return { sessions: [
      { session_id: ID, metadata: { title: "First" }, updated_at: "2026-10-12T11:00:00Z" },
      { session_id: other, metadata: { title: "Second" }, updated_at: "2026-10-11T11:00:00Z" },
    ], total: 2 };
  });
  await h.command({ action: "history" });
  assert.ok(h.notices[0].includes(ID));
  assert.ok(h.notices[0].includes(other));
  assert.ok(h.notices[0].includes("/resume <session_id>"));
  unchanged(h);
});

test("Gateway resume reads canonical history once before selecting the session", async () => {
  const rows = [message("Question", "user"), message("Answer")];
  const h = historyHost({ session_id: ID, messages: rows, total: 2 });
  await h.resume();
  assert.deepEqual(h.requests, [["GET", `/api/v1/assistant/sessions/${ID}/history`]]);
  assert.equal(h.session, ID);
  assert.deepEqual(h.messages.slice(1), rows);
  assert.deepEqual(h.updates, [1, 1]);
});

test("Gateway resume encodes an opaque session ID as one path component", async () => {
  const id = "session/part?limit=500#fragment";
  const h = historyHost({ session_id: id, messages: [], total: 0 }, id);
  await h.resume(id);
  assert.equal(h.session, id);
  assert.equal(h.requests.length, 1);
});

test("Gateway resume preserves chronological latest-20 UI bound and empty redacted text", async () => {
  const rows = Array.from({ length: 25 }, (_, index) => message(index === 24 ? "" : `row-${index}`, index % 2 ? "assistant" : "user"));
  const h = historyHost({ session_id: ID, messages: rows, total: 80 });
  await h.resume();
  assert.deepEqual(h.messages.slice(1), rows.slice(-20));
  assert.equal(h.messages.at(-1).content, "");
});

test("Gateway resume accepts empty history", async () => {
  const h = historyHost({ session_id: ID, messages: [], total: 0 });
  await h.resume();
  assert.equal(h.session, ID);
  assert.equal(h.messages.length, 1);
  assert.equal(h.messages[0].role, "system");
});

test("Gateway resume rejects mismatched or malformed history without replacing the current conversation", async () => {
  for (const data of [
    { session_id: "other-session", messages: [message("Wrong owner content")] },
    null, [], {}, { session_id: ID, messages: {} },
    { session_id: ID, messages: [null] },
    { session_id: ID, messages: [{ role: "assistant", content: {} }] },
    { session_id: ID, messages: [{ content: "Missing role" }] },
  ]) {
    const h = host(() => data);
    await assert.rejects(h.resume(), /Invalid session history response/);
    unchanged(h);
  }
});

for (const reason of ["HTTP 401", "HTTP 404", "HTTP 503", "Request aborted"]) {
  test(`Gateway resume preserves the current conversation after ${reason} without retry`, async () => {
    const h = host(() => { throw new Error(reason); });
    await assert.rejects(h.resume(), { message: reason });
    unchanged(h);
    assert.equal(h.requests[0][0], "GET");
  });
}
