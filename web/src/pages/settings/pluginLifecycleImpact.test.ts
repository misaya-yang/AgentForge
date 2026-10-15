/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled component/transport fixture shapes. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import { AxiosError, isAxiosError } from "axios";
import { pluginLifecycleImpact, pluginLifecycleReviewKey } from "./pluginLifecycleImpact.ts";
import type { PluginCapability, PluginRegistryEntry } from "../../api/plugins.ts";

const capability = (name: string): PluginCapability => ({ id: name, name, version: "1", schema_hash: "sha256:fixture", category: "data", risk_level: "high", read_only: false, required_permissions: ["write"], declared_permissions: ["files"], available: false });
function plugin(): PluginRegistryEntry { return { id: "fixture-plugin", lifecycle: { managed: true, enabled: true, selected_version: "v1", revision: "9", selected_catalog_revision: "12", selection_status: "current", updated_at: null }, versions: [
  { version: "v1", availability: "available", required_permissions: ["write"], declared_permissions: ["files"], capabilities: [capability("old-tool")] },
  { version: "v2", availability: "policy_blocked", required_permissions: ["write"], declared_permissions: ["files"], capabilities: [capability("new-tool")] },
] }; }

test("real-shaped globally unique tool names show version additions/removals despite current denial", () => {
  const result = pluginLifecycleImpact(plugin(), "v2", true);
  assert.equal(result.knownBaseline, true);
  assert.deepEqual(result.entries.map(entry => [entry.name, entry.change]), [["new-tool", "added"], ["old-tool", "removed"]]);
  assert.equal(result.entries[0].after?.available, false);
});
test("disabled known baseline is empty; disabling enabled selection removes visible tools", () => {
  const row = plugin(); row.lifecycle.enabled = false;
  assert.deepEqual(pluginLifecycleImpact(row, "v2", true).entries.map(entry => entry.change), ["added"]);
  assert.deepEqual(pluginLifecycleImpact(plugin(), "v2", false).entries.map(entry => entry.change), ["removed"]);
});
test("deployment default, missing version and changed catalog keep previous state unknown", () => {
  for (const status of ["deployment_default", "version_unavailable", "catalog_changed"] as const) {
    const row = plugin(); row.lifecycle.selection_status = status; row.lifecycle.managed = status !== "deployment_default";
    const result = pluginLifecycleImpact(row, "v2", true);
    assert.equal(result.knownBaseline, false); assert.deepEqual(result.entries.map(entry => entry.change), ["proposed"]);
    assert.equal(result.entries[0].before, undefined);
  }
});
test("missing target cannot be reviewed as an enabled empty set", () => {
  assert.equal(pluginLifecycleImpact(plugin(), "absent", true).targetKnown, false);
  assert.equal(pluginLifecycleImpact(plugin(), "absent", false).targetKnown, true);
});
test("same-selection descriptor is unchanged; review key binds authority, CAS and all rendered metadata", () => {
  const row = plugin(); assert.equal(pluginLifecycleImpact(row, "v1", true).entries[0].change, "unchanged");
  const key = pluginLifecycleReviewKey(row, "v2", true, "tenant-a", "12");
  for (const alter of [(p: PluginRegistryEntry) => { p.versions[1].capabilities[0].risk_level = "unknown"; }, (p: PluginRegistryEntry) => { p.versions[1].capabilities[0].schema_hash = "different"; }, (p: PluginRegistryEntry) => { p.lifecycle.revision = "10"; }, (p: PluginRegistryEntry) => { p.versions[1].capabilities[0].available = true; }]) {
    const next = structuredClone(row); alter(next); assert.notEqual(pluginLifecycleReviewKey(next, "v2", true, "tenant-a", "12"), key);
  }
  for (const args of [["v1", true, "tenant-a", "12"], ["v2", false, "tenant-a", "12"], ["v2", true, "tenant-b", "12"], ["v2", true, "tenant-a", "13"]] as const) assert.notEqual(pluginLifecycleReviewKey(row, ...args), key);
});

