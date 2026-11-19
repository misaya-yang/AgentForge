import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import fetchHttp, { type RequestAdapter } from './fetchClient.ts';
import * as controller from "./consoleStepUp.ts";

function fixture() {
  const calls: string[] = [];
  let generation = 1;
  let identity = { isAuthenticated: true, token: "console-session:1", csrfToken: "csrf-1", user: { user_id: "u", tenant_id: "t", roles: ["admin"] } };
  const listeners = new Set<() => void>();
  let assertion: () => Promise<unknown> = async () => { calls.push("native-passkey"); return { assertion: true }; };
  let enabled = true, verified = false;
  let businessFailure = false;
  let business: (() => Promise<void>) | undefined;
  const api = fetchHttp.create();
  const adapter: RequestAdapter = async config => {
    const path = config.url!; calls.push(`${config.method!.toUpperCase()} ${path}`);
    if (businessFailure && path === "/api/v1/providers/p") throw new Error("unknown mutation outcome");
    if (business && path === "/api/v1/providers/p") await business();
    const data = path === "/api/auth/session/step-up" ? { enabled, verified, passkey_enrolled: true }
      : path.endsWith("/start") ? { flow_id: "step-up-flow", publicKey: { challenge: "YWJj", userVerification: "required" } }
        : path.endsWith("/finish") ? { verified: true } : { saved: true };
    return { data, status: 200, statusText: "OK", headers: {}, config };
  };
  api.defaults.adapter = adapter;
  // Same registration order as production's BFF header interceptor.
  api.interceptors.request.use(config => {
    if (config.headers.get("Authorization") === `Bearer ${identity.token}`) config.headers.delete("Authorization");
    if (!config.headers.has("Authorization")) config.headers.set("X-CSRF-Token", identity.csrfToken);
    return config;
  }, error => { throw error; }, { synchronous: true });
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./installConsoleStepUp.ts", import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports, require(name: string) {
    if (name === "@/lib/fetchClient") return { __esModule: true, default: fetchHttp };
    if (name === "./consoleStepUp") return controller;
    if (name === "./passkey") return { getPasskeyAssertion: () => assertion() };
    if (name === "@/store/useAuthStore") return {
      useAuthStore: { getState: () => identity, subscribe(listener: () => void) { listeners.add(listener); return () => listeners.delete(listener); } },
      consoleAuthGeneration: () => generation,
    };
    if (name === "@/lib/sdkAuthorityScope") return { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) };
    throw new Error(`Unexpected import: ${name}`);
  }, __hot: undefined, window: { location: { origin: "https://console.example" } }, URL, AbortController, Error });
  const uninstall = (exports.installConsoleStepUp as (api: unknown) => () => void)(api);
  return { api, calls, uninstall, setVerified(value: boolean) { verified = value; }, setEnabled(value: boolean) { enabled = value; },
    setAssertion(value: () => Promise<unknown>) { assertion = value; }, failBusiness() { businessFailure = true; },
    holdBusiness(value: () => Promise<void>) { business = value; },
    replaceSession() { generation++; identity = { ...identity, token: `console-session:${generation}` }; listeners.forEach(listener => listener()); },
    replaceWithOriginalIdentity() { generation++; identity = { ...identity, token: "console-session:1" }; listeners.forEach(listener => listener()); },
    listenerCount: () => listeners.size };
}
test("actual Fetch adapter performs re-authentication before exactly one provider mutation", async () => {
  const h = fixture(); try {
    await h.api.put("/api/v1/providers/p", { api_key: "synthetic" });
    assert.deepEqual(h.calls, ["GET /api/auth/session/step-up", "POST /api/auth/enterprise/passkeys/step-up/start", "native-passkey", "POST /api/auth/enterprise/passkeys/step-up/finish", "PUT /api/v1/providers/p"]);
    assert.equal(h.listenerCount(), 0);
  } finally { h.uninstall(); }
});
test("synchronous A to B to A replacement cannot dispatch a previously prepared mutation", async () => {
  const h = fixture(); try {
    const pending = h.api.put("/api/v1/providers/p", { api_key: "synthetic" });
    h.replaceSession(); h.replaceWithOriginalIdentity();
    await assert.rejects(pending, /session changed|canceled/);
    assert.equal(h.calls.some(call => call === "PUT /api/v1/providers/p"), false);
    assert.equal(h.listenerCount(), 0);
  } finally { h.uninstall(); }
});
test("account switch while native WebAuthn is pending cannot finish or write an old draft", async () => {
  const h = fixture(); let resolve!: (value: unknown) => void;
  h.setAssertion(() => new Promise(done => { resolve = done; }));
  try {
    const pending = h.api.put("/api/v1/providers/p", { api_key: "synthetic" });
    for (let i = 0; i < 25 && !resolve; i++) await Promise.resolve();
    assert.ok(resolve); h.replaceSession(); resolve({ assertion: true });
    await assert.rejects(pending, /session changed|canceled/);
    assert.equal(h.calls.some(call => call.endsWith("/finish") || call === "PUT /api/v1/providers/p"), false);
  } finally { h.uninstall(); }
});
test("native cancellation never sends sensitive mutation, and unknown write outcome is never retried", async () => {
  const h = fixture(); try {
    h.setAssertion(async () => { throw new Error("cancelled passkey"); });
    await assert.rejects(h.api.put("/api/v1/providers/p", {}), /cancelled/);
    assert.equal(h.calls.filter(call => call === "PUT /api/v1/providers/p").length, 0);
    h.setVerified(true); h.failBusiness();
    await assert.rejects(h.api.put("/api/v1/providers/p", {}), /unknown mutation outcome/);
    assert.equal(h.calls.filter(call => call === "PUT /api/v1/providers/p").length, 1);
  } finally { h.uninstall(); }
});
test("existing SDK bearer bypasses browser ceremonies; console session markers never do", async () => {
  const h = fixture(); try {
    await h.api.put("/api/v1/providers/p", {}, { headers: { Authorization: "Bearer sdk-token" } });
    assert.deepEqual(h.calls, ["PUT /api/v1/providers/p"]); h.calls.length = 0;
    await h.api.put("/api/v1/providers/p", {}, { headers: { Authorization: "Bearer console-session:1" } });
    assert.equal(h.calls[0], "GET /api/auth/session/step-up");
    h.calls.length = 0; await h.api.get("/api/v1/providers");
    assert.deepEqual(h.calls, ["GET /api/v1/providers"]);
  } finally { h.uninstall(); }
});
test("sensitive response arriving after account replacement is discarded before UI receives secrets", async () => {
  const h = fixture(); let resolve!: () => void;
  h.setVerified(true); h.holdBusiness(() => new Promise(done => { resolve = done; }));
  try {
    const pending = h.api.put("/api/v1/providers/p", { api_key: "synthetic" });
    for (let i = 0; i < 25 && !resolve; i++) await Promise.resolve();
    assert.ok(resolve); h.replaceSession(); resolve();
    await assert.rejects(pending, /session changed after sensitive request/);
    assert.equal(h.calls.filter(call => call === "PUT /api/v1/providers/p").length, 1);
  } finally { h.uninstall(); }
});
