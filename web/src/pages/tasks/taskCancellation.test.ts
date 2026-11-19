import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as cancellation from "./taskCancellation.ts";
import * as navigation from "./taskNavigation.ts";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

interface Tree { type: string | ((props: Record<string, unknown>) => Tree | null); props: Record<string, unknown>; key?: string }
function nodes(tree: unknown): Tree[] {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree;
  return [node, ...nodes(node.props?.children)];
}
function text(tree: unknown): string {
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  return tree && typeof tree === "object" ? text((tree as Tree).props?.children) : "";
}
function button(tree: unknown, key: string) {
  const node = nodes(tree).find(node => node.type === "button" && text(node) === `tasks.cancellation.${key}`);
  assert.ok(node, key); return node.props;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

// Production component + production API adapter + production operation controller.
// React scheduling, query invalidation, auth and transport are explicit doubles.
function host() {
  const auth = { token: "synthetic-a", user: { user_id: "actor-a", permissions: [], roles: [], tier: "normal" } };
  let props = { taskId: "task-a", status: "processing" };
  let detailState = "available";
  let focused = "";
  const calls: { method: string; path: string }[] = [];
  const invalidated: unknown[] = [];
  let write: (path: string) => Promise<unknown> = async () => ({ task_id: props.taskId, status: "cancelled" });
  let read: (path: string) => Promise<unknown> = async () => ({ task_id: props.taskId, status: "cancelled" });
  type Hook = { deps?: unknown[]; value?: unknown; cleanup?: () => void };
  const wrapper: Hook[] = [];
  const parentSlots: Hook[] = [];
  let content: Hook[] = [], slots: Hook[] = wrapper;
  let cursor = 0, key: string | undefined, nextId = 0;
  let effects: Array<() => void> = [];
  const same = (a?: unknown[], b?: unknown[]) => !!a && !!b && a.length === b.length && a.every((v, i) => Object.is(v, b[i]));
  const jsx = (type: Tree["type"], props: Tree["props"], key?: string) => {
    if (props.ref && typeof props.ref === "object") {
      (props.ref as { current: unknown }).current = { focus() { focused = type === "section" ? "section" : text(props.children); } };
    }
    return { type, props, key };
  };
  const queryClient = { invalidateQueries: async (options: unknown) => { invalidated.push(options); } };
  const authHook = Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth });
  const readScope = { id: "cancellation-parent-read-scope", isCurrent: () => Boolean(auth.token && auth.user) };
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    react: {
      useId() { const slot = slots[cursor++] ??= { value: `id-${++nextId}` }; return slot.value; },
      useCallback: (callback: unknown) => callback,
      useState(initial: unknown) { const slot = slots[cursor++] ??= { value: initial }; return [slot.value, (value: unknown) => { slot.value = typeof value === "function" ? value(slot.value) : value; }]; },
      useRef(initial: unknown) { const slot = slots[cursor++] ??= { value: { current: initial } }; return slot.value; },
      useMemo(make: () => unknown, deps: unknown[]) { const slot = slots[cursor++] ??= {}; if (!same(slot.deps, deps)) { slot.value = make(); slot.deps = deps; } return slot.value; },
      useEffect(effect: () => (() => void), deps: unknown[]) { const slot = slots[cursor++] ??= {}; if (!same(slot.deps, deps)) { effects.push(() => { slot.cleanup?.(); slot.cleanup = effect(); }); slot.deps = deps; } },
    },
    "@tanstack/react-query": { useQueryClient: () => queryClient },
    "react-i18next": { useTranslation: () => ({ t: (key: string, options?: unknown) => key + (options ? ` ${JSON.stringify(options)}` : ""), i18n: { language: "en-US" } }) },
    "@/components/ui/button": { Button: "button" },
    "@/components/ui/input": { Input: "input" },
    "@/components/ui/badge": { Badge: "badge" },
    "@/components/ui/card": { Card: "card", CardContent: "card-content", CardHeader: "card-header", CardTitle: "card-title" },
    "react-router-dom": { Link: "a", useSearchParams: () => [new URLSearchParams({ task_id: props.taskId }), () => { throw new Error("Unexpected search"); }] },
    "lucide-react": new Proxy({}, { get: (_, name) => String(name) }),
    "@/lib/clipboard": { copyToClipboard: () => { throw new Error("Unexpected clipboard"); } },
    "../taskNavigation": navigation,
    "./TaskTraceLink": { TaskTraceLink: "task-trace-link" },
    "./TaskDiagnostics": { TaskDiagnostics: "task-diagnostics" },
    // Explicit parent-boundary doubles. Actual output rendering and authority-hook
    // lifecycle remain covered by taskOutputBrowser.test.ts.
    "./TaskOutputBrowser": { TaskOutputBrowser: "task-output-browser" },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => readScope },
    "@/hooks/useTasks": {
      useTask: (id: string, scope: unknown) => { assert.equal(id, props.taskId); assert.equal(scope, readScope); return { data: { task_id: detailState === "wrong" ? "other" : props.taskId, status: props.status, created_at: "2026-10-13T00:00:00Z" }, isError: detailState === "error", isLoading: detailState === "loading" }; },
      useTaskResult: (_id: string | undefined, scope: unknown) => { assert.equal(scope, readScope); return {}; },
    },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope },
    "@/lib/utils": { generateUUID: () => `view-${++nextId}` },
    "@/store/useAuthStore": { useAuthStore: authHook },
    "../taskCancellation": cancellation,
    "@/lib/api": { api: {
      delete: async (path: string) => { calls.push({ method: "DELETE", path }); return { data: await write(path) }; },
      get: async (path: string) => { calls.push({ method: "GET", path }); return { data: await read(path) }; },
    } },
  };
  function load(path: string) {
    const exports: Record<string, (...args: unknown[]) => unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText, {
      exports, require(name: string) { assert.ok(name in modules, name); return modules[name]; }, Error,
    });
    return exports;
  }
  const api = load("../../api/gateway.ts"); modules["@/api/gateway"] = api;
  const component = load("./components/TaskCancellation.tsx");
  modules["./TaskCancellation"] = component;
  const parent = load("./components/TaskQueryTab.tsx");
  function cleanup() { for (const slot of content) slot.cleanup?.(); content = []; }
  return {
    auth, calls, invalidated, api,
    focused: () => focused,
    write(fn: typeof write) { write = fn; }, read(fn: typeof read) { read = fn; },
    task(taskId: string, status = "processing") { props = { taskId, status }; },
    detail(state: string) { detailState = state; },
    unmount: cleanup,
    render(fromParent = false) {
      let cancellationProps: Record<string, unknown> = props;
      if (fromParent) {
        slots = parentSlots; cursor = 0;
        const root = parent.TaskQueryTab() as Tree;
        const tree = (root.type as (props: Tree["props"]) => Tree)(root.props);
        const control = nodes(tree).find(node => node.type === component.TaskCancellation);
        if (!control) { cleanup(); key = undefined; return null; }
        cancellationProps = control.props;
      }
      slots = wrapper; cursor = 0;
      const outer = component.TaskCancellation(cancellationProps) as Tree | null;
      if (!outer) { cleanup(); return null; }
      if (key !== outer.key) { cleanup(); key = outer.key; }
      slots = content; cursor = 0;
      const result = (outer.type as (props: Tree["props"]) => Tree | null)(outer.props);
      for (const effect of effects) effect(); effects = [];
      return result;
    },
  };
}
function click(tree: unknown, key: string) { const props = button(tree, key); assert.ok(!props.disabled); (props.onClick as () => void)(); }
function confirm(h: ReturnType<typeof host>) { click(h.render(), "request"); click(h.render(), "confirm"); }

