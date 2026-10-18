import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { EventEmitter } from "node:events";
import { Readable } from "node:stream";
import { setImmediate as nextChunk } from "node:timers/promises";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Actual production modules; only HTTP dispatch is synthetic, with a real Node
// Readable and UTF-8 decoder. No listener, provider, authentication or network.
function host(chunks, { status = 200, iteratorError = false } = {}) {
  const response = Readable.from((async function* () {
    for (const chunk of chunks) { yield chunk; await nextChunk(); }
    if (iteratorError) throw new Error("fixture disconnect");
  })(), { objectMode: false });
  response.statusCode = status;
  response.headers = { "content-type": "text/event-stream" };
  const requests = [];
  let writes = 0;
  const modules = new Map();
  function load(relative) {
    if (modules.has(relative)) return modules.get(relative);
    const source = readFileSync(new URL(relative, import.meta.url), "utf8");
    const compiled = ts.transpileModule(source, {
      fileName: relative,
      compilerOptions: { target: ts.ScriptTarget.ES2023, module: ts.ModuleKind.CommonJS },
      reportDiagnostics: true,
    });
    assert.deepEqual(compiled.diagnostics, []);
    const exports = {};
    modules.set(relative, exports);
    vm.runInNewContext(compiled.outputText, {
      exports, URL, Buffer, AbortController, TextDecoder, setTimeout, clearTimeout,
      require(id) {
        if (id === "node:crypto") return { randomUUID: () => "fixture-request" };
        if (id === "node:http" || id === "node:https") return {
          request(url, options, callback) {
            requests.push({ url: String(url), options });
            const request = Object.assign(new EventEmitter(), {
              setTimeout() {}, write() { writes++; },
              end() { queueMicrotask(() => callback(response)); },
              destroy() { response.destroy(); request.emit("close"); },
            });
            return request;
          },
        };
        if (id === "../types/events.js") return load("../src/types/events.ts");
        if (id === "./problem.js") return load("../src/remote/problem.ts");
        if (id === "../version.js") return load("../src/version.ts");
        throw new Error(`Unexpected import: ${id}`);
      },
    });
    return exports;
  }
  const { GatewayClient } = load("../src/remote/client.ts");
  const client = new GatewayClient({ base_url: "http://127.0.0.1:1", api_key: "", tenant_id: "", stream_idle_timeout: 5 });
  return { response, requests, writes: () => writes, stream: (options = {}) => client.streamSSE("/api/v1/assistant/chat/stream", { message: "fixture" }, options) };
}
const textFrame = (text, nl = "\n") => `data: ${JSON.stringify({ event_type: "text_delta", data: text })}${nl}${nl}`;
const bytewise = bytes => [...bytes].map(byte => Buffer.from([byte]));
async function collect(chunks) {
  const h = host(chunks);
  const events = [];
  for await (const event of h.stream()) events.push(event);
  assert.equal(h.requests.length, 1);
  assert.equal(h.writes(), 1);
  assert.equal(h.response.destroyed, true);
  return events;
}
test("Chinese and emoji survive every individual byte and CRLF fragment", async () => {
  const events = await collect(bytewise(Buffer.from(textFrame("你好 🌍 café", "\r\n") + "data: [DONE]\r\n\r\n")));
  assert.deepEqual(events.map(e => e.eventType), ["text_delta", "done"]);
  assert.equal(events[0].data.content, "你好 🌍 café");
});
test("coalesced events and split SSE fields stop on terminal without replay", async () => {
  const bytes = Buffer.from(textFrame("一") + textFrame("🙂") + "data: [DONE]\n\n" + textFrame("ignored"));
  const events = await collect([bytes.subarray(0, 3), bytes.subarray(3)]);
  assert.deepEqual(events.map(e => e.eventType), ["text_delta", "text_delta", "done"]);
  assert.deepEqual(events.slice(0, 2).map(e => e.data.content), ["一", "🙂"]);
});
test("multiline and complete-data-line EOF handling remain unchanged", async () => {
  const events = await collect(bytewise(Buffer.from('data: {"event_type":"text_delta",\r\ndata: "data":"尾🙂"}\r\n')));
  assert.equal(events.length, 1);
  assert.equal(events[0].data.content, "尾🙂");
});
test("malformed UTF-8 retains replacement characters", async () => {
  const bytes = Buffer.concat([Buffer.from('data: {"event_type":"text_delta","data":"'), Buffer.from([0xc3, 0x28]), Buffer.from('"}\n\n')]);
  const events = await collect(bytewise(bytes));
  assert.equal(events[0].data.content, "�(");
});
test("EOF flush does not invent a frame from an unfinished data line", async () => {
  const bytes = Buffer.concat([Buffer.from(textFrame("kept") + "data: "), Buffer.from([0xf0, 0x9f])]);
  const events = await collect([bytes]);
  assert.equal(events.length, 1);
  assert.equal(events[0].data.content, "kept");
});
test("consumer break destroys the response without repeating admission", async () => {
  const h = host([Buffer.from(textFrame("你好")), Buffer.from(textFrame("later"))]);
  for await (const event of h.stream()) { assert.equal(event.data.content, "你好"); break; }
  assert.equal(h.response.destroyed, true);
  assert.equal(h.requests.length, 1);
  assert.equal(h.writes(), 1);
});
test("iterator failure destroys the response and remains failure without replay", async () => {
  const h = host([Buffer.from(textFrame("before"))], { iteratorError: true });
  const events = [];
  await assert.rejects(async () => { for await (const event of h.stream()) events.push(event); }, /fixture disconnect/);
  assert.equal(events.length, 1);
  assert.equal(h.response.destroyed, true);
  assert.equal(h.requests.length, 1);
});
test("HTTP error bodies preserve fragmented Unicode and flush incomplete UTF-8 at EOF", async () => {
  const bytes = Buffer.concat([Buffer.from("错误 🌍"), Buffer.from([0xf0, 0x9f])]);
  const h = host(bytewise(bytes), { status: 429 });
  await assert.rejects(async () => { for await (const event of h.stream()) assert.fail(event); }, error => {
    assert.equal(error.status, 429);
    assert.equal(error.body, "错误 🌍�");
    return true;
  });
  assert.equal(h.requests.length, 1);
  assert.equal(h.writes(), 1);
});
test("stopOnTerminal false retains the existing multi-terminal stream contract", async () => {
  const h = host([Buffer.from("data: [DONE]\n\n" + textFrame("after"))]);
  const events = [];
  for await (const event of h.stream({ stopOnTerminal: false })) events.push(event);
  assert.deepEqual(events.map(e => e.eventType), ["done", "text_delta"]);
  assert.equal(events[1].data.content, "after");
  assert.equal(h.requests.length, 1);
});
