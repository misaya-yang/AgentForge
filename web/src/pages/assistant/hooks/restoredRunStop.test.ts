import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Run = { sessionId: string; runId: string };
type Result = { observedRun: Run | null; restoredStopPending: boolean; setObservedRun: (run: Run | null) => void; setRestoredStopPending: (pending: boolean) => void; stopRestoredRun: () => void };
type Slot = { value: unknown; deps?: unknown[] };
const sessionSource = readFileSync(new URL("./useChatSession.ts", import.meta.url), "utf8");
const parsed = ts.createSourceFile("useChatSession.ts", sessionSource, ts.ScriptTarget.Latest, true);
const parent = parsed.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "useChatSession") as ts.FunctionDeclaration;
const declarations = parent.body!.statements.filter(ts.isVariableStatement).flatMap(node => [...node.declarationList.declarations]);
function callback(name: string) {
  const declaration = declarations.find(node => node.name.getText(parsed) === name)!;
  return (declaration.initializer as ts.CallExpression).arguments[0].getText(parsed);
}
function evaluate(code: string, context: Record<string, unknown>) {
  return vm.runInNewContext(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, context);
}
function host() {
  const slots: Slot[] = []; let cursor = 0;
  let sessionId: string | undefined = "session-a"; let error: string | null = null; let token = "a";
  const restoreEpochRef = { current: 0 }; const streamEpochRef = { current: 0 };
  const requests: { runId: string; resolve: (value: { cancelled: boolean }) => void; reject: (error: Error) => void }[] = [];
  const noop = () => {};
  const memo = (value: unknown, deps: unknown[]) => {
    const index = cursor++; const old = slots[index];
    if (!old || deps.some((dep, i) => !Object.is(dep, old.deps?.[i]))) slots[index] = { value, deps };
    return slots[index].value;
  };
  const react = {
    useRef: (value: unknown) => memo({ current: value }, []),
    useCallback: memo,
    useState(initial: unknown) {
      const index = cursor++; slots[index] ??= { value: initial };
      return [slots[index].value, (value: unknown) => { slots[index].value = value; }];
    },
  };
  const context: Record<string, unknown> = {
    restoreEpochRef, streamEpochRef, artifactRestoreOwner: { current: null },
    setArtifactRestorePending: noop, abortControllerRef: { current: null }, clearCancelFallback: noop,
    activeTaskIdRef: { current: null }, cancelApiTaskIdRef: { current: null }, sendInFlightRef: { current: false }, setIsStreaming: noop,
    ASSISTANT_VIEW_DETACH_REASON: "view-detach", pendingSessionIdRef: { current: undefined },
    setMessages: noop, setServerRunBlocking: noop, setModelRecreateNeeded: noop, setSourceRecreateNeeded: noop,
    setActiveSessionId: (id: string | undefined) => { sessionId = id; }, setHistoryRestoreState: noop,
    setHistoryRestoreError: (value: string | null) => { error = value; }, setArtifacts: noop, setArtifactLoadError: noop,
    setShowArtifacts: noop, setWorkingMemory: noop, setShowTaskPanel: noop, lastStreamConfigRef: { current: null }, setCodeExecution: noop,
  };
  const getVersion = evaluate(`(${callback("getRestoredRunVersion")})`, context) as () => string;
  context.retireArtifactRestore = evaluate(`(${callback("retireArtifactRestore")})`, context);
  context.abandonActiveStream = evaluate(`(${callback("abandonActiveStream")})`, context);
  const modules: Record<string, unknown> = {
    react,
    "@/api/assistant": { cancelTask: (runId: string) => new Promise((resolve, reject) => requests.push({ runId, resolve, reject })) },
    "@/store/useAppStore": { useAppStore: { getState: () => ({ assistantActiveSessionId: sessionId }) } },
  };
  const exports: Record<string, (getVersion: () => string, setError: (error: string | null) => void) => Result> = {};
  evaluate(readFileSync(new URL("./useRestoredRunStop.ts", import.meta.url), "utf8"), {
    exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; },
  });
  const setError = (value: string | null) => { error = value; };
  function render() {
    cursor = 0; const result = exports.useRestoredRunStop(getVersion, setError);
    context.setObservedRun = result.setObservedRun; context.setRestoredStopPending = result.setRestoredStopPending;
    return result;
  }
  function restore(run: Run) { sessionId = run.sessionId; render().setObservedRun(run); return render(); }
  return {
    render, restore, requests, error: () => error,
    newChat() { evaluate(`(${callback("handleNewChat")})()`, context); },
    switchTo(run: Run) {
      // Execute the production selection callback through its initial ownership changes;
      // stop at the first asynchronous read by supplying deliberately pending promises.
      Object.assign(context, { activeSessionId: sessionId, messages: [], historyRestoreState: "ready", getSession: () => new Promise(noop), getAssistantSessionHistory: () => new Promise(noop), getSessionArtifacts: () => new Promise(noop) });
      evaluate(`(${callback("handleSelectSession")})(${JSON.stringify(run.sessionId)})`, context);
      return restore(run);
    },
    authChange() {
      let listener: ((state: unknown, prior: unknown) => void) | undefined;
      const subscription = parent.body!.statements.find(node => ts.isExpressionStatement(node) && node.expression.getText(parsed).startsWith("useLayoutEffect(() => useAuthStore.subscribe")) as ts.ExpressionStatement;
      const effect = (subscription.expression as ts.CallExpression).arguments[0];
      evaluate(`(${effect.getText(parsed)})()`, { ...context, useAuthStore: { subscribe: (fn: typeof listener) => { listener = fn; } }, sdkAuthorityScope: JSON.stringify, t: (key: string) => key });
      const prior = token; token = token === "a" ? "b" : "a";
      listener!({ token, isAuthenticated: true, user: {} }, { token: prior, isAuthenticated: true, user: {} });
    },
    unmount() {
      const effect = parent.body!.statements.find(node => ts.isExpressionStatement(node) && node.expression.getText(parsed).includes("return () => {\n      streamEpochRef.current += 1;")) as ts.ExpressionStatement;
      const body = (effect.expression as ts.CallExpression).arguments[0];
      evaluate(`(${body.getText(parsed)})()()`, context);
    },
  };
}
const runA = { sessionId: "session-a", runId: "run-a" };
const flush = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };

test("current Stop is single-flight; accepted request stays pending until authoritative observation", async () => {
  const h = host(); const view = h.restore(runA); view.stopRestoredRun(); view.stopRestoredRun();
  assert.equal(h.requests.length, 1); assert.equal(h.requests[0].runId, "run-a"); assert.equal(h.render().restoredStopPending, true);
  h.requests[0].resolve({ cancelled: true }); await flush();
  assert.equal(h.render().restoredStopPending, true); assert.equal(h.error(), null);
  h.render().setObservedRun(null); assert.equal(h.render().restoredStopPending, false);
});
for (const reject of [false, true]) test(`current ${reject ? "transport failure" : "refused Stop"} permits explicit manual retry`, async () => {
  const h = host(); h.restore(runA).stopRestoredRun();
  if (reject) h.requests[0].reject(new Error("transport")); else h.requests[0].resolve({ cancelled: false });
  await flush(); assert.equal(h.render().restoredStopPending, false); assert.ok(h.error()); assert.equal(h.requests.length, 1);
  h.render().stopRestoredRun(); assert.equal(h.requests.length, 2);
});
for (const reject of [false, true]) test(`session A to B to A fences old ${reject ? "rejection" : "response"} and stale callbacks`, async () => {
  const h = host(); const old = h.restore(runA); old.stopRestoredRun();
  h.switchTo({ sessionId: "session-b", runId: "run-b" }); old.stopRestoredRun();
  const current = h.switchTo(runA); current.stopRestoredRun(); assert.equal(h.requests.length, 2);
  if (reject) h.requests[0].reject(new Error("old")); else h.requests[0].resolve({ cancelled: false });
  await flush(); assert.equal(h.error(), null); assert.equal(h.render().restoredStopPending, true);
});
test("same-session replacement fences an old callback before dispatch and settlement", async () => {
  const h = host(); const old = h.restore(runA); old.stopRestoredRun();
  const next = h.restore({ ...runA, runId: "run-next" }); old.stopRestoredRun(); next.stopRestoredRun();
  h.requests[0].reject(new Error("old")); await flush();
  assert.equal(h.requests.length, 2); assert.equal(h.requests[1].runId, "run-next"); assert.equal(h.error(), null); assert.equal(h.render().restoredStopPending, true);
});
for (const transition of ["authChange", "newChat", "unmount"] as const) test(`${transition} retires Stop before dispatch and settlement using production lifecycle code`, async () => {
  const h = host(); const old = h.restore(runA); old.stopRestoredRun(); h[transition]();
  if (transition === "authChange") h.authChange(); // Returning to the same identity cannot revive the old version.
  old.stopRestoredRun(); h.requests[0].reject(new Error("retired")); await flush();
  assert.equal(h.requests.length, 1); assert.equal(h.error(), null);
  if (transition !== "unmount") { h.restore(runA).stopRestoredRun(); assert.equal(h.requests.length, 2); }
});

test("auth ABA blocks a retained Stop callback before any request was dispatched", () => {
  const h = host(); const old = h.restore(runA); h.authChange(); h.authChange(); old.stopRestoredRun();
  h.render().stopRestoredRun(); assert.equal(h.requests.length, 0);
  h.restore(runA).stopRestoredRun(); assert.equal(h.requests.length, 1);
});
