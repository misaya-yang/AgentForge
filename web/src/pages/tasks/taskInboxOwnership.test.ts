import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { createStore } from "zustand/vanilla";
import * as navigation from "./taskNavigation.ts";

type Tree = { type: unknown; props: Record<string, unknown> };
type Scope = { id: string; isCurrent: () => boolean };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
type Auth = { token: string | null; isAuthenticated: boolean; user: { user_id: string; permissions: string[]; roles: string[]; tier: string } | null };
type Options = { queryKey: unknown[]; queryFn: (context: { signal: AbortSignal }) => Promise<unknown>; enabled?: boolean };
type ListState = { data?: unknown[]; isLoading?: boolean; isFetching?: boolean; isError?: boolean; refetch?: () => void };
const receipt = (id: string) => ({ task_id: id, status: "completed", service_id: "existing-service", created_at: "2026-10-12T00:00:00Z" });
function nodes(value: unknown): Tree[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const tree = value as Tree; return [tree, ...nodes(tree.props?.children)];
}
function text(value: unknown): string {
  if (typeof value === "string" || typeof value === "number") return String(value);
  if (Array.isArray(value)) return value.map(text).join(" ");
  return value && typeof value === "object" ? text((value as Tree).props?.children) : "";
}
function button(tree: unknown, label: string) {
  const found = nodes(tree).find(node => node.type === "button" && text(node).trim() === label);
  assert.ok(found, label); return found;
}
function click(node: Tree) { (node.props.onClick as () => void)(); }
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve };
}
const settle = () => new Promise<void>(resolve => setTimeout(resolve, 0));

// Production inbox, read-scope, query hooks, authority helper and API modules run
// with controlled React scheduling, UI and HTTP collaborators. The store and the
// QueryClient/QueryObserver below are real libraries, not a mounted browser.
function host() {
  const original: Auth = { token: "synthetic-a", isAuthenticated: true, user: { user_id: "user-a", permissions: ["read"], roles: ["member"], tier: "free" } };
  const store = createStore<Auth>(() => original);
  const useAuthStore = Object.assign((select: (state: Auth) => unknown) => select(store.getState()), store);
  let params = new URLSearchParams("tab=inbox&task_status=completed&task_offset=50&unrelated=kept");
  const history = [params.toString()]; const calls: Array<{ path: string; params?: unknown; signal?: AbortSignal }> = [];
  let transport: () => Promise<unknown> = async () => [];
  let refetches = 0; let sequence = 0; let cursor = 0; let effects: Array<() => void> = [];
  const slots: Slot[] = []; let captured!: Options; let scope!: Scope;
  let list: ListState = { data: Array.from({ length: 51 }, (_, i) => receipt(`task-${i + 51}`)) };
  const memo = (factory: () => unknown, deps: unknown[]) => {
    const i = cursor++; const prior = slots[i];
    if (!prior || deps.some((value, j) => !Object.is(value, prior.deps?.[j]))) slots[i] = { value: factory(), deps };
    return slots[i].value;
  };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: {
      useState(initial: unknown) {
        const i = cursor++; slots[i] ??= { value: initial };
        return [slots[i].value, (next: unknown) => { slots[i].value = typeof next === "function" ? next(slots[i].value) : next; }];
      },
      useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps),
      useLayoutEffect(fn: () => () => void, deps: unknown[]) {
        const i = cursor++; const prior = slots[i];
        if (!prior || deps.some((value, j) => !Object.is(value, prior.deps?.[j]))) {
          const next: Slot = { deps }; slots[i] = next;
          effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); });
        }
      },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "a", useSearchParams: () => [params, (update: (current: URLSearchParams) => URLSearchParams) => { params = update(params); history.push(params.toString()); }] },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en-US" } }) },
    "lucide-react": new Proxy({}, { get: (_, key) => String(key) }),
    "@/components/ui/badge": { Badge: "badge" }, "@/components/ui/button": { Button: "button" },
    "@/components/ui/card": { Card: "card", CardContent: "card-content" },
    "../taskNavigation": navigation, "@/pages/tasks/taskNavigation": navigation,
    "@/store/useAuthStore": { useAuthStore }, "@/lib/utils": { generateUUID: () => `opaque-visit-${++sequence}` },
    "@tanstack/react-query": { useQuery: (options: Options) => { captured = options; return { ...list, refetch: () => { refetches++; } }; } },
    "@/lib/api": { api: { get: async (path: string, config: { params?: unknown; signal?: AbortSignal }) => { calls.push({ path, ...config }); return { data: await transport() }; } } },
  };
  function load<T>(relative: string): T {
    const exports = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(relative, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText, { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; }, Error });
    return exports as T;
  }
  modules["@/lib/sdkAuthorityScope"] = load("../../lib/sdkAuthorityScope.ts");
  const readScope = load<{ useAuthenticatedReadScope: () => Scope }>("../../hooks/useAuthenticatedReadScope.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = { useAuthenticatedReadScope: () => { scope = readScope.useAuthenticatedReadScope(); return scope; } };
  modules["@/api/gateway"] = load("../../api/gateway.ts");
  const hooks = load<{ useTasks: (status?: string, offset?: number, scope?: Scope) => unknown }>("../../hooks/useTasks.ts");
  modules["@/hooks/useTasks"] = hooks;
  const inbox = load<{ TaskInboxTab: () => Tree | null }>("./components/TaskInboxTab.tsx");
  function render<T>(fn: () => T): T {
    cursor = 0; const result = fn(); const pending = effects; effects = []; pending.forEach(effect => effect()); return result;
  }
  return {
    original, store, history, calls, params: () => params, options: () => captured, scope: () => scope, refetches: () => refetches,
    transport(fn: typeof transport) { transport = fn; }, list(next: ListState) { list = next; },
    inbox: () => render(inbox.TaskInboxTab),
    observe(status = "completed", offset = 50) {
      render(() => { scope = readScope.useAuthenticatedReadScope(); hooks.useTasks(status, offset, scope); }); return captured;
    },
    row(node: Tree) { assert.equal(typeof node.type, "function"); return (node.type as (props: Tree["props"]) => Tree)(node.props); },
    unmount() { slots.forEach(slot => slot.cleanup?.()); },
  };
}

