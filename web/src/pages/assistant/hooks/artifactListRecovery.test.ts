import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

type Tree = { type: unknown; props: Record<string, unknown> };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void };
const noop = () => {};
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const node = value as Tree; return [node, ...nodes(node.props?.children)]; }
function host() {
  let auth = { token: "a", isAuthenticated: true, user: { user_id: "a", permissions: [] as string[], roles: ["member"], tier: "normal" } }; let app = { assistantActiveSessionId: "session-a" };
  const authListeners = new Set<(state: typeof auth, prior: typeof auth) => void>(); const appListeners = new Set<(state: typeof app, prior: typeof app) => void>();
  let version = 0; let viewEpoch = 1; let open = true; let eligible = true; let items: unknown[] = [{ artifact_id: "known" }]; let failed = true;
  const hookSlots: Slot[] = []; let slots = hookSlots; let cursor = 0; let effects: Array<() => void> = [];
  const requests: Array<{ path: string; signal: AbortSignal; resolve: (value: unknown) => void; reject: (value: unknown) => void }> = [];
  const memo = (fn: () => unknown, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) slots[i] = { value: fn(), deps }; return slots[i].value; };
  const effect = (fn: () => (() => void) | undefined, deps: unknown[]) => { const i = cursor++; const prior = slots[i]; if (!prior || deps.some((d, j) => !Object.is(d, prior.deps?.[j]))) { const next: Slot = { deps }; slots[i] = next; effects.push(() => { prior?.cleanup?.(); next.cleanup = fn(); }); } };
  const react = { useState(initial: unknown) { const i = cursor++; const owner = slots; owner[i] ??= { value: typeof initial === "function" ? initial() : initial }; return [owner[i].value, (next: unknown) => { owner[i].value = typeof next === "function" ? next(owner[i].value) : next; }]; }, useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useRef: (value: unknown) => memo(() => ({ current: value }), []), useEffect: effect, useLayoutEffect: effect };
  const jsx = (type: unknown, props: Record<string, unknown>) => ({ type, props }); const named = new Proxy({}, { get: (_, name) => String(name) });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string, fallback?: string) => fallback || key }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((selector: (state: typeof auth) => unknown) => selector(auth), { getState: () => auth, subscribe: (fn: (state: typeof auth, prior: typeof auth) => void) => { authListeners.add(fn); return () => authListeners.delete(fn); } }) },
    "@/store/useAppStore": { useAppStore: Object.assign(() => app, { getState: () => app, subscribe: (fn: (state: typeof app, prior: typeof app) => void) => { appListeners.add(fn); return () => appListeners.delete(fn); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    "@/lib/api": { api: { get: (path: string, options: { signal: AbortSignal }) => new Promise((resolve, reject) => requests.push({ path, signal: options.signal, resolve: value => resolve({ data: { artifacts: value } }), reject })) } },
    "@/pages/assistant/sse-events": { SSEEventType: {} }, "motion/react": { motion: named, AnimatePresence: "AnimatePresence" }, "lucide-react": named,
    "@/lib/utils": { cn: (...values: unknown[]) => values.filter(Boolean).join(" ") }, "@/hooks/use-toast": { toast: { error: noop } },
    "@/lib/format": { formatFileSize: () => "1 KB", getFormatLabel: () => "file" }, "@/lib/clipboard": { copyToClipboard: noop },
    "@/lib/authenticatedDownload": { artifactAccessErrorKey: () => "error", downloadAssistantArtifact: noop, openAssistantArtifact: noop },
  };
  function compile(path: string) { const exports: Record<string, (props: unknown) => unknown> = {}; vm.runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8").replaceAll("import.meta.env", "({})"), { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText, { exports, AbortController, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } }); return exports; }
  modules["@/api/assistant"] = compile("../../../api/assistant.ts"); const hook = compile("./useArtifactListRecovery.ts").useArtifactListRecovery; const drawer = compile("../../../components/artifacts/ArtifactsPanel.tsx").ArtifactsPanel;
  const getVersion = () => String(version); const isViewCurrent = (epoch: number) => open && epoch === viewEpoch; const apply = (next: unknown[]) => { items = next; version++; }; const setLoadError = (value: boolean) => { failed = value; };
  type Result = { refreshArtifacts: () => Promise<void>; artifactRefreshStatus: string; canRefreshArtifacts: boolean };
  return { requests,
    render() { slots = hookSlots; cursor = 0; const result = hook({ sessionId: app.assistantActiveSessionId, open, viewEpoch, isViewCurrent, eligible, getVersion, apply, setLoadError }) as Result; const pending = effects; effects = []; pending.forEach(fn => fn()); return result; },
    drawer(props = {}) { slots = []; cursor = 0; const result = drawer({ isOpen: true, onClose: noop, artifacts: [], executionStatus: "idle", executionOutput: "", onRefresh: noop, ...props }); effects = []; return result; },
    items: () => items, failed: () => failed,
    updateArtifacts(next: unknown[]) { items = next; version++; }, beginStream() { version++; eligible = false; }, setEligible(value: boolean) { eligible = value; },
    view(value: boolean) { if (open !== value) { open = value; viewEpoch++; } },
    session(id: string) { const prior = app; app = { assistantActiveSessionId: id }; [...appListeners].forEach(fn => fn(app, prior)); },
    auth(patch: Partial<typeof auth>) { const prior = auth; auth = { ...auth, ...patch }; [...authListeners].forEach(fn => fn(auth, prior)); },
    unmount() { hookSlots.forEach(slot => slot.cleanup?.()); },
  };
}
test("read-only retry replaces a failed list with returned artifacts and suppresses duplicate clicks", async () => {
  const h = host(); const first = h.render(); const pending = first.refreshArtifacts(); await first.refreshArtifacts(); assert.equal(h.requests.length, 1); assert.equal(h.requests[0].path, "/api/v1/assistant/sessions/session-a/artifacts"); assert.equal(h.render().artifactRefreshStatus, "loading");
  h.requests[0].resolve([{ artifact_id: "saved" }]); await pending; assert.equal(h.failed(), false); assert.deepEqual(h.items(), [{ artifact_id: "saved" }]); assert.equal(h.render().artifactRefreshStatus, "ready");
});
test("transient error retains known files; a confirmed owned empty snapshot removes them", async () => {
  const h = host(); const pending = h.render().refreshArtifacts(); h.requests[0].reject({ response: { status: 503 } }); await pending; assert.deepEqual(h.items(), [{ artifact_id: "known" }]); assert.equal(h.render().artifactRefreshStatus, "error"); const retry = h.render().refreshArtifacts(); h.requests[1].resolve([]); await retry; assert.equal(h.items().length, 0); assert.equal(h.failed(), false);
});
test("denied/inaccessible reads clear retained list without exposing response bodies", async () => {
  for (const status of [403, 404]) { const h = host(); const pending = h.render().refreshArtifacts(); h.requests[0].reject({ response: { status, data: "private detail" } }); await pending; assert.equal(h.items().length, 0); assert.equal(h.render().artifactRefreshStatus, "unavailable"); }
});
test("newer streamed artifacts supersede a saved snapshot and require explicit refresh", async () => {
  const h = host(); const pending = h.render().refreshArtifacts(); h.updateArtifacts([{ artifact_id: "new-stream" }]); h.requests[0].resolve([]); await pending; assert.deepEqual(h.items(), [{ artifact_id: "new-stream" }]); assert.equal(h.render().artifactRefreshStatus, "superseded"); assert.equal(h.requests.length, 1);
});
test("stream admission and unfinished restore block new reads and invalidate older results", async () => {
  const h = host(); h.setEligible(false); await h.render().refreshArtifacts(); assert.equal(h.requests.length, 0); h.setEligible(true); const pending = h.render().refreshArtifacts(); h.beginStream(); h.requests[0].resolve([]); await pending; assert.equal(h.items().length, 1); assert.equal(h.render().canRefreshArtifacts, false);
});
test("close/reopen and session A-B-A cannot revive retained callbacks or pending responses", async () => {
  const h = host(); const first = h.render(); const pending = first.refreshArtifacts(); h.view(false); h.view(true); await first.refreshArtifacts(); h.requests[0].resolve([]); await pending; assert.equal(h.items().length, 1); h.render();
  const second = h.render(); const next = second.refreshArtifacts(); h.session("session-b"); h.session("session-a"); await second.refreshArtifacts(); assert.equal(h.requests[1].signal.aborted, true); h.requests[1].resolve([]); await next; assert.equal(h.items().length, 1); assert.equal(h.requests.length, 2);
});
test("credential A-B-A and unmount invalidate all older file reads", async () => {
  const h = host(); const first = h.render(); const pending = first.refreshArtifacts(); h.auth({ token: "b" }); h.auth({ token: "a" }); await first.refreshArtifacts(); h.requests[0].resolve([]); await pending; assert.equal(h.items().length, 1); const next = h.render(); h.unmount(); await next.refreshArtifacts(); assert.equal(h.requests.length, 1);
});
test("actual drawer exposes read-only retry/loading/wait and superseded states without rerun", () => {
  const h = host(); const retry = nodes(h.drawer({ loadError: true })).find(node => node.type === "button" && node.props.children === "assistant.artifactRecovery.retry"); assert.ok(retry); assert.equal(retry.props.disabled, false);
  const loading = nodes(h.drawer({ refreshStatus: "loading" })).find(node => node.type === "button" && node.props.children === "assistant.artifactRecovery.loading"); assert.equal(loading?.props.disabled, true);
  assert.ok(nodes(h.drawer({ refreshStatus: "superseded" })).find(node => node.props.role === "status")); assert.ok(nodes(h.drawer({ refreshDisabled: true })).find(node => node.props.children === "assistant.artifactRecovery.wait"));
});
test("production setArtifacts and drawer setters synchronously advance ownership before React commits", () => {
  const source = readFileSync(new URL("./useChatSession.ts", import.meta.url), "utf8"); const file = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest, true); const fn = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "useChatSession") as ts.FunctionDeclaration;
  const body = fn.body!.statements.filter(ts.isVariableStatement).flatMap(node => [...node.declarationList.declarations]);
  const artifact = body.find(node => node.name.getText(file) === "setArtifacts")!; const drawer = body.find(node => node.name.getText(file) === "setShowArtifacts")!;
  const version = { current: 0 }; const view = { current: { open: false, epoch: 0 } }; let writes = 0;
  for (const [declaration, argument] of [[artifact, "[]"], [drawer, "true"], [drawer, "false"]] as const) {
    const callback = (declaration.initializer as ts.CallExpression).arguments[0]; const code = ts.transpileModule(`(${callback.getText(file)})(${argument})`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
    vm.runInNewContext(code, { artifactVersionRef: version, setArtifactsState: () => { writes++; }, artifactDrawerRef: view, setArtifactDrawer: noop });
  }
  assert.equal(version.current, 1); assert.equal(writes, 1); assert.equal(view.current.epoch, 2); assert.equal(view.current.open, false);
});
test("abandoned restore retires immediately and its late finally cannot block or clear a new visit", async () => {
  const source = readFileSync(new URL("./useChatSession.ts", import.meta.url), "utf8");
  const file = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest, true);
  const fn = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "useChatSession") as ts.FunctionDeclaration;
  const declarations = fn.body!.statements.filter(ts.isVariableStatement).flatMap(node => [...node.declarationList.declarations]);
  const retire = declarations.find(node => node.name.getText(file) === "retireArtifactRestore")!;
  const callback = (retire.initializer as ts.CallExpression).arguments[0];
  const epoch = { current: 1 }; const owner: { current: number | null } = { current: 1 }; let pending = true;
  const retireNow = () => vm.runInNewContext(ts.transpileModule(`(${callback.getText(file)})()`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, { restoreEpochRef: epoch, artifactRestoreOwner: owner, setArtifactRestorePending: (value: boolean) => { pending = value; } });
  const oldOwner = owner.current; // The old network request is deliberately still unresolved.
  retireNow(); assert.equal(epoch.current, 2); assert.equal(owner.current, null); assert.equal(pending, false);
  const h = host(); h.session("new-session"); h.setEligible(!pending); const refresh = h.render().refreshArtifacts(); assert.equal(h.requests.length, 1);
  owner.current = 3; pending = true;
  // Execute the production exact-owner finally check after a newer restore owns the visit.
  const finallyBlock = source.match(/if \(artifactRestoreOwner.current === restoreEpoch\) \{ artifactRestoreOwner.current = null; setArtifactRestorePending\(false\); \}/)![0];
  vm.runInNewContext(finallyBlock, { artifactRestoreOwner: owner, restoreEpoch: oldOwner, setArtifactRestorePending: (value: boolean) => { pending = value; } });
  assert.equal(owner.current, 3); assert.equal(pending, true);
  h.requests[0].resolve([]); await refresh;
  const newChat = declarations.find(node => node.name.getText(file) === "handleNewChat")!.getText(file);
  assert.ok(newChat.includes("retireArtifactRestore()"));
  assert.ok(source.includes("useAuthStore.subscribe((state, prior) =>"));
  assert.ok(source.includes('retireArtifactRestore();\n    setHistoryRestoreState("idle")'));
});
test("production auth subscription retires an unresolved restore into a truthful retryable state", () => {
  const source = readFileSync(new URL("./useChatSession.ts", import.meta.url), "utf8");
  const file = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest, true);
  const fn = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "useChatSession") as ts.FunctionDeclaration;
  const subscription = fn.body!.statements.find(node => ts.isExpressionStatement(node) && node.getText(file).startsWith("useLayoutEffect("))!;
  let listener: ((state: Record<string, unknown>, prior: Record<string, unknown>) => void) | undefined;
  const owner: { current: number | null } = { current: 7 }; let history = "loading"; let error: string | null = null; let pending = true; let epoch = 7;
  vm.runInNewContext(ts.transpileModule(subscription.getText(file), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, {
    useLayoutEffect: (effect: () => unknown) => effect(), useAuthStore: { subscribe: (fn: typeof listener) => { listener = fn; return noop; } },
    sdkAuthorityScope: (user: unknown) => JSON.stringify(user), artifactRestoreOwner: owner,
    retireArtifactRestore: () => { epoch++; owner.current = null; pending = false; },
    setHistoryRestoreState: (state: string) => { history = state; }, setHistoryRestoreError: (value: string) => { error = value; }, t: (key: string) => key,
  });
  const prior = { token: "a", isAuthenticated: true, user: { id: "same-user" } };
  listener!({ ...prior, token: "b" }, prior);
  assert.equal(epoch, 8); assert.equal(owner.current, null); assert.equal(pending, false); assert.equal(history, "failed"); assert.equal(error, "assistant.artifactRecovery.restoreInterrupted");
  // The superseded restore cannot publish ready state, and a new visit can explicitly retry.
  assert.notEqual(epoch, 7);
  history = "ready"; error = null;
  listener!({ ...prior, token: "c" }, prior);
  assert.equal(history, "ready"); assert.equal(error, null);
});
