import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import * as navigation from "./agentDirectoryNavigation.ts";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Page = { items: Array<Record<string, unknown>>; next_cursor: string | null };
type Query = { queryKey: unknown[]; queryFn: (context: { signal: AbortSignal }) => Promise<Page>; enabled: boolean; retry: boolean };
function nodes(value: unknown): Tree[] {
  if (Array.isArray(value)) return value.flatMap(nodes);
  if (!value || typeof value !== "object") return [];
  const node = value as Tree; return [node, ...nodes(node.props?.children)];
}
function label(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(label).join("");
  return value && typeof value === "object" ? label((value as Tree).props?.children) : "";
}
const row = (id: string) => ({ agent_id: id, name: id, owner_id: "owner-a", status: "draft", updated_at: "2026-10-13T00:00:00Z" });
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(yes => { resolve = yes; }); return { promise, resolve }; }
function host(initial = "other=keep") {
  let params = new URLSearchParams(initial); let options!: Query;
  let result: { data?: Page; isLoading?: boolean; isFetching?: boolean; error?: Error } = {};
  let authorized = true; let scope = "scope-a"; let index = 0; let uuid = 0; let location = params.toString();
  const slots: unknown[] = []; const effects: Array<() => unknown> = [];
  let auth = { token: "test-token-a", isAuthenticated: true, user: { user_id: "owner-a", permissions: ["agents:read"], roles: ["member"], tier: "normal" } };
  const subscribers: Array<(state: typeof auth, prior: typeof auth) => void> = [];
  const history: string[] = []; const reads: Array<{ path: string; params: unknown; signal: unknown }> = [];
  let respond = async (): Promise<Page> => ({ items: [], next_cursor: null });
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const named = new Proxy({}, { get: (_, key) => String(key) });
  const authority = (user: typeof auth.user) => JSON.stringify(user);
  const authStore = Object.assign((select: (state: typeof auth) => unknown) => select(auth), {
    getState: () => auth,
    subscribe: (callback: (state: typeof auth, prior: typeof auth) => void) => { subscribers.push(callback); return () => {}; },
  });
  const setParams = (update: (current: URLSearchParams) => URLSearchParams) => { params = update(params); history.push(params.toString()); };
  const modules: Record<string, unknown> = {
    react: { useState(value: unknown) { const i = index++; if (!(i in slots)) slots[i] = value; return [slots[i], (next: unknown) => { slots[i] = typeof next === "function" ? next(slots[i]) : next; }]; },
      useMemo(factory: () => unknown, deps: unknown[]) { const i = index++; const prior = slots[i] as { deps: unknown[]; value: unknown } | undefined;
        if (!prior || deps.some((item, j) => !Object.is(item, prior.deps[j]))) slots[i] = { deps, value: factory() }; return (slots[i] as { value: unknown }).value; },
      useLayoutEffect(effect: () => unknown) { effects.push(effect); } },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "lucide-react": named,
    antd: { ...Object.fromEntries(["Alert", "Button", "Dropdown", "Input", "Select", "Skeleton", "Table", "Tag"].map(key => [key, key])), Empty: Object.assign(() => {}, { PRESENTED_IMAGE_SIMPLE: "empty" }), Typography: named, App: { useApp: () => ({ message: { success() {}, error() {} }, modal: { confirm() {} } }) } },
    "react-i18next": { useTranslation: () => ({ t: (key: string, args?: unknown) => args ? `${key}:${JSON.stringify(args)}` : key, i18n: { resolvedLanguage: "en-US" } }) },
    "react-router-dom": { Link: "Link", useSearchParams: () => [params, setParams], useNavigate: () => () => {} },
    dayjs: Object.assign(() => ({ locale: () => ({ fromNow: () => "now" }) }), { extend() {} }), "dayjs/plugin/relativeTime": {}, "dayjs/locale/zh-cn": {}, "./agent-studio.css": {},
    "./agentDirectoryNavigation": navigation,
    "@/store/useAuthStore": { useAuthStore: authStore }, "@/lib/sdkAuthorityScope": { sdkAuthorityScope: authority }, "@/lib/utils": { generateUUID: () => `scope-${++uuid}` },
    "@tanstack/react-query": { useQuery: (next: Query) => { options = next; return { isFetching: false, isLoading: false, ...result, refetch() {} }; }, useMutation: () => ({}), useQueryClient: () => ({}) },
    "@/lib/sse": {},
    "@/lib/api": { api: { get: async (path: string, config: { params: unknown; signal: unknown }) => { reads.push({ path, ...config }); return { data: await respond() }; } } },
  };
  function compile(path: string, suffix = "") {
    const exports: Record<string, (...args: never[]) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8") + suffix, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText, { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports;
  }
  modules["@/api/agents"] = compile("../../api/agents.ts");
  const compiled = compile("./AgentListPage.tsx", "\nexports.Content = AgentDirectoryContent;") as unknown as { AgentListPage: () => Tree; Content: (props: unknown) => Tree };
  return {
    reads, history, get params() { return params; }, get options() { return options; },
    setResult(next: typeof result) { result = next; }, setResponse(next: typeof respond) { respond = next; },
    render() { if (location !== params.toString()) { slots.length = 0; location = params.toString(); } index = 0; return compiled.Content({ scopeId: scope, isCurrent: () => authorized }); },
    wrapper() { index = 0; const tree = compiled.AgentListPage(); effects.splice(0).forEach(effect => effect()); return tree; },
    changeAuth(token: string) { const prior = auth; auth = { ...auth, token }; subscribers.forEach(callback => callback(auth, prior)); },
    invalidate() { authorized = false; }, newScope() { scope = "scope-b"; },
    restore(url: string) { params = new URLSearchParams(url); },
  };
}
function button(tree: Tree, key: string) { const found = nodes(tree).find(node => node.type === "Button" && label(node) === `agents.list.${key}`); assert.ok(found, key); return found.props as { disabled?: boolean; onClick: () => void }; }
function input(tree: Tree, key: string) { const found = nodes(tree).find(node => node.props?.["aria-label"] === `agents.list.${key}`); assert.ok(found, key); return found.props; }

