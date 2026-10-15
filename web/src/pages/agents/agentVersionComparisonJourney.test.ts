import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createDefaultAgentSpec } from "../../types/agents.ts";
import * as comparison from "./agentVersionComparisonState.ts";

// Execute the production components and comparison code. React scheduling, UI
// primitives and existing API reads are controlled; no HTTP or browser runs.
type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
function all(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => all(child, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...all(node.props?.children, predicate), ...all(node.props?.description, predicate)];
}
function text(tree: unknown): string {
  if (tree === null || tree === undefined || typeof tree === "boolean") return "";
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  const node = tree as Tree;
  return [node.props?.title, node.props?.description, node.props?.children].map(text).join(" ");
}
function deferred<T>() {
  let resolve!: (value: T) => void; let reject!: (value: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function version(id: string, instructions: string) {
  return { tenant_id: "tenant-a", agent_id: "agent-a", agent_version_id: id, version_number: id === "v1" ? 1 : 2,
    schema_version: "agent-spec/v1", spec: { ...createDefaultAgentSpec(), instructions }, spec_hash: `hash-${id}`,
    source_draft_id: "draft-a", source_draft_revision: id === "v1" ? 1 : 2, created_by: "user-a", created_at: "2026-10-13T00:00:00Z" };
}
const fixtureRows = () => [version("v2", "New instructions"), version("v1", "Old instructions")];
const prefix = "agents.studio.versionComparison";
function host(mode: "content" | "entry" | "wrapper" | "release" = "content") {
  const slots: unknown[] = []; const cleanups: Array<() => void> = [];
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  let cursor = 0; let effects: Array<() => void> = []; let changed = false; let current = true; let uuid = 0;
  let agentId = "agent-a"; let releaseMode = "versions";
  let auth = { token: "opaque-token-a", isAuthenticated: true, user: { user_id: "user-a", permissions: ["agents:read"], roles: ["member"], tier: "normal" } };
  let readAgent = async () => ({ agent_id: agentId, tenant_id: "tenant-a" });
  let readVersions = async () => fixtureRows();
  const reads: string[] = []; const mutations: unknown[] = [];
  const react = {
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
      return [slots[index], (next: unknown) => {
        const value = typeof next === "function" ? next(slots[index]) : next;
        if (!Object.is(slots[index], value)) { slots[index] = value; changed = true; }
      }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useMemo(make: () => unknown, deps: unknown[]) {
      const index = cursor++; const prior = slots[index] as { value: unknown; deps: unknown[] } | undefined;
      if (!prior || deps.some((value, i) => !Object.is(value, prior.deps[i]))) slots[index] = { value: make(), deps };
      return (slots[index] as { value: unknown }).value;
    },
    useLayoutEffect(effect: () => void | (() => void), deps: unknown[]) {
      const index = cursor++; const prior = slots[index] as unknown[] | undefined;
      if (!prior || deps.some((value, i) => !Object.is(value, prior[i]))) {
        slots[index] = deps; effects.push(() => { const cleanup = effect(); if (cleanup) cleanups.push(cleanup); });
      }
    },
  };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, key) => String(key) });
  const authority = (user: typeof auth.user) => JSON.stringify([user?.user_id, user?.permissions, user?.roles, user?.tier]);
  const authStore = Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth,
    subscribe: (listener: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(listener); return () => subscribers.delete(listener); } });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { ...Object.fromEntries(["Alert", "Button", "Checkbox", "Drawer", "Empty", "Select", "Tag", "Skeleton", "Modal", "Timeline", "Input"].map(key => [key, key])), Typography: named, App: { useApp: () => ({ message: {} }) } },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { resolvedLanguage: "en-US" } }) },
    "@/api/agents": { getAgent: (id: string) => { reads.push(`agent:${id}`); return readAgent(); }, listAgentVersions: (id: string) => { reads.push(`versions:${id}`); return readVersions(); } },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++uuid}` },
    "@/store/useAuthStore": { useAuthStore: authStore }, "./agentVersionComparisonState": comparison,
    "@tanstack/react-query": { useQuery: () => ({ data: [], isLoading: false }), useQueryClient: () => ({}),
      useMutation: () => ({ mutate: (value: unknown) => mutations.push(value) }) },
    "@/api/eval": {}, "./agentReleaseExpiry": {}, "./AgentVersionComparison": { AgentVersionComparison: "AgentVersionComparison" }, "./ApiTokenPanel": { ApiTokenPanel: "ApiTokenPanel" },
  };
  const compiled: Record<string, (props: unknown) => Tree> = {};
  const source = readFileSync(new URL(mode === "release" ? "./AgentReleasePanel.tsx" : "./AgentVersionComparison.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source + (mode === "release" ? "" : "\nexports.Content = VersionComparisonContent; exports.Entry = ComparisonEntry;"), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports: compiled, require: (id: string) => { assert.ok(id in modules, `Unexpected import ${id}`); return modules[id]; } });
  const h = {
    reads, mutations,
    render() {
      for (let pass = 0; pass < 20; pass++) {
        cursor = 0; effects = []; changed = false;
        const tree = mode === "content" ? compiled.Content({ agentId, isCurrent: () => current })
          : mode === "entry" ? compiled.Entry({ agentId, isCurrent: () => current })
            : mode === "wrapper" ? compiled.AgentVersionComparison({ agentId })
              : compiled.AgentReleasePanel({ mode: releaseMode, agentId, draftRevision: 2, dirty: false, role: "viewer", versions: fixtureRows(), datasets: [] });
        effects.forEach(effect => effect());
        if (!changed) return tree;
      }
      throw new Error("Component did not settle");
    },
    async flush() { for (let i = 0; i < 12; i++) { await Promise.resolve(); h.render(); } },
    find(predicate: (node: Tree) => boolean) { return all(h.render(), predicate); },
    select(side: "before" | "after", value: string) {
      const node = h.find(node => node.type === "Select" && node.props["aria-label"] === `${prefix}.${side}`)[0];
      assert.ok(node, side); (node.props.onChange as (value: string) => void)(value); h.render();
    },
    choose() { h.select("before", "v1"); h.select("after", "v2"); },
    button(key: string) { const node = h.find(node => node.type === "Button" && node.props.children === `${prefix}.${key}`)[0]; assert.ok(node, key); return node; },
    click(key: string) { const node = h.button(key); assert.ok(!node.props.disabled); (node.props.onClick as () => void)(); h.render(); },
    content() { return text(h.render()); },
    visibleValues() { return h.find(node => node.type === "pre").map(node => node.props.children); },
    unmount() { cleanups.splice(0).forEach(cleanup => cleanup()); },
    setCurrent(value: boolean) { current = value; },
    setAgent(value: string) { agentId = value; },
    setAuth(value: Partial<typeof auth>) { const previous = auth; auth = { ...auth, ...value }; subscribers.forEach(listener => listener(auth, previous)); },
    auth() { return auth; },
    setReadVersions(value: typeof readVersions) { readVersions = value; },
    setReadAgent(value: typeof readAgent) { readAgent = value; },
    releaseMode(value: string) { releaseMode = value; },
  };
  return h;
}

test("drawer reads nothing before explicit opening; close removes the comparison content", () => {
  const h = host("entry"); h.render(); assert.deepEqual(h.reads, []);
  assert.equal(h.find(node => typeof node.type === "function").length, 0);
  h.click("open"); assert.equal(h.find(node => typeof node.type === "function").length, 1);
  const drawer = h.find(node => node.type === "Drawer")[0]; (drawer.props.onClose as () => void)();
  assert.equal(h.find(node => typeof node.type === "function").length, 0);
  assert.deepEqual(h.mutations, []);
});
test("current authorized GETs populate exact-ID choices without choosing a pair automatically", async () => {
  const h = host(); h.render(); await h.flush();
  assert.deepEqual(h.reads, ["agent:agent-a", "versions:agent-a"]);
  assert.deepEqual(h.visibleValues(), []);
  const select = h.find(node => node.type === "Select")[0];
  assert.deepEqual(Array.from(select.props.options as { value: string }[], option => option.value), ["v2", "v1"]);
  h.choose(); assert.deepEqual(h.visibleValues(), ["Old instructions", "New instructions"]);
  assert.ok(h.content().includes("hash-v1")); assert.ok(h.content().includes("hash-v2"));
  assert.deepEqual(h.mutations, []);
});
test("swap reverses the exact selected snapshots without another read or mutation", async () => {
  const h = host(); h.render(); await h.flush(); h.choose(); h.click("swap");
  assert.deepEqual(h.visibleValues(), ["New instructions", "Old instructions"]);
  assert.equal(h.reads.length, 2); assert.deepEqual(h.mutations, []);
});
test("same-ID selection is distinct from two different IDs with unchanged configuration", async () => {
  const h = host(); h.setReadVersions(async () => [version("v1", "Same"), version("v2", "Same")]);
  h.render(); await h.flush(); h.choose(); assert.ok(h.content().includes(`${prefix}.noChanges`));
  h.select("after", "v1"); assert.ok(h.content().includes(`${prefix}.sameSelection`));
  assert.deepEqual(h.visibleValues(), []);
});
test("unchanged switch reveals saved known fields, never opaque configuration values", async () => {
  const h = host(); const rows = fixtureRows();
  rows[0].spec.memory.hidden = "private-marker";
  rows[0].spec.capabilities = [{ type: "connector", resource_id: "mail", config: { access_token: "secret-marker" } }];
  h.setReadVersions(async () => rows); h.render(); await h.flush(); h.choose();
  const checkbox = h.find(node => node.type === "Checkbox")[0]; (checkbox.props.onChange as (event: unknown) => void)({ target: { checked: true } });
  assert.ok(h.visibleValues().includes("dashscope"));
  assert.ok(!h.content().includes("private-marker")); assert.ok(!h.content().includes("secret-marker"));
  assert.ok(h.content().includes(`${prefix}.withheld`));
});
test("refresh hides old content while reading and never substitutes a missing selected version", async () => {
  const h = host(); h.render(); await h.flush(); h.choose();
  const next = deferred<ReturnType<typeof fixtureRows>>(); h.setReadVersions(() => next.promise); h.click("refresh");
  assert.deepEqual(h.visibleValues(), []); assert.ok(h.content().includes(`${prefix}.loading`));
  next.resolve([version("v2", "New instructions"), version("v3", "Third")]); await h.flush();
  assert.ok(h.content().includes(`${prefix}.selectionUnavailable`)); assert.deepEqual(h.visibleValues(), []);
  h.select("before", "v3"); assert.deepEqual(h.visibleValues(), ["Third", "New instructions"]);
});
test("read failure is distinct from an empty catalog; explicit retry is read-only and sanitized", async () => {
  const h = host(); h.setReadVersions(async () => { throw new Error("credential-error-secret"); });
  h.render(); await h.flush(); assert.ok(h.content().includes(`${prefix}.readError`));
  assert.ok(!h.content().includes("credential-error-secret")); assert.ok(!h.content().includes(`${prefix}.needTwo`));
  h.setReadVersions(async () => []); h.click("refresh"); await h.flush();
  assert.ok(h.content().includes(`${prefix}.needTwo`)); assert.deepEqual(h.mutations, []);
});
test("a mismatched tenant or Agent response fails closed without any returned snapshot text", async () => {
  for (const field of ["tenant_id", "agent_id"] as const) {
    const h = host(); h.setReadVersions(async () => fixtureRows().map(row => ({ ...row, [field]: "different" })));
    h.render(); await h.flush(); assert.ok(h.content().includes(`${prefix}.readError`)); assert.deepEqual(h.visibleValues(), []);
  }
});
test("newer refresh wins over an older late success or failure", async () => {
  const h = host(); const old = deferred<ReturnType<typeof fixtureRows>>(); h.setReadVersions(() => old.promise);
  h.render(); h.setReadVersions(async () => fixtureRows());
  // Force a second read through the actual handler to exercise response fencing.
  (h.button("refresh").props.onClick as () => void)(); await h.flush(); h.choose();
  old.reject(new Error("old failure")); await h.flush();
  assert.deepEqual(h.visibleValues(), ["Old instructions", "New instructions"]);
  assert.ok(!h.content().includes(`${prefix}.readError`));
});
test("identity loss removes comparison immediately and discards a late authorized read", async () => {
  const h = host(); h.render(); await h.flush(); h.choose(); h.setCurrent(false);
  assert.deepEqual(h.visibleValues(), []);
  const late = host(); const pending = deferred<ReturnType<typeof fixtureRows>>(); late.setReadVersions(() => pending.promise);
  late.render(); late.setCurrent(false); pending.resolve(fixtureRows()); await late.flush();
  assert.deepEqual(late.visibleValues(), []); assert.equal(late.find(node => node.type === "Select").length, 0);
});
test("unmount rejects late data and a newly opened comparison starts unselected", async () => {
  const h = host(); const pending = deferred<ReturnType<typeof fixtureRows>>(); h.setReadVersions(() => pending.promise); h.render(); h.unmount();
  pending.resolve(fixtureRows()); await h.flush(); assert.deepEqual(h.visibleValues(), []);
  const reopened = host(); reopened.render(); await reopened.flush(); assert.deepEqual(reopened.visibleValues(), []);
});
test("Agent, token, user and authority changes remount with opaque keys; logout removes the entry", () => {
  const h = host("wrapper"); let previous = h.render();
  assert.ok(!JSON.stringify(previous).includes("opaque-token"));
  for (const change of [() => h.setAgent("agent-b"), () => h.setAuth({ token: "opaque-token-b" }),
    () => h.setAuth({ user: { ...h.auth().user, user_id: "user-b" } }),
    () => h.setAuth({ user: { ...h.auth().user, permissions: [] } })]) {
    const oldCheck = previous.props.isCurrent as () => boolean; change(); const next = h.render();
    assert.notEqual(next.key, previous.key);
    if (h.auth().token !== "opaque-token-a") assert.equal(oldCheck(), false);
    previous = next;
  }
  h.setAuth({ isAuthenticated: false }); assert.equal(h.render(), null);
});
test("actual release panel exposes historical comparison for viewers without altering draft release flow", () => {
  const h = host("release");
  const nodes = h.find(node => node.type === "AgentVersionComparison"); assert.equal(nodes.length, 1); assert.equal(nodes[0].props.agentId, "agent-a");
  h.releaseMode("eval"); assert.equal(h.find(node => node.type === "AgentVersionComparison").length, 0);
  assert.deepEqual(h.mutations, []);
});


test("batched account A to B to A cannot revive an old read observation", () => {
  const h = host("wrapper"); const original = h.auth(); const before = h.render();
  const oldCheck = before.props.isCurrent as () => boolean;
  assert.equal(oldCheck(), true);
  h.setAuth({ token: "other-token", user: { ...original.user, user_id: "other-user" } });
  h.setAuth(original); // No React render occurred between store notifications.
  assert.equal(oldCheck(), false);
  const after = h.render(); assert.notEqual(after.key, before.key);
  assert.equal((after.props.isCurrent as () => boolean)(), true);
  h.unmount();
});
