import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as binding from "../../../features/chat/stream/sdkApprovalBinding.ts";
import * as lifecycle from "../../../features/chat/stream/sdkLifecycle.ts";

// Exercise the actual card's closures using a deterministic hook host, without a browser.
function host() {
  const slots: unknown[] = [];
  let cursor = 0;
  const effects: (() => void)[] = [];
  const state = { token: "fixture-token", user: { user_id: "fixture-owner" } };
  let fetch: () => Promise<unknown> = async () => fixture();
  const store = Object.assign((select: (s: typeof state) => unknown) => select(state), { getState: () => state });
  const react = {
    useState(initial: unknown) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
      return [slots[index], (value: unknown) => { slots[index] = value; }];
    },
    useRef(initial: unknown) { const index = cursor++; return slots[index] ?? (slots[index] = { current: initial }); },
    useCallback(fn: unknown) { cursor++; return fn; },
    useEffect(fn: () => void) { const index = cursor++; if (!(index in slots)) { slots[index] = true; effects.push(fn); } },
  };
  const jsx = (type: unknown, props: unknown) => ({ type, props });
  const modules: Record<string, unknown> = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "react-i18next": { useTranslation: () => ({ t: (_key: string, fallback?: unknown) => fallback ?? _key }) },
    "@/api/agentThreads": { getAgentRuntimeApproval: () => fetch() },
    "@/api/assistant": { getAssistantToolApproval: () => fetch() },
    "@/features/chat/stream/sdkLifecycle": lifecycle, "@/features/chat/stream/sdkApprovalBinding": binding,
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: (user: unknown) => JSON.stringify(user) },
    "@/store/useAuthStore": { useAuthStore: store },
    "@/pages/agents/agentCatalogPresentation": { capabilityDisplayName: (name: string) => name },
  };
  const source = readFileSync(new URL("./ToolApprovalCard.tsx", import.meta.url), "utf8");
  const exports: Record<string, (props: unknown) => { type: (props: unknown) => unknown; props: unknown }> = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; }, window: { setInterval: () => 1, clearInterval: () => {} }, Date });
  return {
    state, setFetch(fn: typeof fetch) { fetch = fn; },
    render(props: unknown) { cursor = 0; const outer = exports.ToolApprovalCard(props); return outer.type(outer.props); },
    async mount(props: unknown) { this.render(props); effects.splice(0).forEach(fn => fn()); await settle(); return this.render(props); },
  };
}
const fixture = () => ({ approval: { approval_id: "approval", run_id: "run", tool_call_id: "call", tool_name: "create_document", status: "pending" },
  preview: { can_approve: true, tool_name: "create_document", effect: "write", target: "document", authorization_scope: "one action",
    arguments_hash: "a".repeat(64), expires_in_seconds: 60, parameters: [{ name: "title", value: "fixture" }] } });
const base = { tool: { id: "call", name: "create_document", approvalId: "approval" }, approvalSource: "sdk", sdkRunId: "run" };
async function settle() { for (let i = 0; i < 40; i++) await Promise.resolve(); }
function buttons(tree: unknown): { disabled: boolean; onClick: () => void }[] {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(buttons);
  const node = tree as { type: string; props: { children?: unknown; disabled: boolean; onClick: () => void } };
  return node.type === "button" ? [node.props] : buttons(node.props?.children);
}

