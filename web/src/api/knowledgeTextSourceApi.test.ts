import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Call = { method: string; url: string; payload?: unknown; options?: unknown };

function apiModule() {
  const calls: Call[] = [];
  const api = {
    async get(url: string, options?: unknown) { calls.push({ method: "GET", url, options }); return { data: { source_type: "text" } }; },
    async put(url: string, payload: unknown, options?: unknown) { calls.push({ method: "PUT", url, payload, options }); return { data: { status: "pending", document_id: "doc" } }; },
  };
  const modules: Record<string, unknown> = {
    "@/lib/api": { api },
    "@/lib/sse": { sseFetch: async () => {}, sseFetchEvents: async function* () {} },
  };
  const exports: Record<string, unknown> = {};
  const source = ts.transpileModule(readFileSync(new URL("./knowledge.ts", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  vm.runInNewContext(source, { exports, require: (id: string) => modules[id] });
  return { exports, calls };
}

test("text source read and optimistic replacement use the public endpoints and payload", async () => {
  const { exports, calls } = apiModule();
  const signal = new AbortController().signal;
  const getActive = exports.getActiveDocumentSource as (...args: unknown[]) => Promise<unknown>;
  const replace = exports.replaceDocumentContent as (...args: unknown[]) => Promise<unknown>;
  await getActive("dataset/a", "doc?#", { signal, timeoutMs: 5000 });
  await replace("dataset/a", "doc?#", { content: "v2", expected_current_version: 1 }, { signal, timeoutMs: 5000 });
  assert.equal(calls.length, 2);
  assert.equal(calls[0].method, "GET");
  assert.equal(calls[0].url, "/api/v1/knowledge/dataset%2Fa/documents/doc%3F%23/source");
  assert.equal((calls[0].options as { signal: AbortSignal; timeout: number }).signal, signal);
  assert.equal((calls[0].options as { timeout: number }).timeout, 5000);
  assert.equal(calls[1].method, "PUT");
  assert.equal(calls[1].url, "/api/v1/knowledge/dataset%2Fa/documents/doc%3F%23/content");
  assert.equal((calls[1].payload as { content: string }).content, "v2");
  assert.equal((calls[1].payload as { expected_current_version: number }).expected_current_version, 1);
  assert.equal((calls[1].options as { signal: AbortSignal; timeout: number }).signal, signal);
  assert.equal((calls[1].options as { timeout: number }).timeout, 5000);
});