test("directory location keeps opaque cursors and normalizes supported filters", () => {
  const location = navigation.agentDirectoryLocation(new URLSearchParams("agent_cursor=opaque%2B%2F%3D&agent_status=deleted&agent_search=" + "a".repeat(300)));
  assert.equal(location.cursor, "opaque+/="); assert.equal(location.status, undefined); assert.equal(location.search.length, 255);
});
test("cursor update preserves unrelated URL values, search and owner", () => {
  const next = navigation.agentDirectoryParams(new URLSearchParams("other=x&agent_search=two+words&agent_owner=user&agent_cursor=old"), { cursor: "new+/=" });
  assert.equal(next.get("other"), "x"); assert.equal(next.get("agent_search"), "two words"); assert.equal(next.get("agent_owner"), "user"); assert.equal(next.get("agent_cursor"), "new+/=");
});
test("next cursor requires a new nonempty server string", () => {
  for (const value of [null, undefined, 4, {}, "", "same"]) assert.equal(navigation.agentDirectoryNextCursor(value, "same"), undefined);
  assert.equal(navigation.agentDirectoryNextCursor("next", "same"), "next");
});
test("production page and API request a bounded cursor page with AbortSignal", async () => {
  const h = host("agent_search=needle&agent_status=active&agent_owner=owner-b&agent_cursor=opaque"); h.render();
  const signal = new AbortController().signal; await h.options.queryFn({ signal });
  assert.equal(h.reads[0].path, "/api/v1/agents"); assert.deepEqual(JSON.parse(JSON.stringify(h.reads[0].params)), { limit: 25, cursor: "opaque", search: "needle", status: "active", owner_id: "owner-b" }); assert.equal(h.reads[0].signal, signal);
});
test("production navigation reaches a fifth page instead of truncating at100", () => {
  const h = host();
  for (let page = 0; page < 5; page++) {
    h.setResult({ data: { items: Array.from({ length: 25 }, (_, i) => row(String(page * 25 + i))), next_cursor: page < 4 ? `cursor-${page + 1}` : null } });
    const tree = h.render(); assert.equal(h.options.queryKey[1], "scope-a");
    assert.ok(label(tree).includes('agents.list.pageCount:{"count":25}'));
    if (page < 4) button(tree, "nextPage").onClick(); else assert.equal(button(tree, "nextPage").disabled, true);
  }
  assert.equal(h.params.get("agent_cursor"), "cursor-4"); assert.equal(h.params.get("other"), "keep");
});
test("filters reset cursor atomically and exact owner accepts IDs absent from page", () => {
  for (const [field, value, expected] of [["searchLabel", "new", "agent_search"], ["filterOwner", "unseen-owner", "agent_owner"], ["filterStatus", "archived", "agent_status"]]) {
    const h = host("agent_cursor=old&other=keep"); const control = input(h.render(), field);
    const change = control.onChange as (value: unknown) => void;
    change(field === "filterStatus" ? value : { target: { value } });
    assert.equal(h.params.get("agent_cursor"), "old");
    button(h.render(), "applyFilters").onClick();
    assert.equal(h.params.get("agent_cursor"), null); assert.equal(h.params.get(expected), value); assert.equal(h.params.get("other"), "keep");
  }
});
test("failed page retains cursor and first-page recovery, hides stale rows and advancement", () => {
  const h = host("agent_cursor=bad"); h.setResult({ data: { items: [row("stale")], next_cursor: "next" }, error: new Error("failed") }); const tree = h.render();
  assert.equal(button(tree, "nextPage").disabled, true); assert.equal(button(tree, "firstPage").disabled, false);
  assert.equal(nodes(tree).some(node => node.type === "Table"), false); assert.equal(h.params.get("agent_cursor"), "bad");
  button(tree, "firstPage").onClick(); assert.equal(h.params.get("agent_cursor"), null);
});
test("in-flight fetch disables both paging controls and cannot reuse old next cursor", () => {
  const h = host("agent_cursor=current"); h.setResult({ data: { items: [], next_cursor: "next" }, isFetching: true }); const tree = h.render();
  assert.equal(button(tree, "nextPage").disabled, true); assert.equal(button(tree, "firstPage").disabled, true);
});
test("empty final page allows recovery to first page without claiming directory empty total", () => {
  const h = host("agent_cursor=tail"); h.setResult({ data: { items: [], next_cursor: null } }); const tree = h.render();
  assert.equal(button(tree, "firstPage").disabled, false); assert.equal(button(tree, "nextPage").disabled, true); assert.ok(label(tree).includes('agents.list.pageCount:{"count":0}'));
});
test("URL restoration selects exact prior page and filters", () => {
  const h = host(); h.restore("agent_cursor=back-page&agent_search=restored&agent_status=active"); h.render();
  assert.deepEqual(JSON.parse(JSON.stringify(h.options.queryKey[2])), { search: "restored", status: "active", cursor: "back-page" });
});
test("authority-invalidated late read is rejected and stale navigation does nothing", async () => {
  const h = host(); const pending = deferred<Page>(); h.setResponse(() => pending.promise); h.setResult({ data: { items: [], next_cursor: "next" } });
  const tree = h.render(); const request = h.options.queryFn({ signal: new AbortController().signal }); h.invalidate(); pending.resolve({ items: [row("private")], next_cursor: null });
  await assert.rejects(request, /authorization changed/); button(tree, "nextPage").onClick(); assert.equal(h.history.length, 0);
  await assert.rejects(h.options.queryFn({ signal: new AbortController().signal }), /authorization changed/); assert.equal(h.reads.length, 1);
});
test("production wrapper synchronously fences A-B-A credentials and clears cursor", () => {
  const h = host("agent_cursor=old&agent_search=keep"); const first = h.wrapper(); const current = first.props.isCurrent as () => boolean;
  assert.equal(current(), true); h.changeAuth("test-token-b"); h.changeAuth("test-token-a"); assert.equal(current(), false);
  assert.equal(h.params.get("agent_cursor"), null); assert.equal(h.params.get("agent_search"), "keep"); assert.notEqual(h.wrapper().key, first.key);
});
test("real QueryObserver isolates late previous-page responses", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const previous = deferred<Page>(); const next = deferred<Page>();
  const h = host(); h.setResponse(() => previous.promise); h.render(); const oldOptions = h.options;
  const observer = new QueryObserver(client, oldOptions); const unsubscribe = observer.subscribe(() => {});
  await Promise.resolve(); h.restore("agent_cursor=next"); h.setResponse(() => next.promise); h.render(); observer.setOptions(h.options);
  next.resolve({ items: [row("current")], next_cursor: null }); await new Promise(resolve => setTimeout(resolve, 0));
  previous.resolve({ items: [row("old")], next_cursor: "wrong" }); await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(observer.getCurrentResult().data?.items[0].agent_id, "current"); unsubscribe(); client.clear();
});


