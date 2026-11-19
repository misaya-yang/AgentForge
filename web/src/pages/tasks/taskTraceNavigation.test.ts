import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import * as navigation from "./taskTraceNavigation.ts";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
const nodes = (tree: unknown): Tree[] => Array.isArray(tree) ? tree.flatMap(nodes) : tree && typeof tree === "object" ? [tree as Tree, ...nodes((tree as Tree).props?.children)] : [];
function host(query = "request_id=req%2F%3F%26&task_id=task-a&task_status=failed&task_offset=50") {
  let cursor = 0; const slots: unknown[] = []; let current = true; let allowed = true; const cleanups: Array<() => void> = []; const pending: Array<() => void> = [];
  const calls: Array<{ id: string; signal: AbortSignal; resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
  const params = new URLSearchParams(query);
  const isCurrent = () => current;
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = {
    react: {
      useState(value: unknown) { const i = cursor++; slots[i] ??= value; return [slots[i], (next: unknown) => { slots[i] = next; }]; },
      useRef(value: unknown) { const i = cursor++; slots[i] ??= { current: value }; return slots[i]; },
      useLayoutEffect(fn: () => () => void) { const i = cursor++; if (!slots[i]) { slots[i] = true; pending.push(() => cleanups.push(fn())); } },
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "Link", useSearchParams: () => [params] },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "../taskTraceNavigation": navigation,
    "@/store/useAuthStore": { useAuthStore: (select: (state: unknown) => unknown) => select({ hasPermission: (permission: string) => { assert.equal(permission, "console:dashboard:view"); return allowed; } }) },
    "@/api/usage": { getRequestTrace: (id: string, signal: AbortSignal) => new Promise((resolve, reject) => { calls.push({ id, signal, resolve, reject }); }) },
  };
  function load(file: string) {
    const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, { exports, AbortController, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } });
    return exports;
  }
  const lookup = load("./components/TaskTraceLookup.tsx").TaskTraceLookup;
  const link = load("./components/TaskTraceLink.tsx").TaskTraceLink;
  return { calls, params, renderLink(requestId: unknown) { cursor = 0; const tree = link({ requestId, taskId: "task-a", isCurrent }); pending.splice(0).forEach(fn => fn()); return tree; },
    render() { cursor = 0; const tree = lookup({ isCurrent, renderTrace: (trace: unknown) => jsx("Trace", { trace }) }); pending.splice(0).forEach(fn => fn()); return tree; },
    deny() { allowed = false; }, retire() { current = false; }, unmount() { cleanups.forEach(fn => fn()); }, writes() { return JSON.stringify(slots); },
  };
}
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
test("opaque exact ID and task return preserve reserved characters and inbox context without external return URLs", () => {
  assert.equal(navigation.exactTraceId("req-😀"), "req-😀");
  assert.equal(new URL(navigation.taskTraceHref("req-😀", "task", new URLSearchParams())!, "https://console.test").searchParams.get("request_id"), "req-😀");
  const href = navigation.taskTraceHref("req/?&", "task/?&", new URLSearchParams("task_status=failed&task_offset=50&return_to=https://evil.test"));
  assert.ok(href); const params = new URL(href, "https://console.test").searchParams;
  assert.equal(params.get("request_id"), "req/?&");
  const back = new URL(navigation.taskTraceReturn(params)!, "https://console.test");
  assert.equal(back.pathname, "/tasks"); assert.equal(back.searchParams.get("task_id"), "task/?&"); assert.equal(back.searchParams.get("task_status"), "failed"); assert.equal(back.searchParams.get("task_offset"), "50"); assert.equal(back.searchParams.has("return_to"), false);
});
test("absent and malformed IDs never generate links or dispatch trace reads", () => {
  for (const id of [undefined, null, 7, "", "\ud800", "\udfff", " req", "req ", "a\n", "x".repeat(513)]) {
    assert.equal(navigation.exactTraceId(id), null); assert.equal(navigation.taskTraceHref(id, "task", new URLSearchParams()), null); assert.equal(host().renderLink(id), null);
  }
  for (const query of ["task_id=a", "request_id=", "request_id=%20req", "request_id=a%0A"]) { const h = host(query); assert.ok(nodes(h.render()).some(n => n.props.children === "tasks.trace.unavailable")); assert.equal(h.calls.length, 0); }
});
test("production lookup loads exact route once and renders only the matching response", async () => {
  const h = host(); assert.ok(nodes(h.render()).some(n => n.props.children === "dashboard.requestTrace.loading")); assert.equal(h.calls.length, 1); assert.equal(h.calls[0].id, "req/?&");
  h.render(); assert.equal(h.calls.length, 1); h.calls[0].resolve({ request_id: "req/?&" }); await flush(); assert.equal(nodes(h.render()).filter(n => n.type === "Trace").length, 1);
  const back = nodes(h.render()).find(n => n.type === "Link")!; assert.equal(back.props.to, "/tasks?tab=query&task_id=task-a&task_status=failed&task_offset=50");
});
test("missing forbidden failed and mismatched responses remain unavailable with no fallback or retry", async () => {
  for (const outcome of [404, 403, 500, "mismatch", "null"]) { const h = host(); h.render(); if (outcome === "mismatch") h.calls[0].resolve({ request_id: "other" }); else if (outcome === "null") h.calls[0].resolve(null); else h.calls[0].reject({ status: outcome }); await flush(); assert.ok(nodes(h.render()).some(n => n.props.children === "tasks.trace.unavailable")); assert.equal(nodes(h.render()).filter(n => n.type === "Trace").length, 0); assert.equal(h.calls.length, 1); }
});
test("unmount and authority retirement fence late success and error, including stale return callbacks", async () => {
  for (const mode of ["unmount", "auth"]) for (const fail of [false, true]) { const h = host(); const back = nodes(h.render()).find(n => n.type === "Link")!; if (mode === "unmount") h.unmount(); else h.retire(); const before = h.writes(); if (fail) h.calls[0].reject(new Error("late")); else h.calls[0].resolve({ request_id: "req/?&" }); await flush(); assert.equal(h.writes(), before); if (mode === "unmount") assert.equal(h.calls[0].signal.aborted, true); { let prevented = false; (back.props.onClick as (e: unknown) => void)({ preventDefault: () => { prevented = true; } }); assert.equal(prevented, true); if (mode === "auth") assert.equal(h.render(), null); } }
});
test("production task link dispatch target is exact and stale navigation cannot activate", () => {
  for (const mode of ["auth", "unmount"]) { const h = host(); const link = h.renderLink("req/?&"); assert.ok(String(link.props.to).startsWith("/dashboard?tab=tracing&request_id=req%2F%3F%26")); if (mode === "auth") h.retire(); else h.unmount(); let prevented = false; (link.props.onClick as (e: unknown) => void)({ preventDefault: () => { prevented = true; } }); assert.equal(prevented, true); }
});

test("task trace link honors existing dashboard permission and retirement", () => {
  const h = host(); const old = h.renderLink("request"); h.deny(); assert.equal(h.renderLink("request"), null);
  let prevented = false; (old.props.onClick as (e: unknown) => void)({ preventDefault: () => { prevented = true; } }); assert.equal(prevented, true);
});

test("trace translations exist at root tasks in both supported locales", () => {
  for (const language of ["en-US", "zh-CN"]) {
    const locale = { ...JSON.parse(readFileSync(new URL(`../../i18n/locales/${language}.json`, import.meta.url), "utf8")), tasks: JSON.parse(readFileSync(new URL(`../../i18n/locales/tasks-${language}.json`, import.meta.url), "utf8")) };
    for (const key of ["open", "back", "notice", "unavailable"]) assert.equal(typeof locale.tasks.trace[key], "string");
    if (language === "zh-CN") assert.match(locale.tasks.trace.open, /追踪/);
  }
});
