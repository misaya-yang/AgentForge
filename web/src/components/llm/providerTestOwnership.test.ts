import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

// Complete production Form → API → Card modules, with controlled form values,
// React scheduling and HTTP. No provider calls, backend execution or browser.
type Tree = { type: unknown; props: Record<string, unknown> };
type Owner = { slots: unknown[]; effects: (() => void)[]; cleanup: (() => void)[] };
type Result = { success: boolean; message: string; latency_ms?: number };
type Provider = { provider_id: string; tenant_id: string; display_name: string; api_type: string;
  base_url: string; has_api_key: boolean; is_enabled: boolean; created_at: string; updated_at: string };
function nodes(tree: unknown, predicate: (node: Tree) => boolean): Tree[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(item => nodes(item, predicate));
  const node = tree as Tree;
  return [...(predicate(node) ? [node] : []), ...nodes(node.props?.children, predicate)];
}
function deferred() {
  let resolve!: (result: Result) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<Result>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function host() {
  let provider: Provider = { provider_id: "fixture-provider", tenant_id: "fixture-tenant",
    display_name: "Fixture Provider", api_type: "openai", base_url: "https://old.fixture.invalid",
    has_api_key: true, is_enabled: true, created_at: "2026-01-01T00:00:00Z", updated_at: "revision-A" };
  const calls: { method: string; url: string; body?: unknown; observedRevision?: string }[] = [];
  const queue: ReturnType<typeof deferred>[] = [];
  let revision = 0;
  let failSave = false;
  let visibleProvider = provider;
  const modules: Record<string, unknown> = {};
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props });
  function load(file: string): Record<string, unknown> {
    const exports: Record<string, unknown> = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(file, import.meta.url), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, require(name: string) {
      if (name in modules) return modules[name];
      if (name.startsWith("@/components/ui/") || name === "lucide-react") return new Proxy({}, { get: (_target, key) => key });
      throw new Error(`Unexpected module ${name}`);
    } });
    return exports;
  }
  const owner: Owner = { slots: [], effects: [], cleanup: [] };
  let cursor = 0;
  const same = (a?: unknown[], b?: unknown[]) => a?.length === b?.length && a?.every((v, i) => Object.is(v, b?.[i]));
  const react = {
    useState(initial: unknown) {
      const i = cursor++;
      if (!(i in owner.slots)) owner.slots[i] = initial;
      return [owner.slots[i], (next: unknown) => { owner.slots[i] = typeof next === "function" ? next(owner.slots[i]) : next; }];
    },
    useMemo(compute: () => unknown, deps: unknown[]) {
      const i = cursor++; const old = owner.slots[i] as { deps: unknown[]; value: unknown } | undefined;
      if (!old || !same(old.deps, deps)) owner.slots[i] = { deps, value: compute() };
      return (owner.slots[i] as { value: unknown }).value;
    },
    useEffect(run: () => void | (() => void), deps: unknown[]) {
      const i = cursor++;
      if (!same(owner.slots[i] as unknown[] | undefined, deps)) {
        owner.slots[i] = deps;
        owner.effects.push(() => { owner.cleanup[i]?.(); owner.cleanup[i] = run() || (() => {}); });
      }
    },
  };
  modules.react = { ...react, useLayoutEffect: react.useEffect };
  modules["react/jsx-runtime"] = { jsx, jsxs: jsx };
  modules["react-i18next"] = { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) };
  modules["@/lib/utils"] = { cn: (...values: unknown[]) => values.filter(Boolean).join(" ") };
  modules["@/lib/api"] = { api: {
    async post(url: string, ...body: unknown[]) {
      calls.push({ method: "POST", url, body, observedRevision: provider.updated_at });
      const next = queue.shift();
      return { data: next ? await next.promise : { success: true, message: "Connection successful", latency_ms: 7 } };
    },
    async put(url: string, body: Partial<Provider>) {
      calls.push({ method: "PUT", url, body });
      if (failSave) throw new Error("Fixture save rejected");
      provider = { ...provider, ...JSON.parse(JSON.stringify(body)), updated_at: `saved-${++revision}` };
      return { data: provider };
    },
  } };
  const api = load("../../api/providers.ts") as { updateProvider: (id: string, body: unknown) => Promise<unknown> };
  modules["@/api/providers"] = api;
  const card = load("./ProviderCard.tsx") as { ProviderCard: (props: Record<string, unknown>) => Tree };
  const pageSlots: unknown[] = []; let pageCursor = 0;
  modules.react = {
    useState(initial: unknown) { const i = pageCursor++; if (!(i in pageSlots)) pageSlots[i] = initial;
      return [pageSlots[i], (next: unknown) => { pageSlots[i] = typeof next === "function" ? next(pageSlots[i]) : next; }]; },
    useMemo: (compute: () => unknown) => compute(), useRef: (value: unknown) => ({ current: value }), useEffect() {},
  };
  let refreshes = 0;
  modules["@tanstack/react-query"] = {
    useQueryClient: () => ({ invalidateQueries() { refreshes++; } }),
    useQuery: (options: { queryKey: string[] }) => ({ data: options.queryKey[0] === "providers" ? [visibleProvider] : [], isLoading: false }),
    useMutation: (options: { mutationFn: (body: unknown) => Promise<unknown>; onSuccess?: (result: unknown, body: unknown) => void; onError?: (error: unknown) => void }) => ({
      isPending: false, async mutateAsync(body: unknown) {
        try { const result = await options.mutationFn(body); options.onSuccess?.(result, body); return result; }
        catch (error) { options.onError?.(error); throw error; }
      },
    }),
  };
  modules["react-router-dom"] = { Link: "Link", useSearchParams: () => [new URLSearchParams("tab=providers"), () => {}] };
  modules["@/hooks/use-toast"] = { useToast: () => ({ toast() {} }) };
  modules["@/hooks/useServices"] = { useServices: () => ({ data: [] }), useHealth: () => ({ data: {} }), useArchitectureStatus: () => ({ data: {} }) };
  modules["@/store/useAppStore"] = { useAppStore: () => ({ selectedServiceId: undefined, setSelectedServiceId() {} }) };
  modules["@/store/useAuthStore"] = { useAuthStore: (select: (value: unknown) => unknown) => select({ user: {}, hasPermission: () => true }) };
  modules["@/components/services/architectureStatusPresentation"] = { canViewArchitectureStatus: () => false };
  for (const [name, exports] of Object.entries({
    "@/components/ManagementQueryState": ["ManagementQueryState"], "@/components/ServiceCard": ["ServiceCard"],
    "@/components/ServiceForm": ["ServiceForm"], "@/components/services/ArchitectureStatusPanel": ["ArchitectureStatusPanel"],
    "@/components/llm": ["ProviderCard", "ProviderForm", "ModelTable", "ModelForm"],
    "@/components/llm/ProviderCatalogReview": ["ProviderCatalogReview"],
  })) modules[name] = Object.fromEntries(exports.map(key => [key, key]));
  modules["@/api/models"] = { modelQueryKeys: { all: ["models"] } };
  modules["@/lib/utils"] = { cn: (...values: unknown[]) => values.filter(Boolean).join(" "), getErrorMessage: () => "Fixture save rejected" };
  modules["@/hooks/useManagedModels"] = { useManagedModels: () => ({ query: { data: [], isLoading: false }, ready: true, busy: false, notice: null }) };
  const page = load("../../pages/Services.tsx") as { ServicesPage: () => Tree };
  const renderPage = () => { pageCursor = 0; return page.ServicesPage(); };
  const cardProps = () => nodes(renderPage(), node => node.type === "ProviderCard")[0].props;
  function render() {
    cursor = 0;
    const tree = card.ProviderCard(cardProps());
    owner.effects.splice(0).forEach(effect => effect());
    return tree;
  }
  function button() { return nodes(render(), node => node.type === "Button" && node.props["aria-label"] === "Test")[0]; }
  const click = () => (button().props.onClick as () => Promise<void>)();
  const text = (message: string) => nodes(render(), node => node.type === "span" && node.props.children === message).length > 0;
  async function save(changes: Partial<Provider> & { api_key?: string } = { base_url: "https://changed.fixture.invalid" }, readback = true) {
    (cardProps().onEdit as () => void)();
    const formProps = nodes(renderPage(), node => node.type === "ProviderForm")[0].props;
    const data = { ...provider, api_key: "", vertex_project: "", vertex_location: "us-central1", ...changes };
    const original = modules.react;
    modules.react = { useState: (initial: unknown) => [initial, () => {}], useRef: (initial: unknown) => ({ current: initial }),
      useMemo: (fn: () => unknown) => fn(), useEffect: () => {} };
    modules["react-hook-form"] = { useForm: () => ({ register: () => ({}), handleSubmit: (fn: (value: unknown) => unknown) => () => fn(data),
      reset: () => {}, watch: (key: keyof typeof data) => data[key], setValue: () => {}, formState: { errors: {} } }) };
    const form = load("./ProviderForm.tsx") as { ProviderForm: (props: Record<string, unknown>) => Tree };
    const tree = form.ProviderForm(formProps);
    await (nodes(tree, node => node.type === "form")[0].props.onSubmit as () => Promise<void>)();
    modules.react = original;
    if (readback) visibleProvider = provider;
    render(); // Simulates query readback publishing the newly observed revision.
  }
  return { calls, click, text, button, render, save,
    failSave() { failSave = true; }, refreshes: () => refreshes,
    defer() { const next = deferred(); queue.push(next); return next; },
    replace(change: Partial<Provider>) { provider = { ...provider, ...change }; visibleProvider = provider; render(); },
    unmount() { owner.cleanup.forEach(cleanup => cleanup?.()); },
  };
}
const success = { success: true, message: "Connection successful" };

