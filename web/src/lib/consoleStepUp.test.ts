import assert from "node:assert/strict";
import test from "node:test";
import { authorizeConsoleMutation, sensitiveConsoleMutation, sensitiveMutationOptions, validateStepUpStatus, type StepUpClient } from "./consoleStepUp.ts";
import { MutationObserver, QueryClient } from "@tanstack/react-query";

function fixture() {
  const calls: string[] = [];
  let current = true;
  const client: StepUpClient = {
    status: async () => { calls.push("status"); return { enabled: true, verified: false, passkey_enrolled: true }; },
    start: async () => { calls.push("start"); return { flow_id: "step-up-only-flow", publicKey: { challenge: "YWJj", userVerification: "required" } }; },
    assertion: async () => { calls.push("webauthn"); return { synthetic: "assertion" }; },
    finish: async (flow, credential) => {
      calls.push("finish"); assert.equal(flow, "step-up-only-flow"); assert.deepEqual(credential, { synthetic: "assertion" }); return { verified: true };
    },
  };
  return { client, calls, isCurrent: () => current, replaceSession: () => { current = false; } };
}
test("sensitive path classifier covers production roots, reads and prefix collisions", () => {
  for (const root of ["/api/v1/api-keys", "/api/v1/config/api-keys", "/api/v1/providers", "/api/v1/roles", "/api/v1/users", "/api/v1/config/auth", "/api/v1/config/sso", "/api/auth/enterprise/identities", "/api/auth/enterprise/scim-credentials"]) {
    for (const method of ["post", "PUT", "PATCH", "DELETE"]) {
      assert.equal(sensitiveConsoleMutation(method, root), true);
      assert.equal(sensitiveConsoleMutation(method, `${root}/id/revoke`), true);
    }
    assert.equal(sensitiveConsoleMutation("GET", root), false);
    assert.equal(sensitiveConsoleMutation("POST", `${root}-unrelated`), false);
  }
  assert.equal(sensitiveConsoleMutation("PUT", "/api/v1/users/user/profile"), false);
  assert.equal(sensitiveConsoleMutation("POST", "/api/auth/enterprise/passkeys/step-up/start"), false);
});
test("authorization verifies before the single sensitive dispatch and never retries", async () => {
  const h = fixture(); await authorizeConsoleMutation(h.client, h.isCurrent); h.calls.push("business-write");
  assert.deepEqual(h.calls, ["status", "start", "webauthn", "finish", "business-write"]);
});
for (const boundary of ["status", "start", "assertion", "finish"] as const) {
  test(`account replacement during ${boundary} prevents sensitive dispatch`, async () => {
    const h = fixture(), original = h.client[boundary];
    Object.assign(h.client, { [boundary]: async (...args: never[]) => { const result = await (original as (...args: never[]) => Promise<unknown>)(...args); h.replaceSession(); return result; } });
    await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent), /session changed/);
    assert.equal(h.calls.includes("business-write"), false);
    assert.equal(h.calls.at(-1), boundary === "assertion" ? "webauthn" : boundary);
  });
  test(`${boundary} failure is fail-closed and does not retry or dispatch`, async () => {
    const h = fixture(); let attempts = 0;
    Object.assign(h.client, { [boundary]: async () => { attempts++; throw new Error("authority unavailable or cancelled"); } });
    await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent), /unavailable or cancelled/);
    assert.equal(attempts, 1); assert.equal(h.calls.includes("business-write"), false);
  });
}
test("enrollment and policy responses fail closed without a password or login fallback", async () => {
  const h = fixture();
  h.client.status = async () => ({ enabled: true, verified: false, passkey_enrolled: false });
  await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent), /enrolled passkey/);
  assert.deepEqual(h.calls, []);
  for (const response of [{}, { enabled: false, verified: true, passkey_enrolled: true }, { enabled: true, verified: "true", passkey_enrolled: true }]) {
    assert.throws(() => validateStepUpStatus(response as never));
  }
});
test("only an explicit server-disabled policy skips ceremony; a local flag is never cached", async () => {
  const h = fixture(); let reads = 0;
  h.client.status = async () => { reads++; return { enabled: false, verified: false, passkey_enrolled: false }; };
  await authorizeConsoleMutation(h.client, h.isCurrent); await authorizeConsoleMutation(h.client, h.isCurrent);
  assert.equal(reads, 2); assert.deepEqual(h.calls, []);
  await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent, undefined, true), /not enabled/);
});
test("a negative completion receipt or an abort cannot authorize a mutation", async () => {
  const h = fixture(); h.client.finish = async () => ({ verified: false });
  await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent), /not verified/);
  const controller = new AbortController(); controller.abort();
  await assert.rejects(authorizeConsoleMutation(h.client, h.isCurrent, controller.signal), /session changed/);
});
test("sensitive write policy overrides global retries on an unknown mutation outcome", async () => {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: 2, retryDelay: 0 } } });
  let attempts = 0;
  const mutation = new MutationObserver(client, { ...sensitiveMutationOptions, mutationFn: async () => { attempts++; throw new Error("unknown mutation outcome"); } });
  await assert.rejects(mutation.mutate(), /unknown mutation outcome/);
  assert.equal(attempts, 1); client.clear();
});
