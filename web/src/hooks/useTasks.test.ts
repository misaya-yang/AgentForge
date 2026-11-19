import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import test from "node:test";
import vm from "node:vm";

type QueryState = { state: { status: string; data?: { status: string } | { status: string }[] } };
type QueryOptions = {
  queryKey: unknown[];
  enabled?: boolean;
  queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
  refetchInterval?: number | false | ((query: QueryState) => number | false);
};
type Hooks = {
  useTasks: (status?: string, offset?: number, scope?: { id: string; isCurrent: () => boolean }) => QueryOptions;
  useTask: (taskId?: string, scope?: { id: string; isCurrent: () => boolean }) => QueryOptions;
  useTaskResult: (taskId?: string, scope?: { id: string; isCurrent: () => boolean }) => QueryOptions;
};
type Get = (path: string, config: { signal?: AbortSignal; params?: unknown }) => Promise<{ data: unknown }>;

// Execute the complete production TS modules with only their imports replaced.
// This is a query/transport-double contract test, not a React Query integration.
function load<T>(relative: string, imports: Record<string, unknown>, names: string[]): T {
  const source = stripTypeScriptTypes(readFileSync(new URL(relative, import.meta.url), "utf8"));
  const executable = source.replace(/^import .*;$/gm, "").replace(/^export /gm, "");
  const context = vm.createContext({ ...imports });
  return vm.runInContext(`${executable}\n;({ ${names.join(", ")} });`, context) as T;
}
function host(get: Get = async () => ({ data: {} }), userId: string | undefined = "user-a"): Hooks {
  const gateway = load<Record<string, unknown>>("../api/gateway.ts", { api: { get } }, ["getTask", "listTasks", "getTaskResult"]);
  return load<Hooks>("./useTasks.ts", { ...gateway, TASK_PAGE_SIZE: 50, useAuthStore: (selector: (state: unknown) => unknown) => selector({ token: userId ? "synthetic-token" : null, user: userId ? { user_id: userId } : null }), useQuery: (options: QueryOptions) => options }, ["useTasks", "useTask", "useTaskResult"]);
}
function interval(options: QueryOptions, data: QueryState["state"]["data"], status = "success") {
  return typeof options.refetchInterval === "function"
    ? options.refetchInterval({ state: { data, status } }) : options.refetchInterval;
}
function resultSelection(hooks: Hooks, searchedId: string | undefined, receipt?: { task_id: string; status: string }) {
  const source = readFileSync(new URL("../pages/tasks/components/TaskQueryTab.tsx", import.meta.url), "utf8");
  const start = source.indexOf("  const taskQuery = useTask(searchedId,");
  const end = source.indexOf("  const handleSearch =", start);
  assert.ok(start >= 0 && end > start, "task/result hook wiring must be present");
  // Execute the exact component query-selection fragment without JSX or React.
  return vm.runInNewContext(`${source.slice(start, end)}\nresultQuery;`, {
    searchedId, readScope: undefined, useTask: () => ({ data: receipt }), useTaskResult: hooks.useTaskResult,
  }) as QueryOptions;
}

test("task detail polls only pending and processing receipts", () => {
  const options = host().useTask("task-a");
  for (const status of ["pending", "processing"]) assert.equal(interval(options, { status }), 5_000);
  for (const status of ["completed", "failed", "cancelled", "unknown"]) assert.equal(interval(options, { status }), false);
  assert.equal(interval(options, undefined, "pending"), false);
});

test("exhausted read errors stop polling even when an active receipt is cached", () => {
  const options = host().useTask("task-a");
  assert.equal(interval(options, { status: "processing" }, "error"), false);
  assert.equal(interval(options, undefined, "error"), false);
  // An explicit successful retry of an active receipt re-enables polling.
  assert.equal(interval(options, { status: "processing" }), 5_000);
});

test("terminal transition removes repeated detail polling", () => {
  const options = host().useTask("task-a");
  const observed = ["pending", "processing", "completed", "completed", "completed"]
    .map((status) => interval(options, { status }));
  assert.deepEqual(observed, [5_000, 5_000, false, false, false]);
});

test("task inbox keeps its active and idle refresh policy", () => {
  const options = host().useTasks("processing");
  assert.equal(interval(options, [{ status: "pending" }]), 3_000);
  assert.equal(interval(options, [{ status: "processing" }]), 3_000);
  assert.equal(interval(options, [{ status: "completed" }]), 15_000);
  assert.equal(interval(options, []), 15_000);
});

test("empty task selection disables detail and result reads", () => {
  const hooks = host();
  assert.equal(hooks.useTask().enabled, false);
  assert.equal(hooks.useTaskResult().enabled, false);
  assert.equal(hooks.useTask("task-a").enabled, true);
});

test("result waits for the selected receipt to complete, then enables without another search", () => {
  const hooks = host();
  for (const status of ["pending", "processing", "failed", "cancelled", "unknown"]) {
    assert.equal(resultSelection(hooks, "task-a", { task_id: "task-a", status }).enabled, false, status);
  }
  const completed = resultSelection(hooks, "task-a", { task_id: "task-a", status: "completed" });
  assert.equal(completed.enabled, true);
  assert.deepEqual(Array.from(completed.queryKey), ["task-result", "user-a", "task-a"]);
});

test("previous task completion cannot enable a newer task result", () => {
  const hooks = host();
  assert.equal(resultSelection(hooks, "task-b", { task_id: "task-a", status: "completed" }).enabled, false);
  assert.equal(resultSelection(hooks, "task-b").enabled, false);
  assert.equal(resultSelection(hooks, undefined).enabled, false);
});

