import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Execute the complete production page and API adapter, with controlled React
// scheduling and HTTP collaborators. No actual credentials or service is used.
type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Frame = { slots: unknown[]; effects: (() => void)[]; cleanup: (() => void)[] };
type Row = ReturnType<typeof row>;
function row(id = "fixture-id", overrides: Record<string, unknown> = {}) {
  return { key_id: id, name: "Fixture key", key_prefix: "synthetic-prefix", user_id: "owner-A",
    scopes: ["knowledge:read"], is_active: true, created_at: "2026-10-13T00:00:00Z",
    expires_at: "2026-10-20T00:00:00Z", last_used_at: null, use_count: 0, ...overrides };
}
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
async function settle() { for (let i = 0; i < 60; i++) await Promise.resolve(); }
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate)];
}
function host(initial: Row[] = []) {
  const frames = new Map<string, Frame>();
  let frame!: Frame; let cursor = 0; let dirty = false; let seen = new Set<string>(); let tree: Tree;
  let live = initial;
  let read: () => Promise<unknown> = async () => structuredClone(live);
  let write: (path: string, body?: Record<string, unknown>) => Promise<unknown> = async (path, body) => {
    if (path.endsWith("/revoke")) { const id = decodeURIComponent(path.split("/").at(-2)!); live = live.map(value => value.key_id === id ? { ...value, is_active: false } : value); return { key_id: id }; }
    const made = row("created-id", body); live = [made, ...live]; return { ...made, api_key: "synthetic-one-time-value" };
  };
  const requests: { method: string; path: string; body?: Record<string, unknown>; params?: unknown }[] = [];
  const auth = { token: "synthetic-session-A", user: { user_id: "owner-A", roles: ["user"], permissions: [], tier: "normal" } };
  const authority = (user: typeof auth.user) => JSON.stringify([user.user_id, user.roles, user.permissions, user.tier]);
  let identity = 0;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const react = {
    useState(initial: unknown) { const owner = frame; const index = cursor++; if (!(index in owner.slots)) owner.slots[index] = typeof initial === "function" ? initial() : initial;
      return [owner.slots[index], (next: unknown) => { const value = typeof next === "function" ? next(owner.slots[index]) : next; if (!Object.is(value, owner.slots[index])) { owner.slots[index] = value; dirty = true; } }]; },
    useRef(initial: unknown) { const index = cursor++; return frame.slots[index] ?? (frame.slots[index] = { current: initial }); },
    useMemo(compute: () => unknown, deps: unknown[]) { const index = cursor++; const old = frame.slots[index] as { deps?: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) frame.slots[index] = { deps, value: compute() }; return (frame.slots[index] as { value: unknown }).value; },
    useLayoutEffect(run: () => void | (() => void), deps: unknown[]) { const owner = frame; const index = cursor++;
      if (!same(owner.slots[index] as unknown[], deps)) { owner.slots[index] = deps; owner.effects.push(() => { owner.cleanup[index]?.(); owner.cleanup[index] = run() || (() => {}); }); } },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const modules: Record<string, unknown> = {
    "@/lib/fetchClient": { isHttpError: (error: { isHttpError?: boolean }) => error?.isHttpError === true },
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-router-dom": { Link: "Link" },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++identity}` },
    "@/lib/api": { api: {
      async get(path: string, options: { params?: unknown; signal?: AbortSignal }) { requests.push({ method: "GET", path, params: options.params }); return { data: await read() }; },
      async post(path: string, body?: Record<string, unknown>) { requests.push({ method: "POST", path, body: body && structuredClone(body) }); return { data: await write(path, body) }; },
    } },
  };
  function load(filename: string): Record<string, (props: Record<string, unknown>) => Tree> {
    const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(filename, import.meta.url), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports, require: (name: string) => { if (name.startsWith("@/components/ui/")) return named; assert.ok(name in modules, `Unexpected import ${name}`); return modules[name]; },
      Error, AbortController, Date,
    });
    return exports;
  }
  modules["@/api/knowledgeApiKeys"] = load("../../api/knowledgeApiKeys.ts");
  const page = load("./KnowledgeApiKeys.tsx").KnowledgeApiKeysPage;
  function renderNode(node: unknown, path: string): unknown {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((value, index) => renderNode(value, `${path}:${index}`));
    const value = node as Tree;
    if (typeof value.type === "function") {
      const key = `${path}:${value.type.name}:${value.key ?? ""}`; seen.add(key);
      frame = frames.get(key) ?? { slots: [], effects: [], cleanup: [] }; frames.set(key, frame); cursor = 0;
      return renderNode(value.type(value.props), key);
    }
    return { ...value, props: { ...value.props, children: renderNode(value.props?.children, `${path}:children`) } };
  }
  function render() { for (let turn = 0; turn < 15; turn++) { dirty = false; seen = new Set(); tree = renderNode({ type: page, props: {} }, "root") as Tree;
    for (const [key, owner] of frames) { if (!seen.has(key)) { owner.cleanup.forEach(cleanup => cleanup?.()); frames.delete(key); } else owner.effects.splice(0).forEach(effect => effect()); }
    if (!dirty) return tree;
  } throw new Error("Unexpected render loop"); }
  const nodes = (predicate: (node: Tree) => boolean) => all(render(), predicate);
  const button = (suffix: string, index = 0) => { const found = nodes(node => node.type === "Button" && node.props.children === `knowledgeKeys.${suffix}`)[index]; assert.ok(found, `Missing ${suffix}`); return found; };
  return { auth, requests, render, button,
    posts: () => requests.filter(request => request.method === "POST"),
    async start() { render(); await settle(); render(); },
    async click(suffix: string, index = 0) { const node = button(suffix, index); assert.equal(Boolean(node.props.disabled), false, `${suffix} disabled`); (node.props.onClick as () => void)(); await settle(); render(); },
    name(value: string) { (nodes(node => node.type === "Input")[0].props.onChange as (event: unknown) => void)({ target: { value } }); render(); },
    selectedWrite: () => nodes(node => node.type === "input")[0].props.checked,
    selectedDays: () => nodes(node => node.type === "select")[0].props.value,
    setDays(value: string) { (nodes(node => node.type === "select")[0].props.onChange as (event: unknown) => void)({ target: { value } }); render(); },
    writeScope(value: boolean) { (nodes(node => node.type === "input")[0].props.onChange as (event: unknown) => void)({ target: { checked: value } }); render(); },
    submit() { (nodes(node => node.type === "form")[0].props.onSubmit as (event: unknown) => void)({ preventDefault() {} }); render(); },
    has(suffix: string) { return nodes(node => node.props?.children === `knowledgeKeys.${suffix}`).length > 0; },
    text() { return JSON.stringify(render()); },
    count(type: string) { return nodes(node => node.type === type).length; },
    setRead(next: typeof read) { read = next; }, setWrite(next: typeof write) { write = next; }, setLive(next: Row[]) { live = next; },
    metadata(value: unknown) { return (modules["@/api/knowledgeApiKeys"] as { keyMetadata: (value: unknown) => unknown }).keyMetadata(value); },
    dispose() { for (const owner of frames.values()) owner.cleanup.forEach(cleanup => cleanup?.()); frames.clear(); },
  };
}