for (const approved of [true, false]) {
  test(`SDK ${approved ? "approve" : "reject"} sends exact refreshed binding once`, async () => {
    const h = host(); const decisions: unknown[][] = [];
    const props = { ...base, onSdkDecision: (...args: unknown[]) => { decisions.push(args); } };
    const tree = await h.mount(props); const button = buttons(tree)[approved ? 0 : 1];
    assert.equal(button.disabled, false); button.onClick(); button.onClick(); await settle();
    assert.deepEqual(decisions, [[approved, binding.sdkApprovalBinding(fixture(), { approvalId: "approval", runId: "run", toolCallId: "call", tool: "create_document" })]]);
    buttons(h.render(props))[approved ? 0 : 1].onClick(); await settle(); assert.equal(decisions.length, 1);
  });
  test(`SDK ${approved ? "approve" : "reject"} refuses changed hash and stale token/owner/expiry`, async () => {
    for (const change of ["hash", "token", "owner", "expiry", "inactive", "identity", "missing_hash"]) {
      const h = host(); let calls = 0;
      const props = { ...base, onSdkDecision: () => { calls++; } };
      const tree = await h.mount(props);
      h.setFetch(async () => {
        const value = fixture();
        if (change === "hash") value.preview.arguments_hash = "b".repeat(64);
        if (change === "missing_hash") value.preview.arguments_hash = "";
        if (change === "token") h.state.token = "replaced";
        if (change === "owner") h.state.user.user_id = "other-owner";
        if (change === "expiry") value.preview.expires_in_seconds = 0;
        if (change === "inactive") Object.assign(value.approval, { active: false });
        if (change === "identity") value.approval.run_id = "other-run";
        return value;
      });
      buttons(tree)[approved ? 0 : 1].onClick(); await settle(); assert.equal(calls, 0, change);
    }
  });
}
test("failed SDK POST only refreshes; no automatic mutation retry", async () => {
  const h = host(); let calls = 0; let reads = 0;
  h.setFetch(async () => { reads++; return fixture(); });
  const props = { ...base, onSdkDecision: () => { calls++; throw new Error("uncertain"); } };
  buttons(await h.mount(props))[0].onClick(); await settle();
  assert.equal(calls, 1); assert.equal(reads, 3);
});
test("Runtime cards preserve their original argument-free approve and reject callbacks", async () => {
  for (const approved of [true, false]) {
    const h = host(); const decisions: unknown[][] = [];
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: (...args: unknown[]) => decisions.push([true, ...args]),
      onReject: (...args: unknown[]) => decisions.push([false, ...args]) };
    buttons(await h.mount(props))[approved ? 0 : 1].onClick(); await settle();
    assert.deepEqual(decisions, [[approved]]);
  }
});
test("SDK cards cannot submit either decision without a reviewed digest or after source revocation", async () => {
  for (const revoked of [true, false]) {
    const h = host(); let calls = 0;
    if (!revoked) h.setFetch(async () => ({ ...fixture(), preview: { ...fixture().preview, arguments_hash: "" } }));
    const props = { ...base, sourceAccessRevoked: revoked, onSdkDecision: () => { calls++; } };
    const tree = await h.mount(props);
    for (const button of buttons(tree).slice(0, 2)) { assert.equal(button.disabled, true); button.onClick(); }
    await settle(); assert.equal(calls, 0);
  }
});
test("SDK activity forwards both decisions and the complete reviewed triple into POST", async () => {
  const requests: unknown[][] = [];
  const user = { user_id: "owner" };
  const jsx = (type: unknown, props: unknown) => ({ type, props });
  const modules: Record<string, unknown> = {
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-i18next": { useTranslation: () => ({ t: (key: string) => key }) },
    "@/api/assistant": { approveToolCall: async (...args: unknown[]) => { requests.push(args); } },
    "@/lib/sdkAuthorityScope": { sdkAuthorityScope: () => "owner-scope" },
    "@/store/useAuthStore": { useAuthStore: (select: (s: unknown) => unknown) => select({ user }) },
    "@/pages/agents/agentCatalogPresentation": { capabilityDisplayName: (name: string) => name },
    "@/pages/assistant/components/ToolApprovalCard": { ToolApprovalCard: "card" },
  };
  const source = readFileSync(new URL("../../../components/chat/SdkToolActivity.tsx", import.meta.url), "utf8");
  const exports: Record<string, (props: unknown) => unknown> = {};
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText,
    { exports, require: (id: string) => { assert.ok(id in modules, id); return modules[id]; } });
  const props = { items: [{ runId: "run", toolCallId: "call", tool: "create_document", approvalId: "approval", approvalStatus: "pending" }],
    active: true, authorityScope: "owner-scope" };
  const tree = exports.SdkToolActivity(props) as { props: { children: { props: { children: { props: { onSdkDecision: (approved: boolean, value: unknown) => Promise<void> } } } }[] } };
  const callback = tree.props.children[0].props.children.props.onSdkDecision;
  const reviewed = binding.sdkApprovalBinding(fixture(), { approvalId: "approval", runId: "run", toolCallId: "call", tool: "create_document" })!;
  await callback(true, reviewed); await callback(false, reviewed);
  assert.deepEqual(JSON.parse(JSON.stringify(requests)), [
    ["approval", { approved: true, ...reviewed }], ["approval", { approved: false, ...reviewed }],
  ]);
  assert.equal(exports.SdkToolActivity({ ...props, authorityScope: "old-owner" }), null);
});

