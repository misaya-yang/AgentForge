import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import type * as Api from "./guardrails.ts";
function fixture() {
  const calls: Array<{ method: string; url: string; body?: unknown }> = [];
  const snapshot = { policy: null, policy_pin: "confirmed-pin" };
  let failStatus: number | null = null;
  const api = { get: async (url: string) => { calls.push({ method: "GET", url }); return { data: snapshot }; },
    put: async (url: string, body: unknown) => { calls.push({ method: "PUT", url, body }); if (failStatus) throw { response: { status: failStatus } }; return { data: { policy_pin: "confirmed-pin" } }; } };
  const exported: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./guardrails.ts", import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, { exports: exported, require: (name: string) => { assert.equal(name, "@/lib/api"); return { api }; }, Error });
  return { api: exported as typeof Api, calls, snapshot, fail: (status = 409) => { failStatus = status; } };
}
test("write keeps expected pin and requires authoritative readback", async () => {
  const h = fixture(); const policy = {} as Api.GuardrailPolicy;
  await h.api.putGuardrailPolicy({ policy, expected_pin: "previous-pin" });
  assert.equal(h.calls.length, 2);
  assert.equal(h.calls[0].method, "PUT");
  assert.equal((h.calls[0].body as { expected_pin: string }).expected_pin, "previous-pin");
  assert.equal(h.calls[1].method, "GET");
  h.snapshot.policy_pin = "different-pin";
  await assert.rejects(h.api.putGuardrailPolicy({ policy, expected_pin: "previous-pin" }), /unconfirmed/);
});
test("conflict is propagated without retry or successful readback", async () => {
  const h = fixture(); h.fail();
  await assert.rejects(h.api.putGuardrailPolicy({ policy: {} as Api.GuardrailPolicy, expected_pin: null }));
  assert.equal(h.calls.length, 1);
});
test("decision IDs are escaped and malformed IDs never reach transport", async () => {
  const h = fixture(); await h.api.getGuardrailDecisions("a/b?c");
  assert.equal(h.calls[0].url, "/api/v1/guardrails/decisions/a%2Fb%3Fc");
  for (const id of ["", "\n", "x".repeat(256)]) await assert.rejects(h.api.getGuardrailDecisions(id));
  assert.equal(h.calls.length, 1);
});

test("503 remains unavailable and never becomes a successful save", async () => {
  const h = fixture(); h.fail(503);
  await assert.rejects(h.api.putGuardrailPolicy({ policy: {} as Api.GuardrailPolicy, expected_pin: null }), (error: unknown) => (error as { response: { status: number } }).response.status === 503);
  assert.equal(h.calls.length, 1);
});