test("Test sends persisted identity only; unchanged provider objects retain feedback", async () => {
  const h = host(); h.render(); assert.equal(h.calls.length, 0);
  await h.click(); assert.equal(h.text(success.message), true);
  assert.equal(JSON.stringify(h.calls[0]), JSON.stringify({ method: "POST", url: "/api/v1/providers/fixture-provider/test", body: [], observedRevision: "revision-A" }));
  h.replace({}); assert.equal(h.text(success.message), true);
});
test("saved endpoint readback invalidates success without automatically probing", async () => {
  const h = host(); await h.click(); await h.save();
  assert.equal(h.text(success.message), false);
  assert.equal(h.calls.length, 2); assert.equal(h.calls[1].method, "PUT");
  assert.equal((h.calls[1].body as Provider).base_url, "https://changed.fixture.invalid");
});
test("credential-only edit invalidates feedback through public revision", async () => {
  const h = host(); await h.click(); await h.save({ api_key: "synthetic-fixture-value" });
  assert.equal(h.text(success.message), false); assert.equal(h.calls.length, 2);
});
for (const outcome of ["success", "failure", "rejection"] as const) {
  test(`obsolete ${outcome} cannot replace a new configuration's pending or successful test`, async () => {
    const h = host(); const old = h.defer(); const first = h.click(); await h.save();
    const current = h.defer(); const second = h.click();
    if (outcome === "rejection") old.reject(new Error("obsolete failure"));
    else old.resolve(outcome === "success" ? success : { success: false, message: "obsolete failure" });
    await first; assert.equal(h.button().props.disabled, true);
    assert.equal(h.text(success.message), false); assert.equal(h.text("obsolete failure"), false);
    current.resolve({ success: true, message: "current success" }); await second;
    assert.equal(h.text("current success"), true); assert.equal(h.button().props.disabled, false);
  });
}
test("A to B to A configuration revisions cannot revive A's old test", async () => {
  const h = host(); const old = h.defer(); const first = h.click();
  h.replace({ updated_at: "revision-B" }); h.replace({ updated_at: "revision-A" });
  old.resolve(success); await first; assert.equal(h.text(success.message), false);
});
test("same-turn duplicate pending clicks dispatch only once, explicit later test is allowed", async () => {
  const h = host(); const old = h.defer(); const click = h.button().props.onClick as () => Promise<void>;
  const first = click(); await click(); assert.equal(h.calls.length, 1);
  old.resolve(success); await first; await h.click(); assert.equal(h.calls.length, 2);
});
test("current failure is displayed and explicit retry may succeed", async () => {
  const h = host(); const pending = h.defer(); const first = h.click(); pending.resolve({ success: false, message: "Connection failed" });
  await first; assert.equal(h.text("Connection failed"), true); await h.click(); assert.equal(h.text(success.message), true);
});
test("unmount discards pending feedback and remount starts clean", async () => {
  const h = host(); const pending = h.defer(); const first = h.click(); h.unmount();
  pending.resolve(success); await first; assert.equal(h.text(success.message), false);
  const remount = host(); assert.equal(remount.text(success.message), false); await remount.click(); assert.equal(remount.text(success.message), true);
});