test("changed SDK action requires a new manual decision after reviewing the refreshed preview", async () => {
  const h = host(); const decisions: unknown[][] = [];
  const props = { ...base, onSdkDecision: (...args: unknown[]) => { decisions.push(args); } };
  const original = await h.mount(props);
  h.setFetch(async () => ({ ...fixture(), preview: { ...fixture().preview, arguments_hash: "b".repeat(64) } }));
  buttons(original)[1].onClick(); await settle(); assert.equal(decisions.length, 0);
  buttons(h.render(props))[1].onClick(); await settle();
  assert.equal(decisions.length, 1);
  assert.equal((decisions[0][1] as binding.SdkApprovalBinding).expected_arguments_hash, "b".repeat(64));
});

// Mirrors SDK Gateway get(): the exact row hash survives its arguments removal,
// while an unsupported action preview intentionally omits preview.arguments_hash.
function unavailableFixture() {
  return { approval: { ...fixture().approval, arguments_hash: "a".repeat(64), active: true, expires_in_seconds: 60 },
    preview: { can_approve: false, authorization_scope: "this action only", reason: "Action details could not be verified." } };
}

test("unsupported SDK action can be rejected once but can never be approved", async () => {
  const h = host(); const decisions: unknown[][] = [];
  h.setFetch(async () => unavailableFixture());
  const props = { ...base, onSdkDecision: (...args: unknown[]) => {
    decisions.push(args);
    h.setFetch(async () => ({ ...unavailableFixture(), approval: { ...unavailableFixture().approval, status: "rejected" } }));
  } };
  const [approve, reject] = buttons(await h.mount(props));
  assert.equal(approve.disabled, true); assert.equal(reject.disabled, false);
  approve.onClick(); await settle(); assert.equal(decisions.length, 0);
  reject.onClick(); reject.onClick(); await settle();
  assert.deepEqual(decisions, [[false, { expected_run_id: "run", expected_tool_call_id: "call", expected_arguments_hash: "a".repeat(64) }]]);
  for (const button of buttons(h.render(props)).slice(0, 2)) { assert.equal(button.disabled, true); button.onClick(); }
  await settle(); assert.equal(decisions.length, 1);
});

test("unsupported SDK rejection rechecks resolved, expired and substituted rows before POST", async () => {
  for (const change of ["hash", "missing_hash", "bad_hash", "missing_preview", "null_preview", "preview_disagreement", "expiry", "inactive",
    "approved", "rejected", "cancelled", "expired", "consumed", "run_id", "tool_call_id", "approval_id", "tool_name", "token", "owner", "revoked"]) {
    const h = host(); let calls = 0;
    const props = { ...base, onSdkDecision: () => { calls++; } };
    h.setFetch(async () => unavailableFixture());
    const [, reject] = buttons(await h.mount(props));
    assert.equal(reject.disabled, false, change);
    h.setFetch(async () => {
      const value = unavailableFixture();
      if (change === "hash") value.approval.arguments_hash = "b".repeat(64);
      if (change === "missing_hash") Reflect.deleteProperty(value.approval, "arguments_hash");
      if (change === "bad_hash") value.approval.arguments_hash = "invalid";
      if (change === "missing_preview") Reflect.deleteProperty(value, "preview");
      if (change === "null_preview") Object.assign(value, { preview: null });
      if (change === "preview_disagreement") Object.assign(value.preview, { arguments_hash: "b".repeat(64) });
      if (change === "expiry") value.approval.expires_in_seconds = 0;
      if (change === "inactive") value.approval.active = false;
      if (["approved", "rejected", "cancelled", "expired", "consumed"].includes(change)) value.approval.status = change;
      if (["run_id", "tool_call_id", "approval_id", "tool_name"].includes(change)) Object.assign(value.approval, { [change]: "other" });
      if (change === "token") h.state.token = "replaced";
      if (change === "owner") h.state.user.user_id = "other-owner";
      if (change === "revoked") h.render({ ...props, sourceAccessRevoked: true });
      return value;
    });
    reject.onClick(); await settle(); assert.equal(calls, 0, change);
  }
});

