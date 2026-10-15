import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { sdkAuthorityScope } from "../../lib/sdkAuthorityScope.ts";

type Modal = { onOk: () => Promise<void>; onCancel: () => void; afterClose: () => void };
type Actions = { pending: boolean; notice: { text: string; warning: boolean } | null; cancel: () => void; retry: (ids: string[]) => void };
function deferred() {
  let resolve!: (value: unknown) => void; let reject!: (reason: unknown) => void;
  const promise = new Promise<unknown>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function host() {
  const slots: unknown[] = []; const cleanups: Array<() => void> = [];
  let cursor = 0; let effects: Array<() => void> = []; let sequence = 0;
  let auth = { token: "account-a", isAuthenticated: true, user: { user_id: "a", permissions: ["console:eval:run"], roles: ["member"], tier: "normal" } };
  const subscribers = new Set<(state: typeof auth, previous: typeof auth) => void>();
  const react = {
    useState(initial: unknown) { const i = cursor++; if (!(i in slots)) slots[i] = initial; return [slots[i], (value: unknown) => { slots[i] = typeof value === "function" ? value(slots[i]) : value; }]; },
    useRef(initial: unknown) { const i = cursor++; return slots[i] ?? (slots[i] = { current: initial }); },
    useMemo(make: () => unknown, deps: unknown[]) { const i = cursor++; const old = slots[i] as { value: unknown; deps: unknown[] } | undefined; if (!old || deps.some((value, n) => !Object.is(value, old.deps[n]))) slots[i] = { value: make(), deps }; return (slots[i] as { value: unknown }).value; },
    useLayoutEffect(effect: () => void | (() => void), deps: unknown[]) { const i = cursor++; const old = slots[i] as unknown[] | undefined; if (!old || deps.some((value, n) => !Object.is(value, old[n]))) { slots[i] = deps; effects.push(() => { cleanups[i]?.(); cleanups[i] = effect() || (() => {}); }); } },
  };
  Object.assign(react, { useCallback: (fn: unknown, deps: unknown[]) => react.useMemo(() => fn, deps) });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const dialogs: Modal[] = []; const writes: unknown[][] = []; const navigations: string[] = []; const reads: unknown[][] = [];
  const storage = new Map<string, string>();
  let write: (...args: unknown[]) => Promise<unknown> = async () => ({ run_id: "run-a", status: "cancelled", runtime_interrupt_pending: 0 });
  let destroyed = 0;
  const options = { runId: "run-a", experimentId: "experiment-a", visit: {}, allowed: true, onRetryQueued: (id: string) => navigations.push(id) };
  const modules: Record<string, unknown> = {
    react,
    antd: { App: { useApp: () => ({ modal: { confirm: (modal: Modal) => { dialogs.push(modal); return { destroy: () => { destroyed++; modal.afterClose(); } }; } } }) } },
    "@tanstack/react-query": { useQueryClient: () => ({ invalidateQueries: (...args: Parameters<QueryClient["invalidateQueries"]>) => { reads.push(args); return queryClient.invalidateQueries(...args); } }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, previous: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/utils": { generateUUID: () => `opaque-${++sequence}` },
    "@/api/eval": Object.fromEntries(["cancelEvalExperimentRun", "retryFailedEvalExperimentCases"].map(name => [name, (...args: unknown[]) => { writes.push([name, ...args]); return write(...args); }])),
  };
  for (const [name, path] of [["@/hooks/useAuthenticatedReadScope", "../../hooks/useAuthenticatedReadScope.ts"], ["actions", "./useEvalRunActions.ts"]]) {
    const exported = {};
    vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
      exports: exported, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; },
      sessionStorage: { getItem: (key: string) => storage.get(key), setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) },
    });
    modules[name] = exported;
  }
  return {
    options, dialogs, writes, navigations, reads, queryClient, storage,
    get destroyed() { return destroyed; },
    render() { cursor = 0; effects = []; const result = (modules.actions as { useEvalRunActions: (input: typeof options) => Actions }).useEvalRunActions(options); effects.forEach(fn => fn()); return result; },
    changeAuth(token: string) { const previous = auth; auth = { ...auth, token }; subscribers.forEach(fn => fn(auth, previous)); },
    setWrite(fn: typeof write) { write = fn; },
    unmount() { cleanups.forEach(fn => fn()); },
  };
}
for (const status of ["cancelled", "succeeded", "failed"]) {
  test(`cancel preserves returned ${status} fact`, async () => {
    const h = host(); h.setWrite(async () => ({ run_id: "run-a", status, runtime_interrupt_pending: 0 }));
    h.render().cancel(); await h.dialogs[0].onOk();
    const text = h.render().notice!.text;
    if (status === "cancelled") assert.match(text, /Run cancelled/); else { assert.match(text, new RegExp(`status: ${status}`)); assert.doesNotMatch(text, /^Run cancelled/); }
    assert.equal(h.writes.length, 1); assert.equal(h.reads.length, 3);
  });
}
test("cancel pending interrupts and malformed responses never imply stopped effects", async () => {
  for (const response of [{ run_id: "run-a", status: "cancelled", runtime_interrupt_pending: 2 }, { run_id: "wrong", status: "cancelled", runtime_interrupt_pending: 0 }, { run_id: "run-a", status: "cancelled" }]) {
    const h = host(); h.setWrite(async () => response); h.render().cancel(); await h.dialogs[0].onOk(); assert.equal(h.render().notice!.warning, true); assert.match(h.render().notice!.text, /unconfirmed|could not be fully refreshed/);
  }
});
for (const change of ["run", "experiment", "visit", "permission", "auth", "aba", "unmount"]) {
  for (const submitted of [false, true]) {
    test(`${change} invalidates ${submitted ? "in-flight" : "unsubmitted"} action`, async () => {
      const h = host(); const response = deferred(); h.setWrite(() => response.promise); h.render().retry(["case-a"]);
      const completion = submitted ? h.dialogs[0].onOk() : undefined;
      if (change === "run") h.options.runId = "run-b";
      if (change === "experiment") h.options.experimentId = "experiment-b";
      if (change === "visit") h.options.visit = {};
      if (change === "permission") h.options.allowed = false;
      if (change === "auth" || change === "aba") { h.changeAuth("account-b"); if (change === "aba") h.changeAuth("account-a"); }
      if (change === "unmount") h.unmount(); else h.render();
      if (!submitted) await h.dialogs[0].onOk();
      response.resolve({ jobs: [{ run_id: "retry-a" }] }); await completion;
      assert.equal(h.writes.length, submitted ? 1 : 0); assert.equal(h.navigations.length, 0); assert.equal(h.reads.length, 0);
    });
  }
}
test("confirmation and submitted operations remain single-flight and copy case selection", async () => {
  const h = host(); const response = deferred(); h.setWrite(() => response.promise); const ids = ["b", "a"];
  h.render().retry(ids); ids.push("later"); h.render().cancel(); assert.equal(h.dialogs.length, 1);
  const first = h.dialogs[0].onOk(); await h.dialogs[0].onOk(); h.dialogs[0].onCancel(); h.render().retry(["other"]);
  assert.equal(h.dialogs.length, 1);
  assert.equal(h.writes.length, 1); assert.deepEqual(Array.from(h.writes[0][2] as string[]), ["a", "b"]);
  response.resolve({ jobs: [{ run_id: "retry-a" }] }); await first;
  assert.deepEqual(h.navigations, ["retry-a"]); assert.equal(h.render().pending, false);
});
test("uncertain retry retains one idempotency key across explicit same-selection confirmation", async () => {
  const h = host(); h.setWrite(async () => { throw new Error("private transport detail"); });
  h.render().retry(["b", "a"]); await h.dialogs[0].onOk(); assert.match(h.render().notice!.text, /outcome is unconfirmed/); assert.doesNotMatch(h.render().notice!.text, /private/);
  h.setWrite(async () => ({ jobs: [{ run_id: "retry-a" }] })); h.render().retry(["a", "b"]); await h.dialogs[1].onOk();
  assert.equal(h.writes[0][4], h.writes[1][4]); assert.equal(h.storage.size, 0);
});
test("real QueryClient readback failure remains explicit and keeps retry identity", async () => {
  const h = host(); let fail = false;
  const observer = new QueryObserver(h.queryClient, { queryKey: ["eval", "run", "run-a", "scope"], queryFn: async () => { if (fail) throw new Error("readback failed"); return {}; }, retry: false });
  const unsubscribe = observer.subscribe(() => {}); await observer.refetch(); fail = true;
  h.setWrite(async () => ({ jobs: [{ run_id: "retry-a" }] })); h.render().retry(["a"]); await h.dialogs[0].onOk();
  assert.match(h.render().notice!.text, /server accepted/); assert.equal(h.storage.size, 1); assert.equal(h.navigations.length, 0);
  unsubscribe();
});
test("abandoned confirmation is destroyed and does not block the next visit", () => {
  const h = host(); h.render().cancel(); h.options.visit = {}; h.render(); h.render().cancel();
  assert.equal(h.destroyed, 1); assert.equal(h.dialogs.length, 2);
});