for (const kind of ["detail", "result", "list"] as const) {
  test(`${kind} query preserves URL, payload and query AbortSignal through the actual API helper`, async () => {
    const payload = { fixture: kind };
    const controller = new AbortController();
    const calls: { path: string; signal?: AbortSignal; params?: unknown }[] = [];
    const hooks = host(async (path, config) => {
      calls.push({ path, ...config });
      return { data: payload };
    });
    const options = kind === "detail" ? hooks.useTask("task-a")
      : kind === "result" ? hooks.useTaskResult("task-a") : hooks.useTasks("processing");
    assert.equal(await options.queryFn({ signal: controller.signal }), payload);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].signal, controller.signal);
    assert.equal(calls[0].path, kind === "list" ? "/api/v1/tasks"
      : `/api/v1/tasks/task-a${kind === "result" ? "/result" : ""}`);
    if (kind === "list") assert.equal(JSON.stringify(calls[0].params), JSON.stringify({ status: "processing", limit: 51, offset: 0 }));
  });

  test(`${kind} query abort rejects its obsolete transport without replay`, async () => {
    let calls = 0;
    const hooks = host((_path, { signal }) => {
      calls += 1;
      assert.ok(signal, "the transport must receive the query signal");
      return new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(new Error("fixture transport aborted")), { once: true });
      });
    });
    const options = kind === "detail" ? hooks.useTask("task-a")
      : kind === "result" ? hooks.useTaskResult("task-a") : hooks.useTasks();
    const controller = new AbortController();
    const request = options.queryFn({ signal: controller.signal });
    const rejected = assert.rejects(request, /fixture transport aborted/);
    controller.abort();
    await rejected;
    assert.equal(calls, 1);
  });
}

test("component keeps details during refresh and exposes result loading and retry", () => {
  const source = readFileSync(new URL("../pages/tasks/components/TaskQueryTab.tsx", import.meta.url), "utf8");
  assert.match(source, /const isLoading = taskQuery\.isLoading;/);
  assert.doesNotMatch(source, /const isLoading = .*taskQuery\.isFetching/);
  assert.match(source, /completedTaskId && resultQuery\.isLoading/);
  assert.match(source, /completedTaskId && resultQuery\.isError/);
  assert.match(source, /onClick=\{\(\) => void resultQuery\.refetch\(\)\}/);
  assert.match(source, /completedTaskId && !resultQuery\.isError && resultQuery\.data/);
  assert.match(source, /processing: \{\s+labelKey: "tasks.status.processing"/);
});


test("scoped task observations use an opaque visit key and reject retired pre-dispatch callbacks", async () => {
  let current = true; let reads = 0; const hooks = host(async () => { reads++; return { data: {} }; });
  const scope = { id: "opaque-visit", isCurrent: () => current }; const options = hooks.useTaskResult("task-a", scope);
  assert.deepEqual(Array.from(options.queryKey), ["task-result", "user-a", "task-a", "opaque-visit"]);
  current = false; await assert.rejects(options.queryFn({ signal: new AbortController().signal }), /retired/); assert.equal(reads, 0);
  assert.equal(hooks.useTask("task-a", scope).enabled, false);
});
test("scoped task detail and result discard a read that completes after authority retirement", async () => {
  for (const kind of ["useTask", "useTaskResult"] as const) {
    let current = true; let resolve!: (value: { data: unknown }) => void;
    const hooks = host(() => new Promise(done => { resolve = done; }));
    const pending = hooks[kind]("task-a", { id: "visit", isCurrent: () => current }).queryFn({ signal: new AbortController().signal });
    current = false; resolve({ data: { outputs: [] } }); await assert.rejects(pending, /retired/);
  }
});


test("scoped inbox reads retain list prefix, filter, pagination and AbortSignal", async () => {
  let observed: unknown; const rows = [{ task_id: "task-a", status: "pending" }];
  const hooks = host(async (_path, config) => { observed = config; return { data: rows }; });
  const scope = { id: "opaque-inbox-visit", isCurrent: () => true };
  const options = hooks.useTasks("failed", 50, scope); const signal = new AbortController().signal;
  assert.deepEqual(JSON.parse(JSON.stringify(options.queryKey)), ["tasks", "user-a", { status: "failed", offset: 50 }, "opaque-inbox-visit"]);
  assert.equal(await options.queryFn({ signal }), rows);
  assert.equal((observed as { signal: AbortSignal }).signal, signal);
  assert.deepEqual(JSON.parse(JSON.stringify((observed as { params: unknown }).params)), { status: "failed", limit: 51, offset: 50 });
  assert.equal(interval(options, [{ status: "pending" }]), 3_000);
  assert.equal(interval(options, []), 15_000);
});
test("scoped inbox refuses dispatch and hides enablement after authority retirement", async () => {
  let current = true; let calls = 0; const hooks = host(async () => { calls++; return { data: [] }; });
  const scope = { id: "inbox-visit", isCurrent: () => current }; const options = hooks.useTasks(undefined, 0, scope);
  current = false;
  await assert.rejects(options.queryFn({ signal: new AbortController().signal }), /retired/);
  assert.equal(calls, 0); assert.equal(hooks.useTasks(undefined, 0, scope).enabled, false);
});
test("scoped inbox rejects successful read completed after authority retirement", async () => {
  let current = true; let resolve!: (value: { data: unknown }) => void;
  const hooks = host(() => new Promise(done => { resolve = done; }));
  const pending = hooks.useTasks("processing", 50, { id: "visit", isCurrent: () => current }).queryFn({ signal: new AbortController().signal });
  current = false; resolve({ data: [{ task_id: "private-old-task" }] });
  await assert.rejects(pending, /retired/);
});