test("typing retains responsive input without changing production query key or requesting", async () => {
  const h = host("agent_cursor=old"); h.setResponse(async () => ({ items: [row("kept")], next_cursor: "next" }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  h.render(); const observer = new QueryObserver(client, h.options); const unsubscribe = observer.subscribe(() => {});
  await new Promise(resolve => setTimeout(resolve, 0)); assert.equal(h.reads.length, 1);
  for (const text of ["a", "ab", "abc", "abcd"]) {
    const change = input(h.render(), "searchLabel").onChange as (event: unknown) => void; change({ target: { value: text } });
    const tree = h.render(); assert.equal(input(tree, "searchLabel").value, text); observer.setOptions(h.options);
    assert.equal(button(tree, "firstPage").disabled, true); assert.equal(button(tree, "nextPage").disabled, true);
  }
  for (const text of ["u", "us", "user-b"]) {
    (input(h.render(), "filterOwner").onChange as (event: unknown) => void)({ target: { value: text } }); h.render(); observer.setOptions(h.options);
  }
  await new Promise(resolve => setTimeout(resolve, 0)); assert.equal(h.reads.length, 1); assert.equal(h.params.get("agent_cursor"), "old");
  button(h.render(), "applyFilters").onClick(); h.render(); observer.setOptions(h.options);
  await new Promise(resolve => setTimeout(resolve, 0)); assert.equal(h.reads.length, 2);
  assert.deepEqual(JSON.parse(JSON.stringify(h.reads[1].params)), { limit: 25, search: "abcd", owner_id: "user-b" });
  assert.equal(button(h.render(), "applyFilters").disabled, true); h.render(); observer.setOptions(h.options);
  await new Promise(resolve => setTimeout(resolve, 0)); assert.equal(h.reads.length, 2); unsubscribe(); client.clear();
});
test("Enter applies both draft fields once, while input-method composition does not", () => {
  const h = host("agent_cursor=old");
  (input(h.render(), "searchLabel").onChange as (event: unknown) => void)({ target: { value: "中文" } });
  (input(h.render(), "filterOwner").onChange as (event: unknown) => void)({ target: { value: "owner-b" } });
  const enter = input(h.render(), "searchLabel").onPressEnter as (event: unknown) => void;
  enter({ nativeEvent: { isComposing: true } }); assert.equal(h.history.length, 0);
  enter({ nativeEvent: { isComposing: false } }); assert.equal(h.params.get("agent_search"), "中文"); assert.equal(h.params.get("agent_owner"), "owner-b"); assert.equal(h.params.get("agent_cursor"), null);
  (input(h.render(), "filterOwner").onPressEnter as (event: unknown) => void)({ nativeEvent: { isComposing: false } }); assert.equal(h.history.length, 1);
});
test("back/forward location restores applied filters and discards unapplied text", () => {
  const h = host("agent_search=original&agent_cursor=page-two");
  (input(h.render(), "searchLabel").onChange as (event: unknown) => void)({ target: { value: "unsaved" } });
  h.restore("agent_search=earlier"); assert.equal(input(h.render(), "searchLabel").value, "earlier");
  h.restore("agent_search=original&agent_cursor=page-two"); const tree = h.render(); assert.equal(input(tree, "searchLabel").value, "original"); assert.equal(button(tree, "applyFilters").disabled, true);
});
test("stale Apply cannot overwrite a navigated page or cross authority", () => {
  for (const invalidate of [false, true]) {
    const h = host("agent_search=old"); (input(h.render(), "searchLabel").onChange as (event: unknown) => void)({ target: { value: "draft" } });
    const apply = button(h.render(), "applyFilters");
    if (invalidate) h.invalidate(); else h.restore("agent_search=new&agent_cursor=new-page");
    apply.onClick(); assert.equal(h.params.get("agent_search"), invalidate ? "old" : "new");
  }
});