type Tree = { type: unknown; props: Record<string, any> };
function nodes(value: any): Tree[] { if (!value || typeof value !== "object") return []; if (Array.isArray(value)) return value.flatMap(nodes); return [value, ...nodes(value.props?.children)]; }
function host() {
  const slots: any[] = []; let cursor = 0; let dirty = true; let tree: Tree; const effects: (() => void)[] = [];
  let props: any = { plugin: plugin(), tenantId: "tenant-a", catalogRevision: "12", canManage: true, stale: false, onSaved: async () => true };
  const writes: any[] = []; let write: (...args: any[]) => Promise<any> = async () => ({});
  const react = { useState(initial: any) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (next: any) => { slots[i] = typeof next === "function" ? next(slots[i]) : next; dirty = true; }]; }, useRef(initial: any) { const i = cursor++; if (!(i in slots)) slots[i] = { current: initial }; return slots[i]; }, useEffect(fn: () => void, deps: any[]) { const i = cursor++; if (!slots[i] || deps.some((value, n) => value !== slots[i][n])) { slots[i] = deps; effects.push(fn); } } };
  const jsx = (type: any, props: any) => ({ type, props });
  const exports: any = {};
  const source = readFileSync(new URL("./PluginLifecycleControls.tsx", import.meta.url), "utf8");
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  const require = (name: string) => ({ react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) }, axios: { isAxiosError }, "@/api/plugins": { updatePluginLifecycle: async (...args: any[]) => { writes.push(args); return write(...args); } }, "@/components/ui/button": { Button: "button" }, "./PluginImpactReview": { PluginImpactReview: "impact" }, "./pluginLifecycleImpact": { pluginLifecycleReviewKey } }[name]);
  vm.runInNewContext(code, { exports, require });
  const recovery: any = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./usePluginLifecycleRecovery.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, { exports: recovery, require });
  const render = () => { do { dirty = false; cursor = 0;
    const owner = recovery.usePluginLifecycleRecovery({ isCurrent: () => true, registryReady: () => true,
      registry: () => ({ tenant_id: props.tenantId, can_manage: props.canManage, catalog_revision_token: props.catalogRevision, plugins: [props.plugin] }),
      refreshRegistry: props.onSaved, invalidateTools() {} });
    tree = exports.PluginLifecycleControls({ ...props, mutation: owner.mutations.get(props.plugin.id), onSave: (payload: any, key: string) => owner.save(props.plugin.id, payload, key) }); while (effects.length) effects.shift()!(); } while (dirty); return nodes(tree); };
  const find = (predicate: (n: Tree) => boolean) => { const result = render().find(predicate); assert.ok(result); return result; };
  return { writes, render, setProps(next: any) { props = { ...props, ...next }; dirty = true; render(); }, setWrite(next: typeof write) { write = next; },
    choose(value = "v2") { find(n => n.type === "select").props.onChange({ target: { value } }); render(); },
    enable(value: boolean) { find(n => n.type === "input" && n.props.type === "checkbox").props.onChange({ target: { checked: value } }); render(); },
    review() { find(n => n.type === "form").props.onSubmit({ preventDefault() {} }); render(); },
    acknowledge() { find(n => n.type === "impact").props.onAcknowledge(true); render(); },
    save() { return find(n => n.type === "button" && n.props.children === "pluginRegistry.lifecycle.save"); },
    cancel() { find(n => n.type === "button" && n.props.children === "common.cancel").props.onClick(); render(); } };
}
async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); }
test("production lifecycle form requires exact acknowledgement then sends original tenant/catalog/revision CAS", async () => {
  const h = host(); h.choose(); h.review(); assert.equal(h.save().props.disabled, true);
  h.save().props.onClick(); await settle(); assert.equal(h.writes.length, 0);
  h.acknowledge(); assert.equal(h.save().props.disabled, false); const submit = h.save().props.onClick; submit(); submit(); await settle();
  assert.equal(h.writes.length, 1); assert.deepEqual(JSON.parse(JSON.stringify(h.writes[0])), ["fixture-plugin", { selected_version: "v2", enabled: true, expected_revision: "9", expected_catalog_revision: "12", expected_tenant_id: "tenant-a" }]);
});
test("cancel and candidate reset discard acknowledgement", () => {
  const h = host(); h.choose(); h.review(); h.acknowledge(); h.cancel(); h.review(); assert.equal(h.save().props.disabled, true);
  h.acknowledge(); h.choose("v1"); h.choose("v2"); assert.equal(h.render().some(n => n.type === "impact"), false); h.review(); assert.equal(h.save().props.disabled, true);
  h.acknowledge(); h.enable(false); h.enable(true); assert.equal(h.render().some(n => n.type === "impact"), false); h.review(); assert.equal(h.save().props.disabled, true);
});
test("descriptor or tenant transitions invalidate review permanently even when reverted", () => {
  for (const kind of ["tenant", "descriptor"]) {
    const h = host(); h.choose(); h.review(); h.acknowledge();
    if (kind === "tenant") { h.setProps({ tenantId: "tenant-b" }); h.setProps({ tenantId: "tenant-a" }); }
    else { const changed = plugin(); changed.versions[1].capabilities[0].read_only = true; h.setProps({ plugin: changed }); h.setProps({ plugin: plugin() }); }
    assert.equal(h.render().some(n => n.type === "impact"), false);
    h.review(); assert.equal(h.save().props.disabled, true); assert.equal(h.writes.length, 0);
  }
});
test("unknown save response retains existing refresh-required lock with no automatic replay", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("synthetic unknown response"); }); h.choose(); h.review(); h.acknowledge(); h.save().props.onClick(); await settle();
  assert.equal(h.writes.length, 1); assert.equal(h.save().props.disabled, true); h.save().props.onClick(); await settle(); assert.equal(h.writes.length, 1);
});

