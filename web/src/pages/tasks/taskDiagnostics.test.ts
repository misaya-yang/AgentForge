import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as diagnostics from "./taskDiagnostics.ts";

const id = "12345678-1234-1234-1234-123456789abc";
test("summary allowlists receipt metadata and never serializes task data", () => {
  const receipt = { task_id: id, status: "failed", error: "SENTINEL_PASSWORD", result: { token: "SENTINEL_TOKEN" }, service_id: "SENTINEL_SERVICE", prompt: "SENTINEL_PROMPT" };
  const result = diagnostics.taskDiagnosticSummary(receipt, "available", "2026-10-13T00:00:00.000Z");
  assert.ok(!result.includes("SENTINEL"));
  assert.deepEqual(JSON.parse(result), { schema: "task-diagnostic/v1", task_id: id, task_id_omitted: false, receipt_status: "failed", result_read: "not_requested", snapshot_created_at: "2026-10-13T00:00:00.000Z", scope: "client_observation_only", side_effects: "not_verified" });
});
test("unknown statuses and non-UUID identities cannot export arbitrary text", () => {
  const result = diagnostics.taskDiagnosticSummary({ task_id: "Bearer secret", status: "Authorization secret" }, "unknown", "now");
  assert.ok(!result.includes("secret"));
  assert.equal(JSON.parse(result).receipt_status, "unknown");
  assert.equal(JSON.parse(result).task_id, null);
  for (const status of ["pending", "processing", "completed", "failed", "cancelled"]) assert.equal(diagnostics.taskDiagnosticState(status), status);
});
test("completed receipt preserves independent result-read failure", () => {
  const summary = JSON.parse(diagnostics.taskDiagnosticSummary({ task_id: id, status: "completed" }, "unavailable", "now"));
  assert.equal(summary.receipt_status, "completed"); assert.equal(summary.result_read, "unavailable");
});

type Tree = { type: unknown; props: Record<string, unknown> };
function nodes(tree: unknown): Tree[] {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  const node = tree as Tree; return [node, ...nodes(node.props?.children)];
}
function text(tree: unknown): string {
  if (typeof tree === "string") return tree;
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  return tree && typeof tree === "object" ? text((tree as Tree).props?.children) : "";
}
function host() {
  const slots: unknown[] = []; let cursor = 0;
  const copied: string[] = []; let write: (value: string) => Promise<void> = async value => { copied.push(value); };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    react: { useState(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (next: unknown) => { slots[i] = next; }]; }, useRef(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = { current: initial }; return slots[i]; } },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/components/ui/button": { Button: "button" },
    "@/lib/clipboard": { copyToClipboard: (value: string) => write(value) },
    "../taskDiagnostics": diagnostics,
  };
  const exports: Record<string, (props: Record<string, unknown>) => Tree> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./components/TaskDiagnostics.tsx", import.meta.url), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, require(name: string) { assert.ok(name in modules, name); return modules[name]; }, Date });
  let refreshes = 0;
  const props = { receipt: { task_id: id, status: "completed" }, result: "unavailable", refreshing: false, refresh: () => { refreshes++; } };
  return { copied, props, render() { cursor = 0; return exports.TaskDiagnostics(props); }, setWrite(next: typeof write) { write = next; }, get refreshes() { return refreshes; } };
}
function click(tree: Tree, label: string) {
  const node = nodes(tree).find(item => item.type === "button" && text(item) === `tasks.diagnostics.${label}`);
  assert.ok(node, label); (node.props.onClick as () => void)();
}
async function settle() { await new Promise(resolve => setTimeout(resolve, 0)); }

test("actual component previews before copy and refreshes only through supplied read", async () => {
  const h = host(); let tree = h.render();
  assert.ok(text(tree).includes("tasks.diagnostics.resultUnavailable"));
  assert.ok(!nodes(tree).some(node => text(node) === "tasks.diagnostics.copy"));
  click(tree, "refresh"); assert.equal(h.refreshes, 1); assert.equal(h.copied.length, 0);
  click(tree, "preview"); tree = h.render();
  const preview = text(nodes(tree).find(node => node.type === "pre"));
  click(tree, "copy"); await settle();
  assert.deepEqual(h.copied, [preview]); assert.ok(text(h.render()).includes("tasks.diagnostics.copied"));
});
test("copy rejection is surfaced without a false success", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("synthetic denied"); });
  click(h.render(), "preview"); click(h.render(), "copy"); await settle();
  const tree = h.render(); assert.ok(text(tree).includes("tasks.diagnostics.failed")); assert.ok(!text(tree).includes("tasks.diagnostics.copied"));
  assert.ok(nodes(tree).some(node => node.props.role === "alert"));
});
test("rapid copy clicks dispatch once and keep reviewed snapshot stable", async () => {
  const h = host(); let done!: () => void; let writes = 0;
  h.setWrite(async () => { writes++; await new Promise<void>(resolve => { done = resolve; }); });
  click(h.render(), "preview"); const tree = h.render(); click(tree, "copy"); click(tree, "copy");
  assert.equal(writes, 1); done(); await settle();
  assert.ok(text(h.render()).includes("tasks.diagnostics.copied"));
});