test("inbox binds its production read scope to the list query identity", () => {
  const h = host(); h.inbox();
  assert.equal(h.options().queryKey.length, 4);
  assert.deepEqual(JSON.parse(JSON.stringify(h.options().queryKey)), ["tasks", "user-a", { status: "completed", offset: 50 }, h.scope().id]);
  assert.doesNotMatch(JSON.stringify(h.options().queryKey), /synthetic-a|permissions|member/);
  h.unmount();
});

test("inbox retained handlers refuse an authority change before rerender", () => {
  const h = host(); const tree = h.inbox(); const before = [...h.history];
  h.store.setState({ token: "synthetic-b" });
  click(button(tree, "tasks.inbox.filters.failed")); click(button(tree, "tasks.inbox.nextPage"));
  click(button(tree, "tasks.inbox.previousPage")); click(button(tree, "common.refresh"));
  assert.deepEqual(h.history, before); assert.equal(h.refetches(), 0); h.unmount();
});

test("inbox cache visit cannot revive after a same-user credential A-B-A", async () => {
  const h = host(); const first = h.observe(); const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const cached = [receipt("private-old-visit")]; client.setQueryData(first.queryKey, cached);
  h.store.setState({ token: "synthetic-b" }); h.store.setState(h.original, true); const next = h.observe();
  try {
    assert.equal(client.getQueryData(next.queryKey), undefined);
    assert.equal(client.getQueryData(first.queryKey), cached);
    const fresh = deferred<unknown>(); h.transport(() => fresh.promise);
    const observer = new QueryObserver(client, next); const stop = observer.subscribe(() => {});
    try {
      assert.equal(observer.getCurrentResult().data, undefined);
      fresh.resolve([receipt("current-visit")]); await settle();
      assert.deepEqual(observer.getCurrentResult().data, [receipt("current-visit")]);
    } finally { stop(); }
  } finally { h.unmount(); client.clear(); }
});

