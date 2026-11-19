import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { taskOutputItems, taskOutputPreview, OUTPUT_PREVIEW_LIMIT } from "./taskOutputPreview.ts";

type Tree = { type: unknown; props: Record<string, unknown>; key?: string };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const node = value as Tree; return [node, ...nodes(node.props?.children)]; }
function host(initial: unknown) {
  let slots: Slot[] = []; const outer: Slot[] = []; let inner: Slot[] = []; let innerKey: string | undefined; let cursor = 0; let effects: Array<() => void> = []; let current = true; let sequence = 0;
  let result = initial; let taskId = "task-a"; const copied: string[] = []; let rejectCopy = false; let finishCopy: (() => void) | null = null;
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const react = { useState(initial: unknown) { const i = cursor++; const owned = slots; owned[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [owned[i].value, (next: unknown) => { owned[i].value = typeof next === "function" ? next(owned[i].value) : next; }]; }, useMemo: memo, useRef: (value: unknown) => memo(() => ({ current: value }), []), useLayoutEffect: effect };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = { react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string, args?: Record<string, unknown>) => key === "tasks.outputs.item" ? `item-${args?.number}` : key }) },
    "@/components/ui/button": { Button: "Button" }, "@/lib/utils": { generateUUID: () => String(++sequence) },
    "@/lib/clipboard": { copyToClipboard: async (text: string) => { copied.push(text); if (rejectCopy) throw new Error("denied"); await new Promise<void>(resolve => { finishCopy = resolve; }); } },
    "../taskOutputPreview": { taskOutputItems, taskOutputPreview, OUTPUT_PAGE_SIZE: 20 },
  };
  const exports: Record<string, (props: unknown) => Tree> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./components/TaskOutputBrowser.tsx", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } });
  const scope = { id: "scope-a", isCurrent: () => current };
  return { copied,
    render() { slots = outer; cursor = 0; const tree = exports.TaskOutputBrowser({ taskId, requestId: "request-a", result, scope }); const pending = effects; effects = []; pending.forEach(fn => fn()); if (typeof tree.type !== "function") return tree; if (innerKey !== tree.key) { inner = []; innerKey = tree.key; } slots = inner; cursor = 0; return tree.type(tree.props); },
    replace(next: unknown) { result = next; }, task(id: string) { taskId = id; }, retire() { current = false; }, failCopy() { rejectCopy = true; }, finishCopy() { (finishCopy as (() => void) | null)?.(); },
    unmount() { outer.forEach(slot => slot.cleanup?.()); },
  };
}
function response(outputs: unknown[]) { return { request_id: "request-a", task_id: "task-a", status: "success", outputs }; }
function button(tree: unknown, label: string) { const found = nodes(tree).find(n => n.type === "Button" && n.props.children === label); assert.ok(found, label); return found; }
function item(tree: unknown, number: number) { const found = nodes(tree).find(n => n.type === "Button" && Array.isArray(n.props.children) && n.props.children[0] === `item-${number}`); assert.ok(found, String(number)); return found; }
function click(node: Tree) { (node.props.onClick as () => void)(); }
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };
test("actual output browser reads selected text and keeps media URLs inert descriptors", () => {
  const h = host(response([{ type: "text", data: "<script>alert(1)</script>" }, { type: "image", url: "https://private.example/image", mime_type: "image/png" }]));
  assert.equal(nodes(h.render()).filter(n => n.type === "pre").length, 0); click(item(h.render(), 1)); let tree = h.render(); assert.equal(nodes(tree).find(n => n.type === "pre")?.props.children, "<script>alert(1)</script>");
  click(item(tree, 2)); tree = h.render(); assert.equal(nodes(tree).some(n => n.props.src || n.props.href || n.props.dangerouslySetInnerHTML), false); assert.ok(nodes(tree).find(n => n.props.children === "tasks.outputs.descriptor")); assert.equal(h.copied.length, 0);
});
test("45 outputs remain reachable in 20-item pages without serializing unselected payloads", () => {
  const h = host(response(Array.from({ length: 45 }, (_, index) => ({ type: "text", data: String(index) }))));
  assert.equal(nodes(h.render()).filter(n => n.props["aria-pressed"] !== undefined).length, 20); click(button(h.render(), "tasks.outputs.next")); click(button(h.render(), "tasks.outputs.next")); assert.equal(nodes(h.render()).filter(n => n.props["aria-pressed"] !== undefined).length, 5); click(item(h.render(), 45)); assert.equal(nodes(h.render()).find(n => n.type === "pre")?.props.children, "44"); assert.equal(button(h.render(), "tasks.outputs.next").props.disabled, true);
});
test("huge text is explicitly bounded and copy contains exactly the displayed excerpt", async () => {
  const h = host(response([{ type: "text", data: "x".repeat(200000) }])); click(item(h.render(), 1)); const tree = h.render(); const preview = nodes(tree).find(n => n.type === "pre")?.props.children; assert.equal((preview as string).length, OUTPUT_PREVIEW_LIMIT); assert.ok(nodes(tree).find(n => n.props.children === "tasks.outputs.truncated")); click(button(tree, "tasks.outputs.copy")); assert.equal(h.copied[0], preview); h.finishCopy(); await settle(); assert.ok(nodes(h.render()).find(n => n.props.children === "tasks.outputs.copied"));
});
test("deep and large JSON preview has bounded traversal and never claims a complete export", () => {
  const deep: Record<string, unknown> = {}; let cursor = deep; for (let i = 0; i < 100; i++) { cursor.next = {}; cursor = cursor.next as Record<string, unknown>; }
  assert.equal(taskOutputPreview(deep).truncated, true); assert.equal(taskOutputPreview(Array.from({ length: 5000 }, () => "x")).truncated, true); const text = taskOutputPreview({ value: "a".repeat(200000) }); assert.ok(text.text.length <= OUTPUT_PREVIEW_LIMIT); assert.equal(text.truncated, true);
});
test("clipboard failure is visible and selection changes retire old copy callbacks", async () => {
  const h = host(response([{ type: "text", data: "one" }, { type: "text", data: "two" }])); h.failCopy(); click(item(h.render(), 1)); const oldCopy = button(h.render(), "tasks.outputs.copy"); click(oldCopy); await settle(); assert.ok(nodes(h.render()).find(n => n.props.role === "alert" && n.props.children === "tasks.outputs.failed")); click(item(h.render(), 2)); click(oldCopy); assert.equal(h.copied.length, 1); click(button(h.render(), "tasks.outputs.copy")); assert.equal(h.copied[1], "two");
});
test("result replacement, task navigation, authority retirement and disposal fence stale copies", () => {
  for (const change of ["replace", "task", "auth", "unmount"]) {
    const h = host(response([{ type: "text", data: "old" }])); click(item(h.render(), 1)); const old = button(h.render(), "tasks.outputs.copy");
    if (change === "replace") { h.replace(response([{ type: "text", data: "new" }])); assert.equal(nodes(h.render()).filter(n => n.type === "pre").length, 0); }
    if (change === "task") { h.task("task-b"); h.render(); }
    if (change === "auth") h.retire(); if (change === "unmount") h.unmount(); click(old); assert.equal(h.copied.length, 0, change);
  }
});
test("identity mismatch, empty output and unknown items are distinguished without guessing", () => {
  assert.equal(taskOutputItems(response([]), "task-b", "request-a"), null); assert.equal(taskOutputItems(response([]), "task-a", "request-b"), null);
  const empty = host(response([])); assert.ok(nodes(empty.render()).find(n => n.props.children === "tasks.outputs.empty")); const unknown = host(response([null])); click(item(unknown.render(), 1)); assert.ok(nodes(unknown.render()).find(n => n.props.children === "tasks.outputs.descriptor"));
});
test("task page integrates typed browser while partial detail output remains separate", () => {
  const source = readFileSync(new URL("./components/TaskQueryTab.tsx", import.meta.url), "utf8"); assert.ok(source.includes("useTask(searchedId, readScope)")); assert.ok(source.includes("useTaskResult(completedTaskId, readScope)")); assert.ok(source.includes("requestId={taskQuery.data.request_id}")); assert.ok(source.includes("<TaskOutputBrowser")); assert.ok(source.includes("tasks.partialResult"));
});

