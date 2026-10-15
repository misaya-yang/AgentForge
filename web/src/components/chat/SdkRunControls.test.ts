import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as runs from "../../features/chat/stream/sdkRun.ts";
import * as controls from "../../features/chat/stream/sdkRunController.ts";

const id = "12345678-1234-4234-8234-123456789abc";
const reference = { runId: id, engine: "openai_agents", sessionId: "session" };
const response = (status = "running") => ({ run: { run_id: id, engine: "openai_agents", session_id: "session", status } });
interface Tree { type: string | ((props: unknown) => Tree); props: Record<string, unknown> }
async function settle() { for (let i = 0; i < 24; i++) await Promise.resolve(); }
function buttons(tree: unknown): Record<string, unknown>[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(buttons);
  const node = tree as Tree;
  return node.type === "button" ? [node.props] : buttons(node.props?.children);
}
function strings(tree: unknown): string {
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (!tree || typeof tree !== "object") return "";
  if (Array.isArray(tree)) return tree.map(strings).join(" ");
  return strings((tree as Tree).props?.children);
}
function host() {
  const slots: unknown[] = []; let cursor = 0;
  const effects: (() => void)[] = []; const cleanups: (() => void)[] = [];
  const intervals = new Map<number, () => void>();
  const auth = { token: "test-only-token", user: { user_id: "owner" } };
  const calls: { kind: string; args: unknown[] }[] = [];
  let fetch: () => Promise<unknown> = async () => response();
  let cancel: () => Promise<unknown> = async () => ({ task_id: id, session_id: "session", cancelled: true });
  const scope = (user: unknown) => JSON.stringify(user);
  const react = {
    useState(initial: unknown) {
      const index = cursor++; if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
      return [slots[index], (value: unknown) => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useEffect(fn: () => (() => void) | void, deps: unknown[]) {
      const index = cursor++; const previous = slots[index] as unknown[] | undefined;
      if (!previous || deps.some((value, i) => value !== previous[i])) {
        slots[index] = deps; effects.push(() => { cleanups[index]?.(); cleanups[index] = fn() || (() => {}); });
      }
    },
  };
  const store = Object.assign((select: (state: typeof auth) => unknown) => select(auth), { getState: () => auth });
  const jsx = (type: unknown, props: unknown) => ({ type, props });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/api/assistant": {
      getAssistantRunStatus: (...args: unknown[]) => { calls.push({ kind: "read", args }); return fetch(); },
      cancelTask: (...args: unknown[]) => { calls.push({ kind: "cancel", args }); return cancel(); },
    },
    "@/features/chat/stream/sdkRun": runs, "@/features/chat/stream/sdkRunController": controls,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: scope }, "@/store/useAuthStore": { useAuthStore: store },
  };
  const exports: Record<string, (props: unknown) => Tree | null> = {};
  const source = readFileSync(new URL("./SdkRunControls.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; },
      window: { setInterval: (fn: () => void) => { const id = intervals.size + 1; intervals.set(id, fn); return id; }, clearInterval: (id: number) => intervals.delete(id) } });
  const props = { reference, authorityScope: scope(auth.user), interrupted: false };
  return { auth, calls, props, intervals, setFetch(fn: typeof fetch) { fetch = fn; }, setCancel(fn: typeof cancel) { cancel = fn; },
    render() { cursor = 0; const outer = exports.SdkRunControls(props); return outer && (outer.type as (p: unknown) => Tree)(outer.props); },
    async commit() { this.render(); effects.splice(0).forEach(fn => fn()); await settle(); const tree = this.render(); effects.splice(0).forEach(fn => fn()); return tree; },
    unmount() { cleanups.forEach(fn => fn?.()); },
  };
}