test("successful parent PUT callback hides old success even when list refresh never publishes", async () => {
  const h = host(); await h.click(); await h.save(undefined, false);
  assert.equal(h.text(success.message), false); assert.equal(h.refreshes(), 1);
  assert.equal(h.calls.length, 2);
});
test("failed PUT preserves legitimate current result and does not auto-test", async () => {
  const h = host(); await h.click(); h.failSave();
  await assert.rejects(h.save(), /Fixture save rejected/);
  assert.equal(h.text(success.message), true); assert.equal(h.refreshes(), 0); assert.equal(h.calls.length, 2);
});

test("revision-only readback invalidates feedback without a local save generation", async () => {
  const h = host(); await h.click(); h.replace({ updated_at: "revision-key-only" });
  assert.equal(h.text(success.message), false); assert.equal(h.calls.length, 1);
});
for (const change of [{ tenant_id: "fixture-other-tenant" }, { provider_id: "fixture-other-provider" }]) {
  test(`identity change ${Object.keys(change)[0]} invalidates same-timestamp feedback`, async () => {
    const h = host(); const old = h.defer(); const first = h.click(); h.replace(change);
    old.resolve(success); await first; assert.equal(h.text(success.message), false);
  });
}
for (const outcome of ["success", "rejection"] as const) {
  test(`late old ${outcome} preserves already completed new feedback`, async () => {
    const h = host(); const old = h.defer(); const first = h.click(); await h.save();
    const current = h.defer(); const second = h.click(); current.resolve({ success: true, message: "current success" }); await second;
    if (outcome === "rejection") old.reject(new Error("obsolete failure")); else old.resolve(success);
    await first; assert.equal(h.text("current success"), true); assert.equal(h.text(success.message), false);
  });
}
