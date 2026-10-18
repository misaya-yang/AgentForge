import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as draft from "./routingPolicyDraft.ts";
type Tree = { type: unknown; props: Record<string, unknown> };
const p = () => ({ ...draft.initialRoutingPolicy("t"), policy_id: "policy", version: "v1", candidates: [{ target: { provider: "p", model: "m", version: "v" }, priority: 0, weight: 1 }] });
function fixture() {
  const slots: unknown[] = []; let cursor = 0, current = true;
  const calls: string[] = []; let publish: () => Promise<string> = async () => "a".repeat(64);
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: { useRef(value: unknown) { const i = cursor++; return slots[i] ??= { current: value }; },
      useState(value: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = typeof value === "function" ? value() : value; return [slots[i], (next: unknown) => { slots[i] = next; }]; } },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "react-router-dom": { Link: "Link" },
    "react-i18next": { useTranslation: () => ({ i18n: { language: "en-US" } }) },
    "./routingPolicyDraft": draft,
    "@/api/routingPolicy": {
      async publishRoutingPolicy(value: unknown) { calls.push("publish"); assert.deepEqual(JSON.parse(JSON.stringify(value)), p()); return publish(); },
      async readRoutingPolicy(id: string, version: string) { calls.push(`read ${id}/${version}`); return Object.fromEntries(Object.entries(p()).reverse()); },
      async activateRoutingPolicy(id: string, version: string, expected: unknown) { calls.push(`activate ${id}/${version}/${expected}`); return "a".repeat(64); },
      async readRoutingDecision(id: string) { calls.push(`decision ${id}`); return { id }; },
    },
    "@/store/useAuthStore": {}, "@/hooks/useAuthenticatedReadScope": {},
    "@/components/ui/button": { Button: "Button" }, "@/components/ui/input": { Input: "Input" }, "@/components/ui/textarea": { Textarea: "Textarea" },
  };
  const exports: Record<string, (props: unknown) => Tree> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./RoutingPolicySettings.tsx", import.meta.url), "utf8").replace("function Editor(", "export function Editor("), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, Error, JSON, require(name: string) { assert.ok(name in modules, name); return modules[name]; } });
  const render = () => { cursor = 0; return exports.Editor({ tenant: "t", isCurrent: () => current }); };
  const nodes = (value: unknown): Tree[] => Array.isArray(value) ? value.flatMap(nodes) : value && typeof value === "object" ? [value as Tree, ...nodes((value as Tree).props?.children)] : [];
  const button = (label: string) => nodes(render()).find(node => node.type === "Button" && node.props.children === label)!;
  const setup = () => {
    const tree = nodes(render()); (tree.find(node => node.type === "Textarea")!.props.onChange as (e: unknown) => void)({ target: { value: JSON.stringify(p()) } });
    (nodes(render()).find(node => node.type === "input" && node.props.type === "checkbox")!.props.onChange as (e: unknown) => void)({ target: { checked: true } });
  };
  return { calls, render, button, setup, replaceSession() { current = false; }, setPublish(fn: typeof publish) { publish = fn; }, nodes };
}
const settle = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
test("routing UI publishes once after review and verifies exact immutable readback", async () => {
  const h = fixture(); h.setup(); const click = h.button("Publish reviewed version").props.onClick as () => void;
  click(); click(); await settle(); assert.deepEqual(h.calls, ["publish", "read policy/v1"]);
  assert.equal(h.button("Publish reviewed version").props.disabled, true);
});
test("routing UI never follows an old account's pending publish with reads or activation", async () => {
  const h = fixture(); let resolve!: (pin: string) => void;
  h.setPublish(() => new Promise(done => { resolve = done; })); h.setup();
  (h.button("Publish reviewed version").props.onClick as () => void)();
  h.replaceSession(); resolve("a".repeat(64)); await settle(); assert.deepEqual(h.calls, ["publish"]);
});
test("unconfirmed publish retains draft and requires another explicit review; no retry", async () => {
  const h = fixture(); h.setPublish(async () => { throw new Error("unknown commit outcome"); }); h.setup();
  (h.button("Publish reviewed version").props.onClick as () => void)(); await settle();
  assert.deepEqual(h.calls, ["publish"]); assert.equal(h.button("Publish reviewed version").props.disabled, true);
  assert.ok(h.nodes(h.render()).some(node => node.props.role === "status"));
});
