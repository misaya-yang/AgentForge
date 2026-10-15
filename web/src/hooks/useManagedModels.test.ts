import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver, MutationObserver } from "@tanstack/react-query";

type Model = { tenant_id: string; provider_id: string; model_id: string; is_enabled: boolean; updated_at: string };
type View = { query: { data?: Model[]; isError: boolean }; ready: boolean; busy: boolean; notice: string | null;
  toggle: (model: Model, enabled: boolean) => Promise<void>; review: () => Promise<void> };
function deferred<T>() {
  let resolve!: (value: T) => void; let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject };
}
async function settle() { for (let i = 0; i < 80; i++) await Promise.resolve(); }
function host() {
  // Deliberately retrying defaults: production hook must override mutation retry.
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity }, mutations: { retry: 2, retryDelay: 0, gcTime: Infinity } } });
  const slots: unknown[] = []; let cursor = 0; const cleanup: (() => void)[] = [];
  let scopeId = "scope-a"; let mounted = true; let reads = 0;
  let rows: Model[] = [{ tenant_id: "tenant-a", provider_id: "provider-a", model_id: "shared", is_enabled: true, updated_at: "v1" }];
  let reader = async () => structuredClone(rows);
  let writer = async (id: string, enabled: boolean, provider: string) => {
    rows = rows.map(row => row.model_id === id && row.provider_id === provider ? { ...row, is_enabled: enabled, updated_at: "v2" } : row); return rows[0];
  };
  const writes: { id: string; enabled: boolean; provider: string }[] = [];
  const exports: Record<string, unknown> = {};
  const same = (a: unknown[], b: unknown[]) => a.length === b.length && a.every((value, i) => Object.is(value, b[i]));
  vm.runInNewContext(ts.transpileModule(readFileSync(new URL("./useManagedModels.ts", import.meta.url), "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText, { exports, require(name: string) {
    if (name === "react") return {
      useState(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (value: unknown) => { slots[i] = value; }]; },
      useMemo(compute: () => unknown, deps: unknown[]) { const i = cursor++; const old = slots[i] as { deps: unknown[]; value: unknown } | undefined;
        if (!old || !same(old.deps, deps)) slots[i] = { deps, value: compute() }; return (slots[i] as { value: unknown }).value; },
    };
    if (name === "@/hooks/useAuthenticatedReadScope") return { useAuthenticatedReadScope() {
      const observed = scopeId; return { id: observed, isCurrent: () => mounted && scopeId === observed };
    } };
    if (name === "@/api/models") return { modelQueryKeys: { all: ["models"] },
      listModels: async () => { reads++; return reader(); },
      toggleModel: async (id: string, enabled: boolean, provider: string) => { writes.push({ id, enabled, provider }); return writer(id, enabled, provider); },
    };
    if (name === "@tanstack/react-query") return {
      useQueryClient: () => client,
      useQuery(options: ConstructorParameters<typeof QueryObserver>[1]) {
        const i = cursor++; let observer = slots[i] as QueryObserver | undefined;
        if (!observer) { observer = new QueryObserver(client, options); slots[i] = observer; cleanup.push(observer.subscribe(() => {})); }
        else observer.setOptions(options);
        return { ...observer.getCurrentResult(), refetch: observer.refetch.bind(observer) };
      },
      useMutation(options: ConstructorParameters<typeof MutationObserver>[1]) {
        const i = cursor++; let observer = slots[i] as MutationObserver | undefined;
        if (!observer) { observer = new MutationObserver(client, options); slots[i] = observer; cleanup.push(observer.subscribe(() => {})); }
        else observer.setOptions(options);
        return { ...observer.getCurrentResult(), mutateAsync: observer.mutate.bind(observer) };
      },
    };
    throw new Error(`Unexpected module ${name}`);
  } });
  const render = () => { cursor = 0; return (exports.useManagedModels as () => View)(); };
  return { render, writes, reads: () => reads, model: () => structuredClone(rows[0]), setRows(value: Model[]) { rows = value; },
    setRead(value: typeof reader) { reader = value; }, setWrite(value: typeof writer) { writer = value; },
    async start() { render(); await settle(); return render(); },
    switchScope() { scopeId = "scope-b"; return render(); },
    dispose() { mounted = false; cleanup.forEach(stop => stop()); client.clear(); } };
}