for (const status of ["pending", "processing"]) test(`${status}: explicit confirmation targets existing DELETE and independent GET`, async () => {
  const h = host(); h.task("task /?#", status);
  click(h.render(), "request"); assert.equal(h.calls.length, 0);
  assert.match(text(h.render()), /task \/\?#/);
  click(h.render(), "keep"); assert.equal(h.calls.length, 0);
  confirm(h); await settle();
  assert.deepEqual(h.calls, [{ method: "DELETE", path: "/api/v1/tasks/task%20%2F%3F%23" }, { method: "GET", path: "/api/v1/tasks/task%20%2F%3F%23" }]);
  assert.match(text(h.render()), /cancellation.accepted/);
  assert.match(text(h.render()), /tasks.status.cancelled/);
  assert.equal(h.invalidated.length, 2);
  assert.doesNotMatch(text(h.render()), /cancellation.confirm/);
});

for (const status of ["completed", "failed", "cancelled", "unknown"]) test(`${status}: no cancellation action`, () => {
  const h = host(); h.task("task-a", status); assert.equal(h.render(), null); assert.equal(h.calls.length, 0);
});

test("double confirm before re-render submits once, refresh while pending does not read", async () => {
  const h = host(), pending = deferred<unknown>(); h.write(() => pending.promise);
  click(h.render(), "request"); const action = button(h.render(), "confirm").onClick as () => void;
  action(); action(); assert.equal(h.calls.length, 1); assert.match(text(h.render()), /cancellation.sending/);
  pending.resolve({ task_id: "task-a", status: "cancelled" }); await settle(); assert.equal(h.calls.length, 2);
});

for (const status of ["cancelled", "completed", "failed", "pending", "processing"]) test(`uncertain DELETE reads ${status} without replay`, async () => {
  const h = host(); h.write(async () => { throw new Error("lost response / 409 / 404"); });
  h.read(async () => ({ task_id: "task-a", status })); confirm(h); await settle();
  assert.match(text(h.render()), /cancellation.unknown/); assert.match(text(h.render()), new RegExp(`tasks.status.${status}`));
  click(h.render(), "refresh"); await settle(); assert.deepEqual(h.calls.map(call => call.method), ["DELETE", "GET", "GET"]);
});

for (const response of [null, {}, { task_id: "wrong", status: "cancelled" }, { task_id: "task-a", status: "completed" }]) test(`malformed success is unconfirmed: ${JSON.stringify(response)}`, async () => {
  const h = host(); h.write(async () => response); confirm(h); await settle();
  assert.match(text(h.render()), /cancellation.unknown/); assert.doesNotMatch(text(h.render()), /cancellation.accepted/);
});

test("failed read hides old observation; refresh is read-only and blocks repeated clicks", async () => {
  const h = host(); confirm(h); await settle();
  const pending = deferred<unknown>(); h.read(() => pending.promise);
  const refresh = button(h.render(), "refresh").onClick as () => void; refresh(); refresh();
  assert.equal(h.calls.length, 3); assert.doesNotMatch(text(h.render()), /tasks.status.cancelled/); assert.equal(button(h.render(), "refresh").disabled, true);
  pending.reject(new Error("unavailable")); await settle(); assert.match(text(h.render()), /cancellation.readFailed/);
  h.read(async () => ({ task_id: "task-a", status: "cancelled" })); click(h.render(), "refresh"); await settle();
  assert.doesNotMatch(text(h.render()), /cancellation.readFailed/); assert.equal(h.calls.filter(c => c.method === "DELETE").length, 1);
});

for (const receipt of [{ task_id: "other", status: "cancelled" }, { task_id: "task-a", status: "invented" }]) test(`wrong readback never updates cached views: ${JSON.stringify(receipt)}`, async () => {
  const h = host(); h.read(async () => receipt); confirm(h); await settle();
  assert.match(text(h.render()), /cancellation.readFailed/); assert.equal(h.invalidated.length, 0);
});

test("terminal receipt arriving during confirmation removes action", () => {
  const h = host(); click(h.render(), "request"); h.task("task-a", "completed"); assert.equal(h.render(), null); assert.equal(h.calls.length, 0);
});

for (const phase of ["write", "read"]) test(`navigation abandons late ${phase}; returning to original ID cannot revive it`, async () => {
  const h = host(), pending = deferred<unknown>();
  if (phase === "write") h.write(() => pending.promise); else h.read(() => pending.promise);
  confirm(h); await settle(); h.task("task-b"); h.render(); h.task("task-a"); h.render();
  pending.resolve({ task_id: "task-a", status: "cancelled" }); await settle();
  assert.equal(h.invalidated.length, 0); assert.doesNotMatch(text(h.render()), /cancellation.accepted|cancellation.observed/);
  assert.equal(h.calls.length, phase === "write" ? 1 : 2);
});

for (const change of ["token", "user", "scope", "unmount"]) test(`${change} fences late cancellation and subsequent read`, async () => {
  const h = host(), pending = deferred<unknown>(); h.write(() => pending.promise); confirm(h);
  if (change === "token") h.auth.token = "synthetic-b";
  if (change === "user") h.auth.user = { ...h.auth.user, user_id: "actor-b" };
  if (change === "scope") h.auth.user = { ...h.auth.user, tier: "changed" };
  if (change === "unmount") h.unmount(); else h.render();
  pending.resolve({ task_id: "task-a", status: "cancelled" }); await settle();
  assert.equal(h.calls.length, 1); assert.equal(h.invalidated.length, 0);
});

test("logout hides control and direct controller calls reject terminal statuses", async () => {
  const h = host(); h.auth.token = ""; assert.equal(h.render(), null);
  const op = cancellation.taskCancellation({ taskId: "a", current: () => true, cancel: async () => { throw new Error("must not call"); }, read: async () => { throw new Error("must not read"); }, changed() { assert.fail(); }, observed() { assert.fail(); } });
  await op.refresh(); for (const status of ["completed", "failed", "cancelled", "other"]) await op.confirm(status);
});

test("old read cannot replace a newer task's successful cancellation", async () => {
  const h = host(), oldRead = deferred<unknown>(); h.read(() => oldRead.promise);
  confirm(h); await settle(); h.task("task-b"); h.render();
  h.read(async () => ({ task_id: "task-b", status: "cancelled" })); confirm(h); await settle();
  const before = text(h.render()); assert.match(before, /cancellation.accepted/);
  oldRead.resolve({ task_id: "task-a", status: "failed" }); await settle();
  assert.equal(text(h.render()), before); assert.equal(h.invalidated.length, 2);
});

for (const unavailable of ["error", "loading", "wrong"]) test(`actual parent ${unavailable} recovery retains an in-flight cancellation fence`, async () => {
  const h = host(), pending = deferred<unknown>(); h.write(() => pending.promise);
  click(h.render(true), "request"); click(h.render(true), "confirm");
  h.detail(unavailable); assert.match(text(h.render(true)), /cancellation.sending/);
  h.detail("available"); assert.match(text(h.render(true)), /cancellation.sending/);
  assert.doesNotMatch(text(h.render(true)), /cancellation.request|cancellation.confirm/);
  pending.reject(new Error("response lost")); await settle();
  assert.match(text(h.render(true)), /cancellation.unknown/);
  h.detail("error"); assert.match(text(h.render(true)), /cancellation.unknown/);
  click(h.render(true), "refresh"); await settle(); h.detail("available");
  assert.match(text(h.render(true)), /cancellation.unknown/);
  click(h.render(true), "refresh"); await settle();
  assert.equal(h.calls.filter(call => call.method === "DELETE").length, 1);
});

test("inline confirmation retains trigger, announces target, and explicitly restores focus", async () => {
  const h = host(); let tree = h.render();
  assert.equal(button(tree, "request")["aria-expanded"], false);
  click(tree, "request"); tree = h.render();
  const trigger = button(tree, "request"); assert.equal(trigger["aria-expanded"], true);
  const group = nodes(tree).find(node => node.props.id === trigger["aria-controls"]);
  assert.ok(group); assert.equal(group.props.role, "group");
  const warning = nodes(tree).find(node => node.props.id === trigger["aria-describedby"]);
  assert.ok(warning); assert.equal(warning.props.role, "alert"); assert.match(text(warning), /task-a/);
  click(tree, "keep"); assert.equal(h.focused(), "tasks.cancellation.request");
  assert.equal(button(h.render(), "request")["aria-expanded"], false);
  click(h.render(), "request"); click(h.render(), "confirm");
  assert.equal(h.focused(), "section"); await settle();
});