test("changed unsupported SDK action needs another manual rejection after refreshed review", async () => {
  const h = host(); const decisions: unknown[][] = [];
  const props = { ...base, onSdkDecision: (...args: unknown[]) => { decisions.push(args); } };
  h.setFetch(async () => unavailableFixture());
  const [, reject] = buttons(await h.mount(props));
  h.setFetch(async () => ({ ...unavailableFixture(), approval: { ...unavailableFixture().approval, arguments_hash: "b".repeat(64) } }));
  reject.onClick(); await settle(); assert.equal(decisions.length, 0);
  const [approve, updatedReject] = buttons(h.render(props));
  assert.equal(approve.disabled, true); assert.equal(updatedReject.disabled, false);
  updatedReject.onClick(); await settle();
  assert.deepEqual(decisions, [[false, { expected_run_id: "run", expected_tool_call_id: "call", expected_arguments_hash: "b".repeat(64) }]]);
});

test("uncertain unsupported SDK rejection refreshes state without replaying POST", async () => {
  const h = host(); let calls = 0; let reads = 0;
  h.setFetch(async () => { reads++; return unavailableFixture(); });
  const props = { ...base, onSdkDecision: () => { calls++; throw new Error("uncertain"); } };
  buttons(await h.mount(props))[1].onClick(); await settle();
  assert.equal(calls, 1); assert.equal(reads, 3);
  const [approve, reject] = buttons(h.render(props));
  assert.equal(approve.disabled, true); assert.equal(reject.disabled, false);
});

// Run the actual session approval callback, including its error handling, with
// the actual card. Extract its AST declaration rather than copying its logic;
// unrelated chat hooks and transports are controlled in this in-process fixture.
function runtimeSessionDecision(post: (...args: unknown[]) => Promise<unknown>, missingRun = false) {
  const source = readFileSync(new URL("../hooks/useChatSession.ts", import.meta.url), "utf8");
  const parsed = ts.createSourceFile("useChatSession.ts", source, ts.ScriptTarget.Latest, true);
  let callback: ts.VariableDeclaration | undefined;
  function visit(node: ts.Node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(parsed) === "handleToolApproval") callback = node;
    ts.forEachChild(node, visit);
  }
  visit(parsed); assert.ok(callback);
  let messages = [{ id: "message", role: "assistant", processSummary: { runId: "run", runtimeThreadId: "thread",
    tools: [{ ...base.tool, status: "approval_required" }] } }];
  const changes: unknown[] = [];
  const exports: { decision?: (message: string, tool: string, approval: string, approved: boolean) => Promise<void> } = {};
  vm.runInNewContext(ts.transpileModule(`const ${callback.getText(parsed)}; exports.decision = handleToolApproval;`, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, {
    exports, require: (id: string) => { assert.equal(id, "@/api/agentThreads"); return { decideAgentRuntimeApproval: post }; },
    useCallback: (fn: unknown) => fn, messagesRef: { current: missingRun ? [] : messages }, console: { warn() {} },
    deadApprovalMessageIdsRef: { current: new Set() }, abortControllerRef: { current: {} },
    setServerRunBlocking: (value: boolean) => changes.push(value),
    setMessages: (update: (current: typeof messages) => typeof messages) => { messages = update(messages); changes.push(messages); },
    sendMessage: () => { throw new Error("A Runtime approval must not create a second turn"); },
    reconcileDeadApprovalRun: () => { throw new Error("The fixture has an active Runtime stream"); },
  });
  return { decide: (approved: boolean, approvalId = "approval") => exports.decision!("message", "call", approvalId, approved), changes,
    status: () => messages[0].processSummary.tools[0].status };
}