for (const change of ["token", "user", "permissions", "roles", "tier", "authenticated"] as const) {
  test(`inbox ${change} A-B-A retires reads and callbacks synchronously`, async () => {
    const h = host(); const tree = h.inbox(); const old = h.options(); const prior = h.original;
    if (change === "token") h.store.setState({ token: "synthetic-b" });
    if (change === "user") h.store.setState({ user: { ...prior.user!, user_id: "user-b" } });
    if (change === "permissions") h.store.setState({ user: { ...prior.user!, permissions: [] } });
    if (change === "roles") h.store.setState({ user: { ...prior.user!, roles: ["reader"] } });
    if (change === "tier") h.store.setState({ user: { ...prior.user!, tier: "pro" } });
    if (change === "authenticated") h.store.setState({ isAuthenticated: false });
    h.store.setState(prior, true);
    assert.equal(h.scope().isCurrent(), false);
    await assert.rejects(old.queryFn({ signal: new AbortController().signal }), /retired/);
    assert.equal(h.calls.length, 0); click(button(tree, "common.refresh")); assert.equal(h.refetches(), 0);
    const oldId = h.scope().id; const current = h.inbox(); assert.notEqual(h.scope().id, oldId);
    assert.equal(h.scope().isCurrent(), true); click(button(current, "common.refresh")); assert.equal(h.refetches(), 1); h.unmount();
  });
}

test("retired inbox prevents detail navigation, read retry and cached-row rendering", () => {
  const h = host(); const tree = h.inbox();
  const row = nodes(tree).find(node => typeof node.type === "function" && node.type.name === "TaskRow"); assert.ok(row);
  const link = nodes(h.row(row)).find(node => node.type === "a"); assert.ok(link);
  assert.equal(link.props.to, "?tab=query&task_status=completed&task_offset=50&unrelated=kept&task_id=task-51");
  h.list({ isError: true }); const failed = h.inbox(); const retry = button(failed, "tasks.states.retry");
  let prevented = 0; const event = { preventDefault() { prevented++; } };
  (link.props.onClick as (clickEvent: { preventDefault: () => void }) => void)(event); assert.equal(prevented, 0);
  h.store.setState({ token: null, isAuthenticated: false, user: null });
  (link.props.onClick as (clickEvent: { preventDefault: () => void }) => void)(event); click(retry);
  assert.equal(prevented, 1); assert.equal(h.refetches(), 0);
  h.list({ data: [receipt("private-cached-row")] }); assert.equal(h.inbox(), null); h.unmount();
});

test("inbox disposal retires pending reads and retained controls", async () => {
  const h = host(); const tree = h.inbox(); const pending = deferred<unknown>(); h.transport(() => pending.promise);
  const read = h.options().queryFn({ signal: new AbortController().signal });
  h.unmount(); click(button(tree, "common.refresh")); pending.resolve([receipt("late")]);
  await assert.rejects(read, /retired/); assert.equal(h.refetches(), 0); assert.equal(h.calls.length, 1);
});

test("actual query cache rejects a late inbox success after authority retirement", async () => {
  const h = host(); const late = deferred<unknown>(); h.transport(() => late.promise); const options = h.observe();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const read = client.fetchQuery(options); const rejected = assert.rejects(read, /retired/);
  try {
    h.store.setState({ token: "synthetic-b" }); h.store.setState(h.original, true);
    late.resolve([receipt("late-private")]); await rejected;
    assert.equal(client.getQueryData(options.queryKey), undefined);
    assert.equal(client.getQueryState(options.queryKey)?.status, "error");
  } finally { h.unmount(); client.clear(); }
});

test("existing task-user prefix invalidation still reaches every scoped page and visit", async () => {
  const h = host(); const first = h.observe(); const secondPage = h.observe("failed", 100);
  h.store.setState({ token: "synthetic-b" }); const anotherVisit = h.observe();
  h.store.setState({ user: { ...h.original.user!, user_id: "user-b" } }); const otherUser = h.observe();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  try {
    for (const options of [first, secondPage, anotherVisit, otherUser]) client.setQueryData(options.queryKey, []);
    await client.invalidateQueries({ queryKey: ["tasks", "user-a"], refetchType: "none" });
    for (const options of [first, secondPage, anotherVisit]) assert.equal(client.getQueryState(options.queryKey)?.isInvalidated, true);
    assert.equal(client.getQueryState(otherUser.queryKey)?.isInvalidated, false);
  } finally { h.unmount(); client.clear(); }
});
