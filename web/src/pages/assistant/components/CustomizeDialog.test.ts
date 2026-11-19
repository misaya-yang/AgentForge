/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled React frames and deferred HTTP collaborators, matching the repository's component harnesses. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: any; props: Record<string, any>; key?: string };
type Frame = { slots: any[]; effects: (() => void)[]; cleanup: (() => void)[] };
type Request = { method: string; path: string; resolve: (value: any) => void };
const translate = (key: string) => key;
const settle = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
function nodes(value: any): Tree[] {
  if (!value || typeof value !== "object") return [];
  if (Array.isArray(value)) return value.flatMap(nodes);
  return [value, ...nodes(value.props?.children)];
}

// Execute the production component, API wrappers, authority key, and read-scope
// hook. Only React scheduling, native dialogs, and HTTP completion are controlled.
function host() {
  let auth = { token: "fixture-A", isAuthenticated: true, user: { user_id: "user-A", tenant_id: "tenant-A", roles: ["member"], permissions: ["read", "write"], tier: "normal" } };
  const subscribers = new Set<(next: typeof auth, previous: typeof auth) => void>();
  const frames = new Map<string, Frame>();
  const requests: Request[] = [];
  let frame!: Frame;
  let index = 0, identity = 0, closed = 0;
  let dirty = false, open = true;
  let tree: Tree | null = null;
  let seen = new Set<string>();
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((value, i) => Object.is(value, b?.[i]));
  const react = {
    useState(initial: any) {
      const owner = frame, slot = index++;
      if (!(slot in owner.slots)) owner.slots[slot] = typeof initial === "function" ? initial() : initial;
      return [owner.slots[slot], (value: any) => {
        const next = typeof value === "function" ? value(owner.slots[slot]) : value;
        if (!Object.is(next, owner.slots[slot])) { owner.slots[slot] = next; dirty = true; }
      }];
    },
    useRef(initial: any) { const slot = index++; return frame.slots[slot] ??= { current: initial }; },
    useMemo(compute: () => any, deps: unknown[]) {
      const slot = index++, prior = frame.slots[slot];
      if (!prior || !same(prior.deps, deps)) frame.slots[slot] = { deps, value: compute() };
      return frame.slots[slot].value;
    },
    useCallback(callback: any, deps: unknown[]) { return react.useMemo(() => callback, deps); },
    useEffect(run: () => (() => void) | void, deps: unknown[]) {
      const owner = frame, slot = index++;
      if (!same(owner.slots[slot], deps)) {
        owner.slots[slot] = deps;
        owner.effects.push(() => { owner.cleanup[slot]?.(); owner.cleanup[slot] = run() || (() => {}); });
      }
    },
  };
  const jsx = (type: any, props: any, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_target, name) => String(name) });
  const modules: Record<string, any> = {
    react: { ...react, useLayoutEffect: react.useEffect }, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: translate }) }, "lucide-react": named,
    "@/lib/utils": { generateUUID: () => `scope-${++identity}` },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), {
      getState: () => auth,
      subscribe: (listener: (next: typeof auth, previous: typeof auth) => void) => { subscribers.add(listener); return () => subscribers.delete(listener); },
    }) },
    "@/lib/api": { api: Object.fromEntries(["get", "post", "delete"].map(method => [method,
      (path: string) => new Promise(resolve => requests.push({ method, path, resolve: data => resolve({ data }) }))])) },
  };
  function compile(path: string) {
    const exports: any = {};
    const code = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    vm.runInNewContext(code, { exports, Error, JSON, confirm: () => true, require: (name: string) => {
      if (name.startsWith("@/components/ui/")) return named;
      assert.ok(name in modules, name); return modules[name];
    } });
    return exports;
  }
  modules["@/lib/sdkAuthorityScope"] = compile("../../../lib/sdkAuthorityScope.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = compile("../../../hooks/useAuthenticatedReadScope.ts");
  modules["@/api/skills"] = compile("../../../api/skills.ts");
  modules["@/api/mcp"] = compile("../../../api/mcp.ts");
  const component = compile("./CustomizeDialog.tsx").CustomizeDialog;
  function expand(node: any, path: string): any {
    if (!node || typeof node !== "object") return node;
    if (Array.isArray(node)) return node.map((child, i) => expand(child, `${path}:${child?.key ?? i}`));
    const key = `${path}:${node.type?.name ?? node.type}:${node.key ?? ""}`;
    if (typeof node.type === "function") {
      seen.add(key); frame = frames.get(key) ?? { slots: [], effects: [], cleanup: [] };
      frames.set(key, frame); index = 0; return expand(node.type(node.props), key);
    }
    return { ...node, props: { ...node.props, children: expand(node.props.children, `${key}:children`) } };
  }
  function render() {
    for (let pass = 0; pass < 15; pass++) {
      dirty = false; seen = new Set();
      tree = expand(jsx(component, { open, onClose: () => { closed++; open = false; } }), "root");
      for (const [key, owner] of frames) {
        if (!seen.has(key)) { owner.cleanup.forEach(cleanup => cleanup?.()); frames.delete(key); }
        else owner.effects.splice(0).forEach(effect => effect());
      }
      if (!dirty) return tree;
    }
    throw new Error("Component render did not settle");
  }
  return {
    requests, render,
    auth(change: Partial<typeof auth>) { const previous = auth; auth = { ...auth, ...change }; subscribers.forEach(listener => listener(auth, previous)); },
    currentAuth: () => auth,
    label: () => JSON.stringify(tree),
    one(predicate: (node: Tree) => boolean) { const value = nodes(tree).find(predicate); assert.ok(value); return value; },
    closed: () => closed,
    reopen() { open = true; render(); },
  };
}

