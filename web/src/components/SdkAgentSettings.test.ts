/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled component/transport fixture shapes. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as settings from "../lib/sdkAgentSettings.ts";
import { sdkAuthorityScope } from "../lib/sdkAuthorityScope.ts";

type Element = { type: unknown; props: Record<string, any> };
const settle = () => new Promise<void>(resolve => setImmediate(resolve));
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function host(framework = "openai_agents") {
  const auth = { token: "fixture-auth", user: { user_id: "owner", roles: ["admin"], permissions: ["console:services:edit"], tier: "enterprise" } };
  let row = { service_id: "sdk-service", metadata: { adapter_type: framework,
    sdk_agent: { model: "old-model", instructions: "Original instructions", max_turns: 8 },
    sdk_tools: { knowledge_dataset_ids: ["kb"] }, sdk_provider_key_configured: true }, connector_config: {} };
  let reads = 0;
  let get = async () => structuredClone(row);
  const writes: Record<string, any>[] = [];
  let update = async (patch: Record<string, any>) => { row = { ...row, metadata: { ...patch.metadata, sdk_provider_key_configured: true } }; };
  const slots: any[] = []; let cursor = 0; const effects: (() => void | (() => void))[] = [];
  const element = (type: unknown, props: Record<string, any>, key?: unknown) => ({ type, props, key });
  const modules: Record<string, any> = {
    react: { useRef: (value: any) => { const i = cursor++; return slots[i] ?? (slots[i] = { current: value }); },
      useState: (initial: any) => { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (value: any) => { slots[i] = typeof value === "function" ? value(slots[i]) : value; }]; },
      useEffect: (effect: () => void | (() => void)) => { if (!effects.length) effects.push(effect); } },
    "react/jsx-runtime": { jsx: element, jsxs: element },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) },
    "@/lib/api": { api: {
      get: async (url: string) => { assert.equal(url, "/api/v1/services/sdk-service"); reads++; return { data: await get() }; },
      put: async (url: string, payload: Record<string, any>) => {
        assert.equal(url, "/api/v1/services/sdk-service");
        const patch = JSON.parse(JSON.stringify(payload)); writes.push(patch);
        return { data: await update(patch) };
      },
    } },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (value: any) => any) => select({ ...auth, hasPermission: () => true }), { getState: () => auth }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/sdkAgentSettings": settings,
  };
  for (const [name, component] of [["button", "Button"], ["input", "Input"], ["label", "Label"], ["textarea", "Textarea"]]) modules[`@/components/ui/${name}`] = { [component]: component };
  const gatewayExports: Record<string, any> = {};
  const gatewaySource = readFileSync(new URL("../api/gateway.ts", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(gatewaySource, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports: gatewayExports, require: (name: string) => { assert.ok(modules[name], name); return modules[name]; } });
  modules["@/api/gateway"] = gatewayExports;
  const exports: Record<string, any> = {};
  const source = readFileSync(new URL("./SdkAgentSettings.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (name: string) => { assert.ok(modules[name], name); return modules[name]; } });
  function render() { cursor = 0; return exports.SdkAgentSettingsFields({ serviceId: "sdk-service", canEdit: true }) as Element; }
  function elements(node: unknown, results: Element[] = []): Element[] {
    if (Array.isArray(node)) { node.forEach(child => elements(child, results)); return results; }
    if (node && typeof node === "object" && "props" in node) { const value = node as Element; results.push(value); elements(value.props.children, results); }
    return results;
  }
  function find(predicate: (value: Element) => boolean) { const found = elements(render()).find(predicate); assert.ok(found); return found; }
  const change = (id: string, value: string) => find(node => node.props.id === id).props.onChange({ target: { value } });
  const save = () => find(node => node.type === "form").props.onSubmit({ preventDefault() {} });
  render(); const cleanup = effects[0]();
  return { auth, writes, render, find, change, save, get reads() { return reads; },
    boundary: (serviceId = "sdk-service") => { cursor = 100; return exports.SdkAgentSettings({ serviceId }); },
    unmount: () => { if (typeof cleanup === "function") cleanup(); },
    reload: () => find(node => node.type === "Button" && node.props.type === "button").props.onClick(),
    setGet(value: typeof get) { get = value; }, setUpdate(value: typeof update) { update = value; } };
}
for (const framework of ["openai_agents", "claude_agent_sdk"]) {
  test(`${framework} production form loads, edits, saves and confirms real transport-shaped data`, async () => {
    const h = host(framework); await settle();
    assert.equal(h.find(node => node.props.id === "sdk-edit-model").props.value, "old-model");
    h.change("sdk-edit-model", "new-model"); h.change("sdk-edit-turns", "12");
    await h.save();
    assert.equal(h.writes.length, 1); assert.equal(h.reads, 2);
    assert.equal(h.writes[0].metadata.sdk_agent.model, "new-model");
    assert.equal(h.writes[0].metadata.adapter_type, framework);
    assert.deepEqual(h.writes[0].metadata.sdk_tools, { knowledge_dataset_ids: ["kb"] });
    assert.equal("connector_config" in h.writes[0], false);
    assert.match(h.find(node => node.props.role === "status").props.children, /accepted and read back/);
  });
}
test("replacement is explicit, cleared immediately, duplicate submission cannot replay", async () => {
  const h = host(); await settle(); const pending = deferred<void>(); h.setUpdate(() => pending.promise);
  h.change("sdk-edit-key", "fixture-replacement-key"); const saving = h.save();
  assert.equal(h.find(node => node.props.id === "sdk-edit-key").props.value, "");
  await h.save(); assert.equal(h.writes.length, 1);
  assert.equal(h.writes[0].connector_config.sdk_provider_api_key, "fixture-replacement-key");
  pending.resolve(); await saving;
});
test("unknown write blocks resend until explicit read; errors never expose key or server payload", async () => {
  const h = host(); await settle(); h.setUpdate(async () => { throw new Error("fixture-key-secret server body"); });
  h.change("sdk-edit-key", "fixture-replacement-key"); await h.save();
  assert.match(h.find(node => node.props.role === "alert").props.children, /may have been saved/);
  assert.equal(JSON.stringify(h.render()).includes("fixture-key-secret"), false);
  assert.equal(h.find(node => node.props.type === "submit").props.disabled, true);
  await h.save(); assert.equal(h.writes.length, 1);
  h.reload(); await settle(); assert.equal(h.find(node => node.props.id === "sdk-edit-model").props.value, "old-model");
});
test("accepted write with failed readback stays unconfirmed without automatic retry", async () => {
  const h = host(); await settle(); h.setGet(async () => { throw new Error("offline"); });
  await h.save(); assert.equal(h.writes.length, 1);
  assert.match(h.find(node => node.props.role === "alert").props.children, /accepted.*couldn't be read back/);
});
test("readback mismatch retains draft and requires reload", async () => {
  const h = host(); await settle(); h.setUpdate(async () => {}); h.change("sdk-edit-model", "new-draft");
  await h.save(); assert.match(h.find(node => node.props.role === "alert").props.children, /differ/);
  assert.equal(h.find(node => node.props.id === "sdk-edit-model").props.value, "new-draft");
});
for (const boundary of ["account", "unmount"]) {
  test(`late save after ${boundary} cannot read or publish another view`, async () => {
    const h = host(); await settle(); const pending = deferred<void>(); h.setUpdate(() => pending.promise);
    const saving = h.save(); if (boundary === "account") h.auth.token = "other-owner"; else h.unmount();
    pending.resolve(); await saving; assert.equal(h.reads, 1); await h.save(); assert.equal(h.writes.length, 1);
  });
}
test("invalid byte-length configuration is rejected without a PUT", async () => {
  const h = host(); await settle(); h.change("sdk-edit-model", "猫".repeat(54)); await h.save();
  assert.equal(h.writes.length, 0); assert.match(h.find(node => node.props.role === "alert").props.children, /No changes were sent/);
});

test("outer boundary remounts for credentials/permissions and never puts credentials in keys", async () => {
  const h = host(); await settle();
  const original = h.boundary();
  assert.equal(h.boundary().key, original.key);
  h.auth.token = "rotated-fixture-token";
  const rotated = h.boundary();
  assert.notEqual(rotated.key, original.key);
  h.auth.user.permissions = [];
  assert.notEqual(h.boundary().key, rotated.key);
  const permissionChanged = h.boundary();
  assert.notEqual(h.boundary("other-service").key, permissionChanged.key);
  assert.equal(JSON.stringify(h.boundary()).includes("rotated-fixture-token"), false);
});
test("late settings load after account change cannot fill draft or enable save", async () => {
  const h = host(); await settle(); const pending = deferred<any>();
  h.setGet(() => pending.promise); h.reload(); h.auth.token = "other-owner";
  pending.resolve({ service_id: "sdk-service", metadata: { adapter_type: "openai_agents", sdk_agent: {
    model: "late-private-model", instructions: "Private", max_turns: 8 } }, connector_config: {} });
  await settle(); assert.equal(JSON.stringify(h.render()).includes("late-private-model"), false);
  assert.equal(h.find(node => node.props.type === "submit").props.disabled, true);
});

test("existing shared service dialog embeds SDK editor only for the open official framework service", () => {
  const element = (type: unknown, props: Record<string, any>) => ({ type, props });
  let framework = "openai_agents";
  const modules: Record<string, any> = {
    react: { useState: (value: unknown) => [value, () => {}], useEffect: () => {},
      useMemo: (fn: () => unknown) => fn(), useCallback: (fn: unknown) => fn },
    "react/jsx-runtime": { jsx: element, jsxs: element },
    "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) },
    "@tanstack/react-query": { useQueryClient: () => ({}), useMutation: () => ({}),
      useQuery: ({ queryKey }: { queryKey: string[] }) => ({ data: queryKey[0] === "service-detail"
        ? { service_id: "selected-service", metadata: { adapter_type: framework } } : undefined }) },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => ({ id: "service-dialog-fixture", isCurrent: () => true }) },
    "@/lib/api": { api: {} }, "@/api/gateway": {}, "@/api/providers": { providerQueryKeys: { list: () => ["providers"] }, providersOptions: () => ({ queryKey: ["providers"], queryFn: () => [] }) }, "@/api/models": {},
    "@/lib/sdkAgentConfig": { isNativeAgentSdk: (value: unknown) => value === "openai_agents" || value === "claude_agent_sdk" },
    "@/components/SdkAgentSettings": { SdkAgentSettings: "SdkAgentSettings" },
  };
  const exports: Record<string, any> = {};
  const source = readFileSync(new URL("./ServiceConfigDialog.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (name: string) => {
      if (name.startsWith("@/components/ui/")) return new Proxy({}, { get: (_target, key) => String(key) });
      assert.ok(modules[name], name); return modules[name];
    } });
  const findEditor = (open = true) => {
    const tree = exports.ServiceConfigDialog({ serviceId: "selected-service", serviceName: "Selected", open, onOpenChange() {} });
    const matches: Element[] = [];
    const visit = (node: any) => { if (Array.isArray(node)) node.forEach(visit);
      else if (node?.props) { if (node.type === "SdkAgentSettings") matches.push(node); visit(node.props.children); } };
    visit(tree); return matches;
  };
  for (const value of ["openai_agents", "claude_agent_sdk"]) {
    framework = value; assert.equal(findEditor()[0].props.serviceId, "selected-service");
    assert.equal(findEditor(false).length, 0);
  }
  framework = "langgraph"; assert.equal(findEditor().length, 0);
});
