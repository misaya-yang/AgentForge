import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Executes the complete production table with controlled React scheduling and
// callback promises. This is not browser, HTTP or service-state acceptance.
type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
type Model = { tenant_id: string; provider_id: string; model_id: string; is_enabled: boolean };
function nodes(tree: unknown, type: string): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(item => nodes(item, type));
  const node = tree as Tree;
  return [...(node.type === type ? [node] : []), ...nodes(node.props?.children, type)];
}
function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function host() {
  const models: Model[] = [
    { tenant_id: "tenant-a", provider_id: "provider-a", model_id: "shared-name", is_enabled: true },
    { tenant_id: "tenant-a", provider_id: "provider-b", model_id: "shared-name", is_enabled: false },
    { tenant_id: "tenant-a", provider_id: "provider-a", model_id: "other", is_enabled: true },
  ];
  const slots: unknown[] = []; let cursor = 0;
  const calls: { model: Model; enabled: boolean; operation: ReturnType<typeof deferred> }[] = [];
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const exports: Record<string, unknown> = {};
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./ModelTable.tsx", import.meta.url), "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require(name: string) {
    if (name === "react") return {
      useState(initial: unknown) {
        const i = cursor++; if (!(i in slots)) slots[i] = initial;
        return [slots[i], (next: unknown) => { slots[i] = typeof next === "function" ? next(slots[i]) : next; }];
      },
      useRef(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = { current: initial }; return slots[i]; },
    };
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    if (name === "react-i18next") return { useTranslation: () => ({ t: (key: string) => key }) };
    if (name === "@/lib/utils") return { cn: (...values: unknown[]) => values.filter(Boolean).join(" ") };
    if (name === "@/api/models") return { formatContextWindow: String, formatPrice: String, getAccessLevelDisplayName: String };
    if (name.startsWith("@/components/ui/") || name === "lucide-react") return new Proxy({}, { get: (_object, key) => key });
    throw new Error(`Unexpected module ${name}`);
  } });
  let loading = false; let writable = true; let tree: unknown;
  const render = () => {
    cursor = 0;
    tree = (exports.ModelTable as (props: unknown) => unknown)({ models, providers: {}, loading,
      onToggle: writable ? (model: Model, enabled: boolean) => {
        const operation = deferred(); calls.push({ model, enabled, operation }); return operation.promise;
      } : undefined,
    });
  };
  const switches = () => nodes(tree, "Switch");
  const click = (index: number) => (switches()[index].props.onCheckedChange as (enabled: boolean) => Promise<void>)(!models[index].is_enabled);
  render();
  return { models, calls, render, click, switches, rows: () => nodes(tree, "TableRow").filter(row => row.key !== undefined),
    setLoading(value: boolean) { loading = value; render(); }, setWritable(value: boolean) { writable = value; render(); } };
}

test("same-named provider models have distinct React identity and pending state", async () => {
  const h = host(); assert.equal(new Set(h.rows().map(row => row.key)).size, 3);
  const operation = h.click(0); h.render();
  assert.equal(h.switches()[0].props.disabled, true); assert.equal(h.switches()[1].props.disabled, false);
  assert.equal(h.calls[0].model.provider_id, "provider-a"); assert.equal(h.calls[0].enabled, false);
  h.calls[0].operation.resolve(); await operation; h.render(); assert.equal(h.switches()[0].props.disabled, false);
});

test("duplicate events before rerender dispatch only one mutation", async () => {
  const h = host(); const first = h.click(0); const duplicate = h.click(0);
  assert.equal(h.calls.length, 1); h.calls[0].operation.resolve(); await Promise.all([first, duplicate]);
});

for (const firstToFinish of [0, 1]) {
  test(`concurrent row completion order ${firstToFinish} keeps the other operation locked`, async () => {
    const h = host(); const operations = [h.click(0), h.click(2)]; h.render();
    assert.equal(h.switches()[0].props.disabled, true); assert.equal(h.switches()[2].props.disabled, true);
    h.calls[firstToFinish].operation.resolve(); await operations[firstToFinish]; h.render();
    const remaining = 1 - firstToFinish; const remainingRow = remaining === 0 ? 0 : 2;
    assert.equal(h.switches()[remainingRow].props.disabled, true);
    await h.click(remainingRow); assert.equal(h.calls.length, 2);
    h.calls[remaining].operation.resolve(); await operations[remaining]; h.render();
    assert.equal(h.switches()[0].props.disabled, false); assert.equal(h.switches()[2].props.disabled, false);
  });
}

test("rejected mutation is consumed and only explicit retry dispatches again", async () => {
  const h = host(); const operation = h.click(0); h.calls[0].operation.reject(new Error("mutation failed"));
  await operation; h.render(); assert.equal(h.calls.length, 1); assert.equal(h.switches()[0].props.disabled, false);
  assert.equal(h.switches()[0].props.checked, true);
  const retry = h.click(0); assert.equal(h.calls.length, 2); h.calls[1].operation.resolve(); await retry;
});

test("loading or missing mutation callback cannot dispatch a toggle", async () => {
  const h = host(); h.setLoading(true); await h.click(0); assert.equal(h.calls.length, 0);
  h.setLoading(false); h.setWritable(false); assert.equal(h.switches()[0].props.disabled, true);
  await h.click(0); assert.equal(h.calls.length, 0);
});

test("row identity cannot collide through delimiter characters", () => {
  const h = host(); h.models[0].provider_id = "provider:a"; h.models[0].model_id = "b";
  h.models[1].provider_id = "provider"; h.models[1].model_id = "a:b"; h.render();
  assert.equal(new Set(h.rows().map(row => row.key)).size, 3);
});

test("same provider and model in different tenants have independent identities", () => {
  const h = host(); h.models[1] = { ...h.models[0], tenant_id: "tenant-b" }; h.render();
  assert.equal(new Set(h.rows().map(row => row.key)).size, 3);
});