function completeReads(requests: Request[], marker: string) {
  for (const request of requests) {
    if (request.path === "/api/v1/skills") request.resolve({ skills: [{ name: marker, title: marker, summary: marker, tags: [], source: "user", enabled: true }], total: 1 });
    else if (request.path.endsWith("/servers")) request.resolve({ servers: [{ name: marker, description: marker, connected: true, tool_count: 1 }] });
    else if (request.path.endsWith("/tools")) request.resolve({ tools: [{ name: `mcp_${marker}__read`, description: marker }] });
    else assert.fail(`Not a metadata read: ${request.path}`);
  }
}

test("tenant/user replacement discards cached metadata and ignores late Skills/MCP reads", async () => {
  for (const boundary of ["tenant", "user"]) {
    for (const cached of [false, true]) {
      const h = host(); h.render();
      const late = h.requests.slice();
      if (cached) {
        completeReads([late[0]], "private-A"); await settle(); h.render();
        assert.match(h.label(), /private-A/);
      }
      const current = h.currentAuth();
      h.auth({ user: { ...current.user, ...(boundary === "tenant" ? { tenant_id: "tenant-B" } : { user_id: "user-B" }) } });
      h.render();
      assert.equal(h.requests.length, 6);
      assert.doesNotMatch(h.label(), /private-A/);
      completeReads(late, "private-A"); await settle(); h.render();
      assert.doesNotMatch(h.label(), /private-A/);
      completeReads(h.requests.slice(3), "private-B"); await settle(); h.render();
      assert.match(h.label(), /private-B/); assert.doesNotMatch(h.label(), /private-A/);
    }
  }
});

test("synchronous auth ABA retires captured Skill mutations, MCP refresh, and old close callbacks", async () => {
  const h = host(); h.render(); completeReads(h.requests, "private-A"); await settle(); h.render();
  const toggle = h.one(node => node.type === "Switch").props.onCheckedChange;
  const refresh = h.one(node => node.type === "Button" && nodes(node.props.children).some(child => child.type === "RefreshCw")).props.onClick;
  const close = h.one(node => node.type === "Dialog").props.onOpenChange;
  h.auth({ token: "fixture-B" }); h.auth({ token: "fixture-A" });
  toggle(false); refresh({ stopPropagation() {} }); close(false);
  assert.equal(h.requests.length, 3); assert.equal(h.closed(), 0);
  h.render(); assert.doesNotMatch(h.label(), /private-A/); assert.equal(h.requests.length, 6);
});

test("retired mutations cannot start a new-authority read or restore metadata after close", async () => {
  const h = host(); h.render(); completeReads(h.requests, "private-A"); await settle(); h.render();
  h.one(node => node.type === "Button" && nodes(node.props.children).some(child => child.type === "RefreshCw")).props.onClick({ stopPropagation() {} });
  const pending = h.requests.at(-1)!;
  assert.equal(pending.method, "post");
  const toggle = h.one(node => node.type === "Switch").props.onCheckedChange;
  h.one(node => node.type === "Dialog").props.onOpenChange(false);
  toggle(false); assert.equal(h.requests.length, 4);
  h.render(); h.reopen(); assert.equal(h.requests.length, 7);
  pending.resolve({ tools_refreshed: 1 }); await settle(); h.render();
  assert.equal(h.requests.length, 7); assert.doesNotMatch(h.label(), /private-A/);
  completeReads(h.requests.slice(4), "reopened-current"); await settle(); h.render();
  assert.match(h.label(), /reopened-current/); assert.doesNotMatch(h.label(), /private-A/);
});