test("real card explicitly inspects via existing authenticated route and cancels once with deadlines", async () => {
  const h = host(); let tree = await h.commit(); assert.equal(h.calls.length, 0);
  (buttons(tree)[0].onClick as () => void)(); await settle(); tree = await h.commit();
  assert.equal(h.calls[0].kind, "read"); assert.equal(h.calls[0].args[0], id);
  assert.equal((h.calls[0].args[1] as { timeout: number }).timeout, 10_000);
  assert.equal((h.calls[0].args[1] as { signal: AbortSignal }).signal instanceof AbortSignal, true);
  assert.equal(strings(tree).includes("services.sdkAgent.run.usageUnavailable"), true);
  const cancel = buttons(tree)[1]; assert.equal(cancel.disabled, false);
  (cancel.onClick as () => void)(); (cancel.onClick as () => void)(); await settle();
  assert.equal(h.calls.filter(call => call.kind === "cancel").length, 1);
  assert.equal(h.calls.find(call => call.kind === "cancel")?.args[1], "client_interrupt");
  h.unmount();
});
test("real card owner guard suppresses previous account's data and actions", async () => {
  const h = host(); const tree = await h.commit(); h.auth.user = { user_id: "other" };
  (buttons(tree)[0].onClick as () => void)(); await settle();
  assert.equal(h.calls.length, 0); assert.equal(h.render(), null); h.unmount();
});
test("real card displays local interruption separately from server-completed status and stops polling", async () => {
  const h = host(); h.props.interrupted = true; h.setFetch(async () => response("completed"));
  const initial = await h.commit(); (buttons(initial)[0].onClick as () => void)(); await settle();
  const tree = await h.commit(); const text = strings(tree);
  assert.equal(text.includes("services.sdkAgent.run.interrupted"), true);
  assert.equal(text.includes("services.sdkAgent.run.status.completed"), true);
  assert.equal(buttons(tree).length, 1); assert.equal(h.intervals.size, 0); h.unmount();
});
test("real card does not allow a second POST after timeout and clears polling on unmount", async () => {
  const h = host(); let tree = await h.commit(); (buttons(tree)[0].onClick as () => void)(); await settle(); tree = await h.commit();
  assert.equal(h.intervals.size, 1);
  h.setCancel(async () => { throw new Error("lost response"); });
  (buttons(tree)[1].onClick as () => void)(); await settle(); tree = await h.commit();
  assert.equal(strings(tree).includes("services.sdkAgent.run.cancelUnknown"), true);
  assert.equal(buttons(tree)[1].disabled, true);
  (buttons(tree)[1].onClick as () => void)(); await settle();
  assert.equal(h.calls.filter(call => call.kind === "cancel").length, 1);
  h.unmount(); assert.equal(h.intervals.size, 0);
});
test("API wrappers preserve Runtime call signatures while encoding run identity and propagating cancellation", async () => {
  const source = readFileSync(new URL("../../api/assistant.ts", import.meta.url), "utf8");
  const calls: unknown[][] = []; const api = {
    get: async (...args: unknown[]) => { calls.push(args); return { data: response() }; },
    post: async (...args: unknown[]) => { calls.push(args); return { data: {} }; },
  };
  const exports: Record<string, (...args: unknown[]) => Promise<unknown>> = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
    { exports, require: (name: string) => name === "@/lib/api" ? { api } : {} });
  const options = { signal: new AbortController().signal, timeout: 10_000 };
  await exports.getAssistantRunStatus(id, options); await exports.cancelTask(id, "client_interrupt", options);
  assert.equal(calls[0][0], `/api/v1/assistant/runs/${id}`); assert.equal(calls[0][1], options);
  assert.equal(calls[1][0], `/api/v1/assistant/tasks/${id}/cancel`); assert.equal(calls[1][2], options);
  await exports.getAssistantRunStatus("part/other"); assert.equal(calls[2][0], "/api/v1/assistant/runs/part%2Fother");
  assert.equal(JSON.stringify(calls).includes('"/api/v1/tasks/'), false);
});

test("real card shows expired authority and denies cancellation without claiming a provider outcome", async () => {
  const h = host(); h.setFetch(async () => ({ run: { ...response().run, terminal_reason: "sdk_authority_expired" } }));
  const initial = await h.commit(); (buttons(initial)[0].onClick as () => void)(); await settle(); const tree = await h.commit();
  assert.equal(strings(tree).includes("services.sdkAgent.run.status.expired"), true);
  assert.equal(strings(tree).includes("services.sdkAgent.run.providerNotice"), true);
  assert.equal(buttons(tree).length, 1); h.unmount();
});
test("real card makes denied reads non-actionable and never renders raw response errors", async () => {
  const h = host(); h.setFetch(async () => { throw new Error("private error body"); });
  const initial = await h.commit(); (buttons(initial)[0].onClick as () => void)(); await settle(); const tree = await h.commit();
  assert.equal(strings(tree).includes("services.sdkAgent.run.unavailable"), true);
  assert.equal(strings(tree).includes("private error body"), false); assert.equal(buttons(tree).length, 1); h.unmount();
});