test("accepted status is confirmed only after matching readback and sends provider identity", async () => {
  const h = host(); try { const view = await h.start(); await view.toggle(h.model(), false);
    const current = h.render(); assert.equal(current.notice, "saved"); assert.equal(current.query.data?.[0].is_enabled, false);
    assert.deepEqual(h.writes, [{ id: "shared", enabled: false, provider: "provider-a" }]); assert.equal(h.reads(), 3);
  } finally { h.dispose(); }
});

test("same-render duplicate commands remain single-flight through readback", async () => {
  const h = host(); const pending = deferred<Model>(); try { const view = await h.start(); h.setWrite(() => pending.promise);
    const first = view.toggle(h.model(), false); const second = view.toggle(h.model(), false); await settle();
    assert.equal(h.writes.length, 1); assert.equal(h.render().busy, true);
    pending.resolve(h.model()); await Promise.all([first, second]); assert.equal(h.render().notice, "changed");
  } finally { h.dispose(); }
});

test("lost mutation response is not retried even under retrying global defaults", async () => {
  const h = host(); try { const view = await h.start(); h.setWrite(async () => { throw new Error("response lost"); });
    await view.toggle(h.model(), false); const unknown = h.render(); assert.equal(unknown.notice, "unknown");
    await unknown.toggle(h.model(), false); assert.equal(h.writes.length, 1); assert.equal(unknown.ready, false);
    await unknown.review(); assert.equal(h.render().ready, true); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});

test("a changed preflight issues no mutation until explicit review", async () => {
  const h = host(); try { const view = await h.start(); const original = h.model(); h.setRows([{ ...original, updated_at: "someone-else" }]);
    await view.toggle(original, false); assert.equal(h.writes.length, 0); assert.equal(h.render().notice, "conflict");
    await h.render().review(); assert.equal(h.render().ready, true);
  } finally { h.dispose(); }
});

test("failed readback preserves observed rows and blocks another command", async () => {
  const h = host(); try { const view = await h.start(); h.setWrite(async () => {
    h.setRead(async () => { throw new Error("read unavailable"); }); return h.model();
  }); await view.toggle(h.model(), false); const current = h.render(); assert.equal(current.notice, "readFailed");
    assert.equal(current.query.data?.[0].is_enabled, true); assert.equal(current.ready, false); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});

for (const stage of ["preflight", "write", "readback"] as const) {
  test(`scope replacement during ${stage} cannot publish or dispatch a follow-on operation`, async () => {
    const h = host(); const pendingRows = deferred<Model[]>(); const pendingWrite = deferred<Model>();
    try { const view = await h.start(); const original = h.model();
      if (stage === "preflight") h.setRead(() => pendingRows.promise);
      else h.setWrite(async () => { if (stage === "write") return pendingWrite.promise; h.setRead(() => pendingRows.promise); return original; });
      const work = view.toggle(original, false); await settle(); const count = h.reads(); h.switchScope();
      pendingRows.resolve([original]); pendingWrite.resolve(original); await work; await settle();
      const current = h.render(); assert.equal(current.notice, null); assert.equal(current.busy, false);
      assert.equal(h.writes.length, stage === "preflight" ? 0 : 1);
      // One new-scope query is allowed; the old operation cannot issue a readback.
      assert.equal(h.reads(), count + 1);
    } finally { h.dispose(); }
  });
}


test("a lost response stays unknown even when the requested status was applied", async () => {
  const h = host(); try { const view = await h.start(); h.setWrite(async () => {
    h.setRows([{ ...h.model(), is_enabled: false, updated_at: "applied" }]); throw new Error("response lost after apply");
  }); await view.toggle(h.model(), false); const current = h.render();
    assert.equal(current.notice, "unknown"); assert.equal(current.query.data?.[0].is_enabled, false);
    assert.equal(current.ready, false); assert.equal(h.writes.length, 1);
  } finally { h.dispose(); }
});

test("accepted write remains locked until delayed readback completes", async () => {
  const h = host(); const pending = deferred<Model[]>(); try { const view = await h.start(); const model = h.model();
    h.setWrite(async () => { h.setRead(() => pending.promise); return { ...model, is_enabled: false }; });
    const operation = view.toggle(model, false); await settle(); const waiting = h.render();
    assert.equal(waiting.ready, false); assert.equal(waiting.busy, true); assert.equal(waiting.notice, null);
    await waiting.toggle(model, false); assert.equal(h.writes.length, 1);
    pending.resolve([{ ...model, is_enabled: false }]); await operation; assert.equal(h.render().notice, "saved");
  } finally { h.dispose(); }
});