test("complete issue, one-time reveal, metadata and exact-ID revoke journey", async () => {
  const h = host(); try { await h.start(); assert.ok(h.has("empty")); h.name("Fixture key"); h.submit(); assert.equal(h.posts().length, 0);
    await h.click("confirmCreate"); assert.equal(h.posts().length, 1); const input = h.posts()[0].body!;
    assert.deepEqual(input.scopes, ["knowledge:read"]); assert.ok(Date.parse(String(input.expires_at)) > Date.now());
    assert.ok(h.text().includes("synthetic-one-time-value")); assert.ok(h.has("issued"));
    await h.click("dismissSecret"); assert.ok(!h.text().includes("synthetic-one-time-value"));
    await h.click("reviewRevoke"); assert.equal(h.posts().length, 1); await h.click("confirmRevoke");
    assert.equal(h.posts()[1].path, "/api/v1/api-keys/created-id/revoke"); assert.ok(h.has("revokedObserved")); assert.ok(h.has("revoked"));
    assert.deepEqual(JSON.parse(JSON.stringify(h.requests[0].params)), { include_inactive: true });
  } finally { h.dispose(); }
});
test("write scope is explicitly selected and double confirmation cannot issue twice", async () => {
  const h = host(); const waiting = deferred<unknown>(); try { await h.start(); h.name("Fixture key"); h.writeScope(true); h.submit(); h.setWrite(() => waiting.promise);
    const confirm = h.button("confirmCreate").props.onClick as () => void; confirm(); confirm(); await settle(); assert.equal(h.posts().length, 1);
    assert.deepEqual(h.posts()[0].body?.scopes, ["knowledge:read", "knowledge:write"]);
    waiting.reject(new Error("synthetic timeout")); await settle(); assert.ok(h.has("issueUnknown"));
  } finally { h.dispose(); }
});
test("lost issuance acknowledgement requires fresh metadata and explicit separate decision", async () => {
  const h = host(); try { await h.start(); h.name("Fixture key"); h.submit(); h.setWrite(async () => { h.setLive([row("possibly-created")]); throw new Error("synthetic timeout"); });
    await h.click("confirmCreate"); assert.ok(h.has("issueUnknown")); assert.equal(h.posts().length, 1); h.submit(); assert.equal(h.posts().length, 1);
    assert.ok(!h.text().includes("synthetic-one-time-value")); await h.click("allowNewDecision"); assert.ok(h.has("newDecision")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("unknown issuance plus failed read cannot authorize another creation", async () => {
  const h = host(); try { await h.start(); h.name("Fixture key"); h.submit(); h.setWrite(async () => { h.setRead(async () => { throw new Error("offline"); }); throw new Error("timeout"); });
    await h.click("confirmCreate"); assert.ok(h.has("readError")); assert.equal(h.button("allowNewDecision").props.disabled, true); assert.equal(h.posts().length, 1);
    h.setRead(async () => []); await h.click("refresh"); await h.click("allowNewDecision"); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("late issuance secret is discarded on account, credential or authority change", async () => {
  for (const change of ["account", "token", "role"]) { const h = host(); const waiting = deferred<unknown>(); try {
    await h.start(); h.name("Fixture key"); h.submit(); h.setWrite(() => waiting.promise); await h.click("confirmCreate");
    if (change === "account") h.auth.user.user_id = "owner-B"; else if (change === "token") h.auth.token = "synthetic-session-B"; else h.auth.user.roles = ["admin"];
    h.render(); waiting.resolve({ ...row("old-id", h.posts()[0].body), api_key: "synthetic-old-secret" }); await settle(); h.render();
    assert.ok(!h.text().includes("synthetic-old-secret")); assert.ok(!h.has("issued")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); } }
});
test("late issuance after unmount does not start metadata recovery or expose a secret", async () => {
  const h = host(); const waiting = deferred<unknown>(); await h.start(); h.name("Fixture key"); h.submit(); h.setWrite(() => waiting.promise); await h.click("confirmCreate");
  h.dispose(); const count = h.requests.length; waiting.resolve({ ...row("old-id", h.posts()[0].body), api_key: "synthetic-old-secret" }); await settle(); assert.equal(h.requests.length, count);
});
test("duplicate names remain distinguishable and revoke only the reviewed encoded ID", async () => {
  const h = host([row("one/id"), row("second-id")]); try { await h.start(); await h.click("reviewRevoke"); assert.ok(h.text().includes("one/id")); await h.click("confirmRevoke");
    assert.equal(h.posts()[0].path, "/api/v1/api-keys/one%2Fid/revoke"); assert.ok(h.has("revokedObserved")); assert.equal(h.count("article"), 2);
  } finally { h.dispose(); }
});
test("lost revoke acknowledgement uses metadata observation without replay", async () => {
  const h = host([row()]); try { await h.start(); await h.click("reviewRevoke"); h.setWrite(async () => { h.setLive([row("fixture-id", { is_active: false })]); throw new Error("timeout"); });
    await h.click("confirmRevoke"); assert.ok(h.has("revokedObserved")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("failed or mismatched revoke receipt does not invent disabled status", async () => {
  for (const fail of [true, false]) { const h = host([row()]); try { await h.start(); await h.click("reviewRevoke");
    h.setWrite(async () => { if (fail) throw new Error("offline"); return { key_id: "wrong-id" }; }); await h.click("confirmRevoke");
    assert.ok(h.has("revokeUnknown")); assert.ok(h.has("enabled")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); } }
});
test("successful revoke with failed readback is not labelled observed success", async () => {
  const h = host([row()]); try { await h.start(); await h.click("reviewRevoke"); h.setWrite(async () => { h.setRead(async () => { throw new Error("offline"); }); return { key_id: "fixture-id" }; });
    await h.click("confirmRevoke"); assert.ok(h.has("revokeReadback")); assert.ok(!h.has("revokedObserved")); assert.equal(h.count("article"), 0);
  } finally { h.dispose(); }
});
test("initial read failure is not an empty successful list and blocks creation", async () => {
  const h = host(); try { h.setRead(async () => { throw new Error("offline"); }); await h.start(); assert.ok(h.has("readError")); assert.ok(!h.has("empty")); h.name("Fixture key"); h.submit(); assert.equal(h.posts().length, 0); }
  finally { h.dispose(); }
});
test("metadata projection strips all unknown fields including secret and tolerates legacy null scopes", () => {
  const h = host(); try { const metadata = h.metadata(row("fixture", { scopes: null, api_key: "must-not-retain", key_hash: "must-not-retain" }));
    assert.ok(!JSON.stringify(metadata).includes("must-not-retain")); assert.deepEqual(JSON.parse(JSON.stringify(metadata)).scopes, []);
  } finally { h.dispose(); }
});
test("metadata list local pagination reaches older keys without claiming server pagination", async () => {
  const h = host(Array.from({ length: 23 }, (_, i) => row(`key-${i}`))); try { await h.start(); assert.equal(h.count("article"), 20);
    await h.click("next"); assert.equal(h.count("article"), 3); assert.ok(h.text().includes("key-22")); assert.equal(h.requests.length, 1);
    await h.click("previous"); assert.equal(h.count("article"), 20);
  } finally { h.dispose(); }
});
test("mismatched issuance scope or expiry is uncertain and never exposes the secret", async () => {
  const h = host(); try { await h.start(); h.name("Fixture key"); h.submit(); h.setWrite(async () => ({ ...row(), api_key: "unexpected-secret" }));
    await h.click("confirmCreate"); assert.ok(h.has("issueUnknown")); assert.ok(!h.text().includes("unexpected-secret")); assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("route and navigation retain separate legacy configuration key surface", () => {
  const route = readFileSync(new URL("../../router.tsx", import.meta.url), "utf8");
  const settings = readFileSync(new URL("../Settings.tsx", import.meta.url), "utf8");
  assert.ok(route.includes('path="/settings/knowledge-api-keys"')); assert.ok(settings.includes('to="/settings/knowledge-api-keys"'));
  assert.ok(settings.includes('/api/v1/config/api-keys')); assert.ok(settings.includes('createApiKeyMutation.mutate(apiKeyForm)'));
});

test("current metadata permission denial hides the one-time secret", async () => {
  const h = host(); try { await h.start(); h.name("Fixture key"); h.submit(); await h.click("confirmCreate");
    assert.ok(h.text().includes("synthetic-one-time-value"));
    h.setRead(async () => { throw Object.assign(new Error("denied"), { isHttpError: true, response: { status: 403 } }); });
    await h.click("refresh"); assert.ok(!h.text().includes("synthetic-one-time-value")); assert.ok(h.has("readError"));
  } finally { h.dispose(); }
});

test("a separate grant resets write permission, name and expiry choices", async () => {
  const h = host(); try { await h.start(); h.name("Fixture key"); h.writeScope(true); h.setDays("30"); h.submit();
    h.setWrite(async () => { throw new Error("timeout"); }); await h.click("confirmCreate"); await h.click("allowNewDecision");
    assert.equal(h.selectedWrite(), false); assert.equal(h.selectedDays(), "7"); assert.equal(h.button("reviewCreate").props.disabled, true);
    assert.equal(h.posts().length, 1);
  } finally { h.dispose(); }
});
test("current metadata denial clears a pending exact-key review", async () => {
  const h = host([row()]); try { await h.start(); await h.click("reviewRevoke"); assert.ok(h.has("reviewTitle"));
    h.setRead(async () => { throw Object.assign(new Error("denied"), { isHttpError: true, response: { status: 401 } }); });
    await h.click("refresh"); assert.ok(!h.has("reviewTitle")); assert.equal(h.posts().length, 0);
  } finally { h.dispose(); }
});
test("enabled metadata never promises that expiration or admission succeeded", async () => {
  const h = host([row("expired", { expires_at: "2020-01-01T00:00:00Z" })]); try { await h.start();
    assert.ok(h.has("enabled")); assert.ok(h.has("listScope")); assert.ok(h.text().includes(new Date("2020-01-01T00:00:00Z").toLocaleString())); assert.equal(h.posts().length, 0);
  } finally { h.dispose(); }
});

test("legacy database rows retain unknown scopes and roles and revoke the returned exact ID", async () => {
  const h = host([row("ak_legacy_7", { key_prefix: null, scopes: ["future:scope"], roles: ["custom-role"], tier: "custom-tier" }), row("custom-public-id")]);
  try {
    await h.start(); assert.ok(h.text().includes("future:scope")); assert.ok(h.text().includes("custom-role")); assert.ok(h.text().includes("custom-tier"));
    await h.click("reviewRevoke"); assert.equal(h.posts().length, 0);
    await h.click("confirmRevoke"); assert.equal(h.posts()[0].path, "/api/v1/api-keys/ak_legacy_7/revoke");
    assert.equal(h.posts().length, 1); assert.ok(h.has("revokedObserved")); assert.equal(h.count("article"), 2);
    assert.equal(h.button("reviewRevoke").props.disabled, false);
  } finally { h.dispose(); }
});

test("legacy null metadata is not recast as a Knowledge grant", async () => {
  const h = host([row("server-returned-id", { scopes: null, roles: null, tier: null, key_prefix: null })]);
  try { await h.start(); await h.click("reviewRevoke"); await h.click("confirmRevoke");
    assert.equal(h.posts()[0].path, "/api/v1/api-keys/server-returned-id/revoke");
    assert.equal(h.posts()[0].body, undefined); assert.ok(h.has("revokedObserved"));
  } finally { h.dispose(); }
});

test("database management affordance preserves canonical routing and truthful boundaries", () => {
  const settings = readFileSync(new URL("../Settings.tsx", import.meta.url), "utf8");
  assert.ok(settings.includes('t("knowledgeKeys.manageDatabase")'));
  assert.ok(settings.includes('t("knowledgeKeys.configBoundary")'));
  assert.ok(settings.includes('to="/settings/knowledge-api-keys"'));
  assert.ok(!settings.includes("ak_legacy_"));
  for (const locale of ["en-US", "zh-CN"]) {
    const copy = JSON.parse(readFileSync(new URL(`../../i18n/locales/${locale}.json`, import.meta.url), "utf8")).knowledgeKeys;
    assert.ok(copy.roles && copy.tier && copy.configBoundary && copy.manageDatabase);
    assert.ok(!copy.revokeImpact.includes("New requests using it should be rejected"));
  }
});
