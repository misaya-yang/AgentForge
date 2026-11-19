import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as binding from "../../../features/chat/stream/sdkApprovalBinding.ts";
import * as lifecycle from "../../../features/chat/stream/sdkLifecycle.ts";

type Element = { type: unknown; key?: string; props: { children?: unknown; disabled?: boolean; onClick?: () => void } };
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: () => void; setup?: () => (() => void) | void };
function text(node: unknown): string {
  if (Array.isArray(node)) return node.map(text).join(" ");
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node !== "object") return String(node);
  return text((node as Element).props?.children);
}
function buttons(node: unknown): Element[] {
  if (Array.isArray(node)) return node.flatMap(buttons);
  if (!node || typeof node !== "object") return [];
  return (node as Element).type === "button" ? [node as Element] : buttons((node as Element).props?.children);
}
const fixture = (status = "pending", expiry = 60) => ({
  approval: { approval_id: "approval", run_id: "run", tool_call_id: "call", tool_name: "create_document", status, expires_in_seconds: expiry },
  preview: { can_approve: status === "pending", tool_name: "create_document", effect: "write", target: "document", authorization_scope: "one action", arguments_hash: "a".repeat(64), expires_in_seconds: expiry, parameters: [] },
});
type Response = ReturnType<typeof fixture>;
async function settle() { for (let n = 0; n < 40; n++) await Promise.resolve(); }
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function host(engine: "runtime" | "sdk") {
  let slots: Slot[] = []; let cursor = 0; let key: string | undefined; let disposed = false;
  const effects: (() => void)[] = [];
  const state = { token: "token-a", user: { user_id: "owner-a" } };
  const reads: unknown[][] = []; const decisions: boolean[] = [];
  let get: () => Promise<Response> = async () => fixture();
  let post: () => Promise<void> = async () => {};
  const same = (a?: unknown[], b?: unknown[]) => Boolean(a && b && a.length === b.length && a.every((v, i) => Object.is(v, b[i])));
  const react = {
    useState(initial: unknown) { const i = cursor++; const slot = slots[i] ?? (slots[i] = { value: typeof initial === "function" ? initial() : initial }); return [slot.value, (v: unknown) => { slot.value = typeof v === "function" ? v(slot.value) : v; }]; },
    useRef(initial: unknown) { const i = cursor++; return (slots[i] ?? (slots[i] = { value: { current: initial } })).value; },
    useCallback(fn: unknown, deps: unknown[]) { const i = cursor++; if (!same(slots[i]?.deps, deps)) slots[i] = { value: fn, deps }; return slots[i].value; },
    useEffect(fn: () => (() => void) | void, deps: unknown[]) { const i = cursor++; if (!same(slots[i]?.deps, deps)) { const old = slots[i]; const slot: Slot = { deps, setup: fn }; slots[i] = slot; effects.push(() => { old?.cleanup?.(); slot.cleanup = fn() || undefined; }); } },
  };
  const store = Object.assign((select: (s: typeof state) => unknown) => select(state), { getState: () => state });
  const jsx = (type: unknown, props: Element["props"], key?: string) => ({ type, props, key });
  const read = (...args: unknown[]) => { reads.push(args); return get(); };
  const modules: Record<string, unknown> = { react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: translate }) },
    "@/api/agentThreads": { getAgentRuntimeApproval: read }, "@/api/assistant": { getAssistantToolApproval: read },
    "@/features/chat/stream/sdkLifecycle": lifecycle, "@/features/chat/stream/sdkApprovalBinding": binding,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) }, "@/store/useAuthStore": { useAuthStore: store },
    "@/pages/agents/agentCatalogPresentation": { capabilityDisplayName: (name: string) => name } };
  function translate(key: string, fallback?: unknown) {
    if (key === "services.sdkAgent.lifecycle.resolved" && fallback && typeof fallback === "object" && "status" in fallback) return `${key}: ${String(fallback.status)}`;
    return typeof fallback === "string" ? fallback : key;
  }
  const exports: Record<string, (props: unknown) => Element> = {};
  const source = readFileSync(new URL("./ToolApprovalCard.tsx", import.meta.url), "utf8");
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; }, Date, window: { setInterval: () => 1, clearInterval: () => {} } });
  const decide = async (approved: boolean) => { decisions.push(approved); await post(); };
  const props = { tool: { id: "call", name: "create_document", approvalId: "approval" }, runtimeThreadId: "thread", sourceAccessRevoked: false,
    ...(engine === "sdk" ? { approvalSource: "sdk", sdkRunId: "run", onSdkDecision: decide } : { onApprove: () => decide(true), onReject: () => decide(false) }) };
  function cleanup() { slots.forEach(s => s.cleanup?.()); }
  function render() { assert.equal(disposed, false); const outer = exports.ToolApprovalCard(props); if (key !== outer.key) { cleanup(); slots = []; key = outer.key; } cursor = 0; const tree = (outer.type as (props: unknown) => Element)(outer.props); effects.splice(0).forEach(fn => fn()); return tree; }
  return { props, state, reads, decisions, render, setGet(fn: typeof get) { get = fn; }, setPost(fn: typeof post) { post = fn; },
    replayEffects() { cleanup(); slots.forEach(slot => { if (slot.setup) slot.cleanup = slot.setup() || undefined; }); },
    dispose() { cleanup(); disposed = true; }, async mount() { render(); await settle(); return render(); },
    check() { const button = buttons(render()).find(b => /Check current request|Checking current request/.test(text(b))); assert.ok(button, "approval card must expose read-only current-request recovery"); return button; },
    decision(index = 0) { return buttons(render())[index]; },
  };
}
for (const engine of ["runtime", "sdk"] as const) {
  test(`${engine} initial approval read failure can be explicitly recovered without a decision`, async () => {
    const h = host(engine); h.setGet(async () => { throw Error("read unavailable"); }); await h.mount();
    assert.equal(h.decision().props.disabled, true); h.setGet(async () => fixture()); h.check().props.onClick!(); await settle();
    assert.equal(h.reads.length, 2); assert.equal(h.decisions.length, 0); assert.equal(h.decision().props.disabled, false);
    assert.deepEqual(h.reads[1], engine === "sdk" ? ["approval"] : ["thread", "approval"]);
  });
  for (const status of ["approved", "pending", "expired"] as const) {
    test(`${engine} uncertain decision and failed readback allow read-only recovery to ${status}`, async () => {
      const h = host(engine); await h.mount(); h.setPost(async () => { h.setGet(async () => { throw Error("readback unavailable"); }); throw Error("response lost"); });
      h.decision().props.onClick!(); await settle(); assert.equal(h.decisions.length, 1); assert.equal(h.decision().props.disabled, true);
      h.setGet(async () => fixture(status)); h.check().props.onClick!(); await settle(); assert.equal(h.decisions.length, 1); assert.equal(h.reads.length, 4);
      assert.equal(h.decision().props.disabled, status !== "pending"); if (status !== "pending") assert.match(text(h.render()), new RegExp(`status.${status}`));
    });
  }
  test(`${engine} repeated failed read checks retain an unconfirmed non-actionable request`, async () => {
    const h = host(engine); h.setGet(async () => { throw Error("offline"); }); await h.mount(); h.check().props.onClick!(); await settle();
    assert.equal(h.reads.length, 2); assert.equal(h.decisions.length, 0); assert.equal(h.decision().props.disabled, true); assert.equal(h.check().props.disabled, false);
  });
  test(`${engine} current-request checks are single-flight and exclude stale decision handlers`, async () => {
    const h = host(engine); await h.mount(); const decision = h.decision(); const pending = deferred<Response>(); h.setGet(() => pending.promise);
    const check = h.check(); check.props.onClick!(); check.props.onClick!(); decision.props.onClick!(); await settle();
    assert.equal(h.reads.length, 2); assert.equal(h.decisions.length, 0); assert.equal(h.check().props.disabled, true); assert.equal(h.decision().props.disabled, true);
    pending.resolve(fixture()); await settle(); assert.equal(h.check().props.disabled, false);
  });
  test(`${engine} decision preflight and uncertain settlement exclude manual reads`, async () => {
    const h = host(engine); await h.mount(); const pending = deferred<Response>(); h.setGet(() => pending.promise); const check = h.check(); h.decision().props.onClick!(); check.props.onClick!();
    assert.equal(h.reads.length, 2); pending.resolve(fixture()); await settle(); assert.equal(h.decisions.length, 1);
  });
  test(`${engine} detached approval handlers cannot read a new request`, async () => {
    const h = host(engine); await h.mount(); const old = h.check(); h.props.tool = { ...h.props.tool, approvalId: "new-approval" }; h.render(); await settle(); const before = h.reads.length;
    old.props.onClick!(); await settle(); assert.equal(h.reads.length, before); assert.equal(h.decisions.length, 0);
  });
  test(`${engine} authentication ABA disposes old read handlers`, async () => {
    const h = host(engine); await h.mount(); const old = h.check(); h.state.user = { user_id: "owner-b" }; h.render(); await settle(); h.state.user = { user_id: "owner-a" }; h.render(); await settle(); const before = h.reads.length;
    old.props.onClick!(); await settle(); assert.equal(h.reads.length, before); assert.equal(h.decisions.length, 0);
  });
  test(`${engine} late read completion cannot overwrite a replacement card`, async () => {
    const h = host(engine); await h.mount(); const pending = deferred<Response>(); h.setGet(() => pending.promise); h.check().props.onClick!();
    h.state.user = { user_id: "owner-b" }; h.setGet(async () => fixture("rejected")); h.render(); await settle(); pending.resolve(fixture()); await settle();
    assert.match(text(h.render()), /status.rejected/); assert.equal(h.decision().props.disabled, true); assert.equal(h.decisions.length, 0);
  });
  test(`${engine} source revocation and disposal block retained read callbacks`, async () => {
    const h = host(engine); await h.mount(); const old = h.check(); h.props.sourceAccessRevoked = true; h.render(); const before = h.reads.length;
    old.props.onClick!(); await settle(); assert.equal(h.reads.length, before); assert.equal(h.check().props.disabled, true);
    h.dispose(); old.props.onClick!(); await settle(); assert.equal(h.reads.length, before); assert.equal(h.decisions.length, 0);
  });
  test(`${engine} read recovery honors authoritative expiry on a still-pending row`, async () => {
    const h = host(engine); await h.mount(); h.setGet(async () => fixture("pending", 0)); h.check().props.onClick!(); await settle();
    assert.equal(h.decision().props.disabled, true); assert.equal(h.decision(1).props.disabled, true); assert.match(text(h.render()), /status.expired/); assert.equal(h.decisions.length, 0);
  });

  test(`${engine} mount-effect replay retains fresh read ownership until healthy verification settles`, async () => {
    const h = host(engine); const first = deferred<Response>(); const second = deferred<Response>();
    let reads = 0; h.setGet(() => ++reads === 1 ? first.promise : second.promise);
    h.render(); h.replayEffects(); assert.equal(h.reads.length, 2);
    first.resolve(fixture("approved")); await settle();
    assert.equal(h.check().props.disabled, true); assert.equal(h.decision().props.disabled, true);
    h.check().props.onClick!(); assert.equal(h.reads.length, 2); assert.doesNotMatch(text(h.render()), /status.approved/);
    second.resolve(fixture()); await settle();
    assert.equal(h.check().props.disabled, false); assert.equal(h.decision().props.disabled, false); assert.equal(h.decisions.length, 0);
  });

}
