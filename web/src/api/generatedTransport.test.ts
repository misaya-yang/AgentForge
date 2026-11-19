import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import type * as Transport from "./generatedTransport.ts";
function fixture() {
  const calls: Record<string, unknown>[] = [];
  let failure: unknown;
  const events = [{ event: "reset", id: "cursor", data: { sequence: 1 } }, { event: "done", data: { done: true } }];
  const exports: Record<string, unknown> = {};
  const generated: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../../../sdk/typescript/generated.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText, { exports: generated });
  const modules: Record<string, unknown> = {
    "../../../sdk/typescript/generated": generated,
    "@tanstack/react-query": { queryOptions: (value: unknown) => value },
    "@/lib/api": { api: { get: async (url: string, options: object) => { calls.push({ url, ...options }); if (failure) throw failure; return { data: new Blob(["wire bytes"]) }; }, request: async (input: Record<string, unknown>) => { calls.push(input); if (failure) throw failure; return { data: { receipt: "server" } }; } } },
    "@/lib/sse": { async *sseFetchEvents(url: string, options: Record<string, unknown>) { calls.push({ url, ...options }); yield* events; } },
  };
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./generatedTransport.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, {
    exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, Headers, URLSearchParams, Error, Blob, FormData,
  });
  return { transport: exports as typeof Transport, calls, events, fail(value: unknown) { failure = value; } };
}
test("generated SDK delegates one encoded request to the existing BFF authority and preserves errors and cancellation", async () => {
  const h = fixture(), controller = new AbortController();
  const body = { display_name: "Reviewed" };
  await h.transport.request("PUT /api/v1/providers/{provider_id}", { path: { provider_id: "provider/a ?" }, body }, controller.signal);
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].url, "/api/v1/providers/provider%2Fa%20%3F");
  assert.equal(h.calls[0].data, body); assert.equal(h.calls[0].signal, controller.signal);
  assert.deepEqual(Object.keys(h.calls[0].headers as object), []);
  const error = new Error("unconfirmed write"); h.fail(error);
  await assert.rejects(h.transport.request("DELETE /api/v1/providers/{provider_id}", { path: { provider_id: "p" } }), e => e === error);
  assert.equal(h.calls.length, 2);
});
test("generated operation inputs cannot replace browser authority or omit required path values", () => {
  const { transport } = fixture();
  for (const header of ["Authorization", "authorization", "Origin", "Sec-Fetch-Site", "Cookie", "X-CSRF-Token"]) {
    assert.throws(() => transport.operationRequest("GET /api/v1/providers", { header: { [header]: "injected" } }), /browser authority/);
  }
  assert.throws(() => transport.operationRequest("GET /api/v1/providers/{provider_id}", {}), /path parameter/);
  assert.throws(() => transport.operationRequest("GET https://other.invalid/api/", {}), /same-origin/);
});
test("generated read factories isolate identity and pass the query abort signal", async () => {
  const { transport, calls } = fixture();
  const a = transport.readOptions("tenant-a:user-a:1", "GET /api/v1/providers", {});
  const b = transport.readOptions("tenant-a:user-a:2", "GET /api/v1/providers", {});
  assert.notDeepEqual(a.queryKey, b.queryKey); assert.equal(a.retry, false); assert.equal(a.gcTime, 0);
  const controller = new AbortController();
  await (a.queryFn as (context: { signal: AbortSignal }) => unknown)({ signal: controller.signal });
  assert.equal(calls[0].signal, controller.signal);
});
test("typed stream wrapper retains reset, cursor and terminal control frames through the shared reader", async () => {
  const h = fixture();
  // Exercise the generic wire layer with the actual generated SDK. Protocol event
  // variants remain owned by the producer rather than redeclared in the console.
  const stream = h.transport.streamEvents as (key: string, input: unknown, options: object) => AsyncIterable<unknown>;
  const actual = []; for await (const event of stream("GET /api/v2/agent/threads/{thread}/events", { path: { thread: "t" }, query: { after_sequence: 3 } }, {})) actual.push(event);
  assert.deepEqual(actual, h.events); assert.equal(h.calls[0].preserveControlFrames, true);
  assert.equal(h.calls[0].url, "/api/v2/agent/threads/t/events?after_sequence=3");
});

test("dynamic binary URLs are admitted only by the generated response media contract", async () => {
  const h = fixture();
  const value = await h.transport.readApiBlob("/api/v1/assistant/artifacts/file-1/download");
  assert.equal(await value.text(), "wire bytes"); assert.equal(h.calls.length, 1);
  for (const url of ["https://evil.invalid/api/v1/assistant/artifacts/a/download", "//evil.invalid/api/", "/api/v1/users", "/api/v1/assistant/artifacts/../users/download", "/api/v1/assistant/artifacts/%2e%2e/download", "/api/v1/assistant/artifacts/%2fusers/download", "/api/v1/assistant/artifacts/a%5cusers/download", "/api/v1/assistant/artifacts/%zz/download", "/api/v1/assistant/artifacts/a/download#fragment"]) {
    await assert.rejects(h.transport.readApiBlob(url), /binary|published|same-origin/);
  }
  assert.equal(h.calls.length, 1, "rejected URLs must never reach the HTTP adapter");
  await h.transport.readApiBlob("/api/v1/assistant/shares/share-1/artifact/file-1");
  assert.equal(h.calls.length, 2, "the real audience-scoped shared byte route remains supported");
});
test("generated multipart encodes binary arrays and leaves the boundary to the browser", async () => {
  const h = fixture(), body = { file: new Blob(["file"]), processing_mode: "async" };
  const response = await h.transport.request("POST /api/v1/files/upload", { body });
  assert.ok(response);
  const form = h.calls[0].data as FormData;
  assert.ok(form instanceof FormData); assert.equal(await (form.get("file") as Blob).text(), "file");
  assert.equal(form.get("processing_mode"), "async"); assert.deepEqual(Object.keys(h.calls[0].headers as object), []);
  const headers = new Headers({ "Content-Type": "multipart/form-data" });
  const many = h.transport.operationBody("POST /api/v1/files/upload", { body: { file: [new Blob(["a"]), new Blob(["b"])] } }, headers) as FormData;
  assert.equal(many.getAll("file").length, 2); assert.equal(headers.has("Content-Type"), false);
  assert.equal(h.transport.operationBody("POST /api/v1/files/upload", { body: form }, headers), form);
  assert.throws(() => h.transport.operationBody("POST /api/v1/files/upload", { body: "wrong" }, headers), /object/);
});

test("declared structured JSON media stays intact instead of silently becoming application/json", () => {
  const { transport } = fixture(), headers = new Headers(), body = { userName: "scim-user" };
  assert.equal(transport.operationBody("POST /scim/v2/{tenant}/Users", { body }, headers), body);
  assert.equal(headers.get("Content-Type"), "application/scim+json");
});