for (const approved of [true, false]) {
  test(`Runtime ${approved ? "approve" : "reject"} transport failure stays retryable only after pending readback`, async () => {
    const h = host(); const requests: unknown[][] = []; let fail = true;
    const session = runtimeSessionDecision(async (...args: unknown[]) => {
      requests.push(args); if (fail) throw new Error("Gateway 503 before decision");
    });
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => session.decide(true), onReject: () => session.decide(false) };
    const button = buttons(await h.mount(props))[approved ? 0 : 1];
    button.onClick(); button.onClick(); await settle();
    assert.equal(requests.length, 1); assert.equal(session.changes.length, 0);
    assert.equal(session.status(), "approval_required");
    assert.equal(buttons(h.render(props))[approved ? 0 : 1].disabled, false);
    fail = false;
    buttons(h.render(props))[approved ? 0 : 1].onClick(); await settle();
    assert.deepEqual(requests.map(args => Array.from(args)), [["thread", "approval", approved], ["thread", "approval", approved]]);
    assert.equal(session.status(), approved ? "running" : "error");
    const resolved = buttons(h.render(props))[approved ? 0 : 1];
    assert.equal(resolved.disabled, true); resolved.onClick(); await settle(); assert.equal(requests.length, 2);
  });
}

test("Runtime uncertain decision never replays when readback is resolved or unavailable", async () => {
  for (const status of ["approved", "rejected", "expired", "cancelled", "consumed", "unavailable"]) {
    const h = host(); let calls = 0; let reads = 0;
    h.setFetch(async () => { reads++; return fixture(); });
    const session = runtimeSessionDecision(async () => {
      calls++;
      h.setFetch(async () => {
        reads++;
        if (status === "unavailable") throw new Error("Readback unavailable");
        return { ...fixture(), approval: { ...fixture().approval, status } };
      });
      throw Object.assign(new Error("Decision response lost or Runtime rejects after settlement"), { status: 409 });
    });
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => session.decide(true), onReject: () => session.decide(false) };
    buttons(await h.mount(props))[0].onClick(); await settle();
    assert.equal(calls, 1, status); assert.equal(reads, 3, status);
    assert.equal(session.changes.length, 0, status);
    const rendered = JSON.stringify(h.render(props));
    if (status === "unavailable") assert.ok(rendered.includes("Decision result could not be confirmed"));
    else {
      assert.ok(rendered.includes(`services.sdkAgent.lifecycle.status.${status}`), status);
      assert.ok(!rendered.includes("Decision result could not be confirmed"), status);
    }
    for (const button of buttons(h.render(props)).slice(0, 2)) { assert.equal(button.disabled, true, status); button.onClick(); }
    await settle(); assert.equal(calls, 1, status);
  }
});


test("Runtime missing request or stale message context rejects without a false recorded decision", async () => {
  let calls = 0;
  const valid = runtimeSessionDecision(async () => { calls++; });
  const stale = runtimeSessionDecision(async () => { calls++; }, true);
  await assert.rejects(valid.decide(true, ""), /Approval request is unavailable/);
  await assert.rejects(stale.decide(false), /Approval run is unavailable/);
  assert.equal(calls, 0); assert.equal(valid.changes.length, 0); assert.equal(stale.changes.length, 0);
});

test("Runtime late original decision blocks the next manual POST during fresh preflight", async () => {
  for (const status of ["approved", "rejected", "cancelled", "expired", "consumed"]) {
    const h = host(); let calls = 0;
    const session = runtimeSessionDecision(async () => { calls++; throw new Error("Original decision response lost"); });
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => session.decide(true), onReject: () => session.decide(false) };
    buttons(await h.mount(props))[0].onClick(); await settle();
    const pending = buttons(h.render(props))[0];
    assert.equal(pending.disabled, false); assert.equal(calls, 1);
    // The original request settles after the failed POST's pending readback,
    // but before the user tries again. The next click must read, never replay.
    h.setFetch(async () => ({ ...fixture(), approval: { ...fixture().approval, status } }));
    pending.onClick(); await settle();
    assert.equal(calls, 1, status); assert.equal(session.changes.length, 0, status);
    for (const button of buttons(h.render(props)).slice(0, 2)) assert.equal(button.disabled, true, status);
    assert.ok(JSON.stringify(h.render(props)).includes(`services.sdkAgent.lifecycle.status.${status}`));
  }
});