test("production review renders complete before/after descriptor metadata and explicit uncertainty", () => {
  const jsx = (type: any, props: any): any => typeof type === "function" ? type(props) : ({ type, props });
  const exports: any = {};
  const code = ts.transpileModule(readFileSync(new URL("./PluginImpactReview.tsx", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  vm.runInNewContext(code, { exports, require: (name: string) => ({ "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) }, "./pluginLifecycleImpact": { pluginLifecycleImpact } }[name]) });
  const render = (row: PluginRegistryEntry) => exports.PluginImpactReview({ plugin: row, version: "v2", enabled: true, acknowledged: false, onAcknowledge() {}, disabled: false });
  const row = plugin(); row.versions[1].capabilities[0].version = "null";
  const text = JSON.stringify(render(row));
  for (const value of ["old-tool", "new-tool", "sha256:fixture", "high", "write", "files", "pluginRegistry.mayWrite", "pluginRegistry.impact.added", "pluginRegistry.impact.removed", "pluginRegistry.impact.policyObservation", "pluginRegistry.impact.scope", "pluginRegistry.impact.unspecifiedVersion"]) assert.ok(text.includes(value), value);
  row.lifecycle.selection_status = "catalog_changed";
  const unknown = JSON.stringify(render(row));
  assert.ok(unknown.includes("pluginRegistry.impact.unknownBaseline")); assert.ok(unknown.includes("pluginRegistry.impact.proposed")); assert.ok(!unknown.includes("old-tool"));
  const absent = exports.PluginImpactReview({ plugin: row, version: "absent", enabled: true, acknowledged: false, onAcknowledge() {}, disabled: false });
  assert.equal(nodes(absent).find(n => n.type === "input")?.props.disabled, true);
});

for (const [status, message] of [[403, "forbidden"], [409, "conflict"], [503, "uncertain"]] as const) {
  test(`production lifecycle classifies Axios ${status} and locks replay until registry refresh`, async () => {
    const h = host(); let reads = 0;
    h.setProps({ onSaved: async () => { reads++; return true; } });
    const error = new AxiosError("controlled transport failure");
    error.response = { status, statusText: "controlled", data: {}, headers: {}, config: {} as any };
    h.setWrite(async () => { throw error; });
    h.choose(); h.review(); h.acknowledge(); h.save().props.onClick(); await settle();
    assert.equal(h.render().find(n => n.props.role === "alert")?.props.children, `pluginRegistry.lifecycle.${message}`);
    assert.equal(h.save().props.disabled, true);
    assert.equal(h.render().find(n => n.type === "select")?.props.value, "v2");
    assert.equal(reads, 0);
    h.save().props.onClick(); await settle(); assert.equal(h.writes.length, 1);
  });
}
test("production lifecycle classifies non-Axios rejection as uncertain without replay", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("controlled network failure"); });
  h.choose(); h.review(); h.acknowledge(); h.save().props.onClick(); await settle();
  assert.equal(h.render().find(n => n.props.role === "alert")?.props.children, "pluginRegistry.lifecycle.uncertain");
  assert.equal(h.save().props.disabled, true); h.save().props.onClick(); await settle(); assert.equal(h.writes.length, 1);
});
test("confirmed lifecycle write with failed reread reports saved separately and keeps controls locked", async () => {
  const h = host(); let reads = 0;
  h.setProps({ onSaved: async () => { reads++; return false; } });
  h.choose(); h.review(); h.acknowledge(); h.save().props.onClick(); await settle();
  assert.equal(h.writes.length, 1); assert.equal(reads, 1);
  assert.equal(h.render().find(n => n.props.role === "status")?.props.children, "pluginRegistry.lifecycle.savedRefreshFailed");
  assert.equal(h.render().some(n => n.props.role === "alert"), false);
  assert.equal(h.render().find(n => n.type === "select")?.props.disabled, true);
  h.review(); assert.equal(h.render().some(n => n.type === "impact"), false);
  assert.equal(h.writes.length, 1);
});
