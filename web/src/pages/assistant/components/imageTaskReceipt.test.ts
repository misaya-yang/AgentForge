import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { sdkAuthorityScope } from "../../../lib/sdkAuthorityScope.ts";

type Tree = { type: unknown; key?: string; props: Record<string, unknown> };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void; setup?: () => void | (() => void) };
type Lane = { slots: Slot[]; key?: string };
type Request = { args: unknown[]; resolve: (value: unknown) => void; reject: (error: unknown) => void };
const taskId = "imt_aaaaaaaaaaaaaaaaaaaa";
const turnId = "itn_aaaaaaaaaaaaaaaa";
const receipt = (changes: Record<string, unknown> = {}) => ({ task_id: taskId, session_id: "session-a", turn_id: turnId, status: "completed", progress: 100, effective_model_id: "observed-model", ...changes });
const message = () => ({ id: "message-a", role: "assistant", content: "Original uncertain outcome", status: "failed", outcomeUncertain: true, diagnosticId: taskId, imageTurnId: turnId, sourceAccessRevoked: false });
function nodes(value: unknown): Tree[] { if (Array.isArray(value)) return value.flatMap(nodes); if (!value || typeof value !== "object") return []; const n = value as Tree; return n.props ? [n, ...nodes(n.props.children)] : []; }
function text(value: unknown): string { if (typeof value === "string" || typeof value === "number") return String(value); if (Array.isArray(value)) return value.map(text).join(" "); return value && typeof value === "object" ? text((value as Tree).props?.children) : ""; }
async function settle() { for (let n = 0; n < 40; n++) await Promise.resolve(); }
function host() {
  const parent: Lane = { slots: [] }; let child: Lane | null = null; let lane = parent; let cursor = 0; let effects: Array<() => void> = [];
  const props = { message: message(), sessionId: "session-a" };
  let auth = { token: "token-a", isAuthenticated: true, user: { user_id: "owner-a", permissions: [], roles: ["member"], tier: "standard" } };
  const subscribers = new Set<(a: typeof auth, b: typeof auth) => void>();
  const requests: Request[] = []; let posts = 0; let id = 0;
  const same = (a?: unknown[], b?: unknown[]) => Boolean(a && b && a.length === b.length && a.every((v, n) => Object.is(v, b[n])));
  function memo(make: () => unknown, deps: unknown[]) { const n = cursor++; if (!same(lane.slots[n]?.deps, deps)) lane.slots[n] = { value: make(), deps }; return lane.slots[n].value; }
  function effect(fn: () => void | (() => void), deps: unknown[]) { const n = cursor++; if (!same(lane.slots[n]?.deps, deps)) { const old = lane.slots[n]; const slot: Slot = { deps, setup: fn }; lane.slots[n] = slot; effects.push(() => { old?.cleanup?.(); slot.cleanup = fn() || undefined; }); } }
  let lazyCount = 0;
  const react = { memo: (fn: unknown) => fn, lazy: () => ["approval", "stream-output", "preview", imageComponent][lazyCount++], Suspense: "suspense",
    useMemo: memo, useCallback: (fn: unknown, deps: unknown[]) => memo(() => fn, deps), useEffect: effect, useLayoutEffect: effect,
    useRef(initial: unknown) { const n = cursor++; return (lane.slots[n] ?? (lane.slots[n] = { value: { current: initial } })).value; },
    useState(initial: unknown) { const n = cursor++; const slot = lane.slots[n] ?? (lane.slots[n] = { value: typeof initial === "function" ? initial() : initial }); return [slot.value, (v: unknown) => { slot.value = typeof v === "function" ? v(slot.value) : v; }]; } };
  const jsx = (type: unknown, props: Record<string, unknown>, key?: string) => ({ type, props, key });
  const modules: Record<string, unknown> = { react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/store/useAuthStore": { useAuthStore: Object.assign((select: (s: typeof auth) => unknown) => select(auth), { getState: () => auth, subscribe: (fn: (a: typeof auth, b: typeof auth) => void) => { subscribers.add(fn); return () => subscribers.delete(fn); } }) },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope }, "@/lib/utils": { generateUUID: () => `scope-${++id}`, cn: () => "" },
    "@/lib/api": { api: { get: (...args: unknown[]) => new Promise((resolve, reject) => requests.push({ args, resolve, reject })), post: () => { posts++; throw Error("Unexpected POST"); } } },
    "@/pages/assistant/sse-events": {}, "framer-motion": { motion: { div: "motion-div" } }, "lucide-react": {}, "../thinkingPreview": {}, "./ContextDisplay": {}, "./CitationDisplay": {}, "./Quiz": {}, "./ActivityPill": {},
    "./rightPanelContext": { useRightPanel: () => ({}) }, "@/features/chat/messageRenderPerformance": { messageContainmentStyle: () => ({}) },
    "@/lib/authenticatedDownload": {}, "@/hooks/use-toast": { toast: {} } };
  function compile(relative: string) {
    const exports: Record<string, (props: unknown) => Tree> = {};
    const source = readFileSync(new URL(relative, import.meta.url), "utf8").replace("import.meta.env.VITE_ASSISTANT_UI_V2", '"true"');
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText,
      { exports, require: (name: string) => { assert.ok(name in modules, name); return modules[name]; }, AbortController, Date, window: { setInterval: () => 1, clearInterval: () => {} } });
    return exports;
  }
  modules["@/api/assistant"] = compile("../../../api/assistant.ts");
  modules["@/hooks/useAuthenticatedReadScope"] = compile("../../../hooks/useAuthenticatedReadScope.ts");
  modules["../assistantOutcome"] = compile("../assistantOutcome.ts");
  modules["./activityTerminalState"] = compile("./activityTerminalState.ts");
  const imageComponent = compile("./ImageTaskReceipt.tsx").default;
  const chat = compile("./ChatMessage.tsx").ChatMessage;
  const clean = (target: Lane) => target.slots.forEach(s => s.cleanup?.());
  function render() {
    lane = parent; cursor = 0; effects = []; let used = false;
    const tree = chat(props);
    function expand(value: unknown): unknown {
      if (Array.isArray(value)) return value.map(expand); if (!value || typeof value !== "object") return value; const n = value as Tree; if (!n.props) return value;
      if (n.type === imageComponent) { used = true; if (!child || child.key !== n.key) { if (child) clean(child); child = { slots: [], key: n.key }; } lane = child; cursor = 0; return imageComponent(n.props); }
      return { ...n, props: { ...n.props, children: expand(n.props.children) } };
    }
    const result = expand(tree); if (!used && child) { clean(child); child = null; } effects.forEach(fn => fn()); return result;
  }
  function check() { const button = nodes(render()).find(n => n.type === "button" && /imageTaskReceipt\.(check|checking)/.test(text(n))); assert.ok(button, "unknown image must expose an explicit task receipt check"); return button; }
  return { props, requests, render, check, text: () => text(render()), posts: () => posts,
    click() { const b = check(); assert.notEqual(b.props.disabled, true); (b.props.onClick as () => void)(); },
    changeAuth(token: string) { const old = auth; auth = { ...auth, token }; subscribers.forEach(fn => fn(auth, old)); },
    unmount() { clean(parent); if (child) clean(child); },
    replayEffects() { for (const target of [parent, child]) if (target) { clean(target); target.slots.forEach(s => { if (s.setup) s.cleanup = s.setup() || undefined; }); } },
  };
}
test("unknown image offers a read-only task check without automatic dispatch", async () => { const h = host(); h.render(); assert.equal(h.requests.length, 0); h.click(); assert.equal(h.requests.length, 1); assert.equal(h.requests[0].args[0], `/api/v1/assistant/image-task/${taskId}`); h.requests[0].resolve({ data: receipt() }); await settle(); assert.match(h.text(), /imageTaskReceipt.completed/); assert.equal(h.posts(), 0); h.unmount(); });
test("completed receipt leaves the original uncertain chat outcome unchanged", async () => { const h = host(); const before = JSON.stringify(h.props.message); h.click(); h.requests[0].resolve({ data: receipt() }); await settle(); assert.match(h.text(), /imageTaskReceipt.completed/); assert.match(h.text(), /outcome.unknown.title/); assert.equal(JSON.stringify(h.props.message), before); assert.equal(h.posts(), 0); h.unmount(); });
for (const status of ["pending", "claimed", "running", "completed", "failed", "unknown", "dead_letter", "future_state"]) test(`task receipt preserves producer status ${status}`, async () => { const h = host(); h.click(); h.requests[0].resolve({ data: receipt({ status }) }); await settle(); assert.ok(h.text().includes(status)); assert.equal(h.posts(), 0); h.unmount(); });
for (const [name, value] of Object.entries({ task: receipt({ task_id: "imt_bbbbbbbbbbbbbbbbbbbb" }), session: receipt({ session_id: "session-b" }), turn: receipt({ turn_id: "other-turn" }), missing_session: receipt({ session_id: null }), empty_status: receipt({ status: "" }), missing: null })) test(`unverified ${name} receipt remains unavailable`, async () => { const h = host(); h.click(); h.requests[0].resolve({ data: value }); await settle(); assert.match(h.text(), /imageTaskReceipt.unavailable/); assert.doesNotMatch(h.text(), /observed-model/); assert.equal(h.posts(), 0); h.unmount(); });
test("read denial and later explicit retry never leak raw error or replay generation", async () => { const h = host(); h.click(); h.requests[0].reject({ response: { status: 404, data: "PRIVATE BODY" } }); await settle(); assert.match(h.text(), /imageTaskReceipt.unavailable/); assert.doesNotMatch(h.text(), /PRIVATE BODY/); h.click(); h.requests[1].resolve({ data: receipt({ status: "unknown" }) }); await settle(); assert.match(h.text(), /imageTaskReceipt.unknown/); assert.equal(h.posts(), 0); h.unmount(); });
test("same-tick repeated task checks share one read", async () => { const h = host(); const click = h.check().props.onClick as () => void; click(); click(); assert.equal(h.requests.length, 1); assert.equal(h.check().props.disabled, true); h.requests[0].resolve({ data: receipt() }); await settle(); assert.equal(h.check().props.disabled, false); h.unmount(); });
for (const change of ["auth", "auth_aba", "session", "session_aba", "task", "task_aba", "revoked", "unmount"]) test(`${change} retires pending receipt and stale handlers`, async () => { const h = host(); const stale = h.check().props.onClick as () => void; h.click();
  if (change.startsWith("auth")) { h.changeAuth("token-b"); if (change.endsWith("aba")) h.changeAuth("token-a"); h.render(); }
  else if (change.startsWith("session")) { h.props.sessionId = "session-b"; h.render(); if (change.endsWith("aba")) { h.props.sessionId = "session-a"; h.render(); } }
  else if (change.startsWith("task")) { h.props.message.diagnosticId = "imt_bbbbbbbbbbbbbbbbbbbb"; h.render(); if (change.endsWith("aba")) { h.props.message.diagnosticId = taskId; h.render(); } }
  else if (change === "revoked") { h.props.message.sourceAccessRevoked = true; h.render(); } else h.unmount();
  stale(); assert.equal(h.requests.length, 1); h.requests[0].resolve({ data: receipt() }); await settle(); if (change !== "unmount") assert.doesNotMatch(h.text(), /observed-model/); assert.equal(h.posts(), 0); h.unmount(); });
