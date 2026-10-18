import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

interface Tree { type: unknown; props: Record<string, unknown> }
function fixture() {
  let current = true, calls = 0, reads = 0, refreshes = 0;
  let data = { enabled: true, verified: false, passkey_enrolled: true };
  let verify: () => Promise<void> = async () => { calls++; };
  const slots: unknown[] = []; let cursor = 0;
  let query!: { queryFn: () => Promise<unknown>; retry: boolean; gcTime: number };
  const component: Record<string, () => Tree> = {};
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  const modules: Record<string, unknown> = {
    react: { useRef(value: unknown) { const index = cursor++; return slots[index] ??= { current: value }; },
      useState(value: unknown) { const index = cursor++; if (!(index in slots)) slots[index] = value; return [slots[index], (next: unknown) => { slots[index] = next; }]; } },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/api/auth": { getPasskeyStepUpStatus: async () => { reads++; return data; }, verifyConsolePasskey: () => verify() },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => ({ id: "scope", isCurrent: () => current }) },
    "@tanstack/react-query": { useQuery(options: typeof query) { query = options; return { data, isFetching: false, isError: false, refetch: async () => { refreshes++; } }; } },
    "@/components/ui/button": { Button: "Button" },
    "@/components/ui/card": { Card: "Card", CardContent: "CardContent", CardDescription: "CardDescription", CardHeader: "CardHeader", CardTitle: "CardTitle" },
  };
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./PasskeyStepUp.tsx", import.meta.url), "utf8"), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports: component, require(name: string) { assert.ok(name in modules, name); return modules[name]; }, Error });
  const render = () => { cursor = 0; return component.PasskeyStepUp(); };
  function nodes(tree: unknown): Tree[] {
    if (!tree || typeof tree !== "object") return [];
    if (Array.isArray(tree)) return tree.flatMap(nodes);
    const node = tree as Tree; return [node, ...nodes(node.props.children)];
  }
  return { render, nodes, button: () => nodes(render()).find(node => node.type === "Button")!,
    query: () => query, calls: () => calls, reads: () => reads, refreshes: () => refreshes,
    setData(value: typeof data) { data = value; }, setVerify(value: typeof verify) { verify = value; }, changeAccount() { current = false; } };
}
const settle = async () => { for (let i = 0; i < 15; i++) await Promise.resolve(); };
test("settings exposes real step-up action and disables unavailable or unenrolled policy", async () => {
  const h = fixture(); let button = h.button();
  assert.equal(button.props.disabled, false); assert.equal(h.query().retry, false); assert.equal(h.query().gcTime, 0);
  await h.query().queryFn(); assert.equal(h.reads(), 1);
  (button.props.onClick as () => void)(); await settle();
  assert.equal(h.calls(), 1); assert.equal(h.refreshes(), 1);
  for (const data of [{ enabled: false, verified: false, passkey_enrolled: false }, { enabled: true, verified: false, passkey_enrolled: false }]) {
    h.setData(data); button = h.button(); assert.equal(button.props.disabled, true);
  }
});
test("settings prevents double activation and discards late results on account switch", async () => {
  const h = fixture(); let resolve!: () => void, calls = 0;
  h.setVerify(() => { calls++; return new Promise(done => { resolve = done; }); });
  const click = h.button().props.onClick as () => void; click(); click(); assert.equal(calls, 1);
  h.changeAccount(); resolve(); await settle(); assert.equal(h.refreshes(), 0);
  await assert.rejects(h.query().queryFn(), /session changed/);
});
test("failed passkey verification shows an alert without publishing success", async () => {
  const h = fixture(); h.setVerify(async () => { throw new Error("cancelled"); });
  (h.button().props.onClick as () => void)(); await settle();
  assert.equal(h.refreshes(), 0);
  assert.ok(h.nodes(h.render()).some(node => node.props.role === "alert"));
});
