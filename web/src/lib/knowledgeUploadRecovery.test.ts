import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as zustand from "zustand";
import * as middleware from "zustand/middleware";
import * as consoleTransport from "./consoleSessionTransport.ts";
import type * as Recovery from "./knowledgeUploadRecovery";
import type { AuthState } from "../store/useAuthStore";

function host() {
  const values = new Map<string, string>();
  let unavailable = false;
  const storage = { get length() { return values.size; }, key: (i: number) => [...values.keys()][i] ?? null,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { if (unavailable) throw Error("controlled storage failure"); values.set(key, value); },
    removeItem: (key: string) => { if (unavailable) throw Error("controlled storage failure"); values.delete(key); },
  };
  const modules: Record<string, unknown> = { zustand, "zustand/middleware": middleware, "@/lib/consoleSessionTransport": consoleTransport };
  function load(file: string) {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8").replaceAll("import.meta.hot", "__hot"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
    }).outputText, { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
      sessionStorage: storage, localStorage: storage, crypto, TextEncoder, Date, Error, __hot: undefined });
    return exports;
  }
  const recovery = load("./knowledgeUploadRecovery.ts") as typeof Recovery;
  modules["@/lib/knowledgeUploadRecovery"] = recovery;
  modules["@/lib/sdkAuthorityScope"] = load("./sdkAuthorityScope.ts");
  return { values, storage, recovery, failStorage() { unavailable = true; },
    loadAuth: () => load("../store/useAuthStore.ts").useAuthStore as { getState: () => AuthState; setState: (state: Partial<AuthState>) => void },
  };
}
const files = () => [new File(["PRIVATE SOURCE CONTENT"], "source.txt")];

test("upload receipts persist only bounded source metadata and exact local identities", () => {
  const h = host(), key = h.recovery.uploadRecoveryKey("authority", "tenant", "dataset")!;
  const receipt = h.recovery.beginUploadRecovery(key, files());
  assert.equal(receipt.files[0].state, "not_submitted");
  assert.equal([...h.values.values()].join("").includes("PRIVATE SOURCE CONTENT"), false);
  receipt.files[0].state = "unknown"; assert.equal(h.recovery.updateUploadRecovery(key, receipt), true);
  assert.equal(h.recovery.readUploadRecovery(key)[0].files[0].state, "unknown");
  h.recovery.writeUploadRecovery(key, []); assert.equal(h.recovery.updateUploadRecovery(key, receipt), false);
  assert.equal(h.recovery.readUploadRecovery(key).length, 0);
});

test("expiry removes old metadata across dataset keys on the next recovery access", () => {
  const h = host(); const now = Date.now();
  for (const name of ["a", "b"]) h.values.set(h.recovery.uploadRecoveryKey("owner", "tenant", name)!, JSON.stringify([
    { id: name, createdAt: now - 24 * 60 * 60 * 1000, files: [{ position: 1, name: "expired-private.txt", sizeBytes: 1, state: "unknown" }] },
  ]));
  h.recovery.readUploadRecovery(h.recovery.uploadRecoveryKey("owner", "tenant", "c")!, now);
  assert.equal(h.values.size, 0);
});

test("attempt/file/namespace bounds block without evicting unresolved receipts", () => {
  const h = host(), key = h.recovery.uploadRecoveryKey("owner", "tenant", "dataset")!;
  for (let i = 0; i < 10; i++) h.recovery.beginUploadRecovery(key, files());
  const before = h.values.get(key); assert.throws(() => h.recovery.beginUploadRecovery(key, files()), /limit/);
  assert.equal(h.values.get(key), before);
  assert.throws(() => h.recovery.beginUploadRecovery("other", Array.from({ length: 51 }, () => files()[0])), /limit/);
  h.values.set(h.recovery.uploadRecoveryKey("other", "tenant", "dataset")!, "x".repeat(256 * 1024));
  assert.throws(() => h.recovery.writeUploadRecovery(key, h.recovery.readUploadRecovery(key)), /limit/);
  assert.equal(h.values.get(key), before);
  h.recovery.writeUploadRecovery(key, []); assert.equal(h.values.has(key), false);
  assert.equal(h.values.has(h.recovery.uploadRecoveryKey("other", "tenant", "dataset")!), true);
});

test("malformed and mismatched receipt metadata is rejected without reinterpretation", () => {
  const h = host(), key = h.recovery.uploadRecoveryKey("owner", "tenant", "dataset")!;
  for (const value of ["broken", "{}", JSON.stringify([{ id: "a", createdAt: Date.now(), files: [{ position: 2, name: "a", sizeBytes: 1, state: "accepted" }] }])]) {
    h.values.set(key, value); assert.throws(() => h.recovery.readUploadRecovery(key)); assert.equal(h.values.get(key), value);
  }
});

test("global auth cleanup prevents same-user revival without mounting Knowledge", () => {
  const h = host(); const auth = h.loadAuth(); const user = { user_id: "owner", email: null, display_name: null,
    department: null, roles: ["member"], permissions: [], tier: "normal" };
  auth.getState().setAuth("synthetic-token", user, false);
  const key = h.recovery.uploadRecoveryKey("owner", "tenant", "dataset")!;
  h.recovery.beginUploadRecovery(key, files()); h.values.set("unrelated", "preserve");
  auth.getState().clearAuth(); assert.equal(h.values.has(key), false);
  auth.getState().setAuth("synthetic-token", user, false); assert.equal(h.recovery.readUploadRecovery(key).length, 0);
  assert.equal(h.values.get("unrelated"), "preserve");
  h.recovery.beginUploadRecovery(key, files()); auth.getState().updateUser({ roles: ["viewer"] }); assert.equal(h.values.has(key), false);
});

test("legacy bearer hydration is refused; bootstrap retires receipts from the prior auth document", () => {
  const h = host(); const key = h.recovery.uploadRecoveryKey("owner", "tenant", "dataset")!;
  h.recovery.beginUploadRecovery(key, files());
  h.values.set("agent-gateway-auth", JSON.stringify({ state: { token: "synthetic-token", isAuthenticated: true,
    user: { user_id: "owner", roles: [], permissions: [], tier: "normal" } }, version: 0 }));
  const auth = h.loadAuth(); assert.equal(auth.getState().isAuthenticated, false);
  assert.equal(auth.getState().token,null);assert.equal(h.values.has("agent-gateway-auth"),false);
  assert.equal(h.recovery.readUploadRecovery(key).length, 1);
  auth.getState().setAuth("synthetic-csrf",{user_id:"owner",email:null,display_name:null,department:null,roles:[],permissions:[],tier:"normal"},false);
  assert.equal(h.values.has(key), false);
});

test("failed global cleanup cannot revive stale metadata in this document", () => {
  const h = host(); const key = h.recovery.uploadRecoveryKey("owner", "tenant", "dataset")!;
  h.recovery.beginUploadRecovery(key, files()); h.failStorage(); h.recovery.clearUploadRecoverySession();
  assert.throws(() => h.recovery.readUploadRecovery(key), /unavailable/);
  assert.throws(() => h.recovery.writeUploadRecovery(key, []), /unavailable/);
});