test("late old error cannot clear a new session receipt or its single-flight lock", async () => { const h = host(); h.click(); h.props.sessionId = "session-b"; h.render(); h.click(); h.requests[0].reject(Error("old")); await settle(); assert.equal(h.check().props.disabled, true); h.requests[1].resolve({ data: receipt({ session_id: "session-b", effective_model_id: "new-model" }) }); await settle(); assert.match(h.text(), /new-model/); assert.doesNotMatch(h.text(), /imageTaskReceipt.unavailable/); h.unmount(); });
for (const diagnostic of ["", "bad", "11111111-1111-1111-1111-111111111111"]) test(`non-image diagnostic ${diagnostic || "missing"} has no task check`, () => { const h = host(); h.props.message.diagnosticId = diagnostic; assert.doesNotMatch(h.text(), /imageTaskReceipt.check/); assert.equal(h.requests.length, 0); h.unmount(); });
test("missing current session does not offer a task check", () => { const h = host(); h.props.sessionId = ""; assert.doesNotMatch(h.text(), /imageTaskReceipt.check/); assert.equal(h.requests.length, 0); h.unmount(); });
test("effect replay does not fetch automatically and later check remains usable", async () => { const h = host(); h.render(); h.replayEffects(); assert.equal(h.requests.length, 0); h.click(); h.requests[0].resolve({ data: receipt() }); await settle(); assert.match(h.text(), /observed-model/); h.unmount(); });
test("task receipt omits raw image URLs prompts errors and invalid optional metadata", async () => { const h = host(); h.click(); h.requests[0].resolve({ data: receipt({ images: [{ url: "https://untrusted.invalid/PRIVATE" }], prompt: "PRIVATE PROMPT", error: "PRIVATE ERROR", progress: 999, effective_model_id: null }) }); await settle(); assert.match(h.text(), /imageTaskReceipt.completed/); assert.doesNotMatch(h.text(), /PRIVATE|999|observed-model/); h.unmount(); });