// Runtime summary() exposes the deadline independently of preview availability.
// Expired pending rows may survive until the original waiter/recovery cleans up.
function expiredRuntimeFixture(remaining = 0, status = "pending") {
  return { approval: { ...fixture().approval, status, expires_in_seconds: remaining },
    preview: { can_approve: false, authorization_scope: "this action only", reason: "Action details could not be verified." } };
}

test("Runtime cold expired pending summary disables both decisions without a preview", async () => {
  for (const remaining of [0, -10]) {
    const h = host(); let calls = 0;
    h.setFetch(async () => expiredRuntimeFixture(remaining));
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => { calls++; }, onReject: () => { calls++; } };
    const tree = await h.mount(props);
    assert.ok(JSON.stringify(tree).includes("services.sdkAgent.lifecycle.status.expired"));
    for (const button of buttons(tree).slice(0, 2)) { assert.equal(button.disabled, true); button.onClick(); }
    await settle(); assert.equal(calls, 0);
  }
});

test("Runtime expiry during fresh decision preflight prevents either POST", async () => {
  for (const approved of [true, false]) {
    const h = host(); let calls = 0;
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => { calls++; }, onReject: () => { calls++; } };
    const tree = await h.mount(props);
    h.setFetch(async () => expiredRuntimeFixture());
    buttons(tree)[approved ? 0 : 1].onClick(); await settle();
    assert.equal(calls, 0);
    assert.ok(JSON.stringify(h.render(props)).includes("services.sdkAgent.lifecycle.status.expired"));
    for (const button of buttons(h.render(props)).slice(0, 2)) assert.equal(button.disabled, true);
  }
});

test("Runtime uncertain POST with expired pending readback never offers replay", async () => {
  for (const approved of [true, false]) {
    const h = host(); let calls = 0;
    const session = runtimeSessionDecision(async () => {
      calls++; h.setFetch(async () => expiredRuntimeFixture(-1)); throw new Error("Uncertain decision response");
    });
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => session.decide(true), onReject: () => session.decide(false) };
    buttons(await h.mount(props))[approved ? 0 : 1].onClick(); await settle();
    assert.equal(calls, 1); assert.equal(session.changes.length, 0);
    const tree = h.render(props);
    assert.ok(JSON.stringify(tree).includes("services.sdkAgent.lifecycle.status.expired"));
    for (const button of buttons(tree).slice(0, 2)) { assert.equal(button.disabled, true); button.onClick(); }
    await settle(); assert.equal(calls, 1);
  }
});

test("Runtime resolved decisions retain their status after the original deadline", async () => {
  for (const status of ["approved", "rejected", "cancelled", "consumed", "side_effect_unknown"]) {
    const h = host(); let calls = 0;
    h.setFetch(async () => expiredRuntimeFixture(-1, status));
    const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => { calls++; }, onReject: () => { calls++; } };
    const tree = await h.mount(props);
    assert.ok(JSON.stringify(tree).includes(`services.sdkAgent.lifecycle.status.${status}`));
    for (const button of buttons(tree).slice(0, 2)) { assert.equal(button.disabled, true); button.onClick(); }
    await settle(); assert.equal(calls, 0);
  }
});

test("Runtime unexpired unavailable preview remains rejection-only with one explicit decision", async () => {
  const h = host(); let calls = 0;
  h.setFetch(async () => expiredRuntimeFixture(60));
  const props = { tool: base.tool, runtimeThreadId: "thread", onApprove: () => { throw new Error("Unverified execution"); }, onReject: () => { calls++; } };
  const [approve, reject] = buttons(await h.mount(props));
  assert.equal(approve.disabled, true); assert.equal(reject.disabled, false);
  approve.onClick(); reject.onClick(); reject.onClick(); await settle(); assert.equal(calls, 1);
});