test("production task scope synchronously retires credential and authority A-B-A before rerender", () => {
  let auth = { token: "a", isAuthenticated: true, user: { user_id: "user-a", permissions: ["read"] } };
  let listener: ((state: typeof auth, prior: typeof auth) => void) | undefined; let cleanup: (() => void) | undefined;
  const exports: Record<string, () => { id: string; isCurrent: () => boolean }> = {};
  const modules: Record<string, unknown> = {
    react: { useState: () => [0, () => {}], useMemo: (fn: () => unknown) => fn(), useCallback: (fn: unknown) => fn, useLayoutEffect: (fn: () => () => void) => { cleanup = fn(); } },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (value: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (fn: typeof listener) => { listener = fn; return () => { listener = undefined; }; } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) }, "@/lib/utils": { generateUUID: () => "opaque" },
  };
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("../../hooks/useAuthenticatedReadScope.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText, { exports, require: (id: string) => modules[id] });
  const scope = exports.useAuthenticatedReadScope(); assert.equal(scope.isCurrent(), true);
  const original = auth; auth = { ...auth, token: "b" }; listener!(auth, original); const b = auth; auth = original; listener!(auth, b); assert.equal(scope.isCurrent(), false);
  const next = exports.useAuthenticatedReadScope(); assert.equal(next.isCurrent(), true); const before = auth; auth = { ...auth, user: { ...auth.user, permissions: [] } }; listener!(auth, before); const changed = auth; auth = before; listener!(auth, changed); assert.equal(next.isCurrent(), false);
  const final = exports.useAuthenticatedReadScope(); cleanup!(); assert.equal(final.isCurrent(), false);
});
