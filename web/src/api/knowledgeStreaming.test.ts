import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import test from "node:test";

const apiUrl = new URL("./knowledge.ts", import.meta.url).href;
const transportUrl = new URL("../lib/sse.ts", import.meta.url).href;
const hooks = registerHooks({
  resolve(name, context, next) {
    if ([apiUrl, transportUrl].includes(context.parentURL ?? "")) {
      if (name === "@/lib/api") return { url: "data:text/javascript,export const api = {}; export const getAuthToken = () => null;", shortCircuit: true };
      if (name === "@/lib/sse") return next(transportUrl, context);
      if (name === "./sseEventParser") return next(new URL("../lib/sseEventParser.ts", apiUrl).href, context);
    }
    return next(name, context);
  },
});
const { streamDocumentProgress, qaQueryStream } = await import("./knowledge.ts");
hooks.deregister();

test("production Knowledge progress GET resumes Last-Event-ID and skips replayed document effects", async t => {
  t.mock.method(Math, "random", () => 0);
  const cursors: Array<string | null> = [];
  t.mock.method(globalThis, "fetch", async (_url, init: RequestInit) => {
    assert.equal(init.method, "GET"); cursors.push(new Headers(init.headers).get("Last-Event-ID"));
    assert.ok(cursors.length <= 2);
    return new Response(cursors.length === 1
      ? 'id: dataset-a:1\nevent: progress\ndata: {"document_id":"doc-1"}\n\n'
      : 'id: dataset-a:1\nevent: progress\ndata: {"document_id":"duplicate"}\n\nid: dataset-a:2\nevent: terminal\ndata: {"document_id":"doc-1","terminal":true}\n\n');
  });
  const stream = streamDocumentProgress("dataset-a");
  assert.equal((await stream.next()).value?.id, "dataset-a:1");
  assert.equal((await stream.next()).value?.id, "dataset-a:2");
  await stream.return();
  assert.deepEqual(cursors, [null, "dataset-a:1"]);
});

test("production QA POST is never repeated after a partial answer and network interruption", async t => {
  let requests = 0; let body!: ReadableStreamDefaultController<Uint8Array>;
  t.mock.method(globalThis, "fetch", async (_url, init: RequestInit) => {
    requests++; assert.equal(init.method, "POST");
    return new Response(new ReadableStream({ start(controller) { body = controller; controller.enqueue(new TextEncoder().encode('data: {"event":"delta","data":{"content":"partial"}}\n\n')); } }));
  });
  const stream = qaQueryStream("dataset-a", { query: "question" });
  assert.equal((await stream.next()).value?.event, "delta");
  body.error(new TypeError("network disconnected"));
  await assert.rejects(stream.next(), /server outcome is unknown/);
  assert.equal(requests, 1);
});


test("production Knowledge feed preserves an explicit reset even when its cursor was seen", async t => {
  t.mock.method(globalThis, "fetch", async () => new Response('id: dataset-a:1\nevent: progress\ndata: {"document_id":"doc-1"}\n\nid: dataset-a:2\nevent: progress\ndata: {"document_id":"doc-2"}\n\nid: dataset-a:1\nevent: reset\ndata: {}\n\n'));
  const stream = streamDocumentProgress("dataset-a");
  assert.equal((await stream.next()).value?.event, "progress");
  assert.equal((await stream.next()).value?.event, "progress");
  assert.equal((await stream.next()).value?.event, "reset");
  await stream.return();
});
