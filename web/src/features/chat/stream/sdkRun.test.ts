import assert from "node:assert/strict";
import test from "node:test";
import { sdkRunReference, sdkUsageObservation, sdkRunView, sdkRunIsActive, sdkCancellationRecorded } from "./sdkRun.ts";
import { createSdkRunController, type SdkRunControlState } from "./sdkRunController.ts";

const id = "12345678-1234-4234-8234-123456789abc";
const other = "87654321-1234-4234-8234-123456789abc";
const reference = { runId: id, engine: "openai_agents" as const, sessionId: "session" };
const result = (status = "running", extra = {}) => ({ run: { run_id: id, engine: "openai_agents", session_id: "session", status, ...extra } });
const cancelled = () => ({ task_id: id, session_id: "session", cancelled: true });

test("SDK identities come from genuine stream, lifecycle, final and server history metadata", () => {
  for (const framework of ["openai_agents", "claude_agent_sdk"]) {
    for (const metadata of [
      { sdk_framework: framework, assistant_run_id: id },
      { sdk_framework: framework, sdk_tools_profile: "governed_capabilities_v1", sdk_lifecycle: { event: "approval_required", run_id: id } },
      { usage: { sdk_usage: { framework, assistant_run_id: id } } },
      { stats: { sdk_usage: { framework, assistant_run_id: id } } },
    ]) assert.deepEqual(sdkRunReference(metadata, "session"), { ...reference, engine: framework });
  }
});
test("missing, forged, conflicting and non-SDK references never use request/task/thread IDs", () => {
  for (const metadata of [undefined, {}, { sdk_framework: "agent_runtime", assistant_run_id: id },
    { sdk_framework: "openai_agents", request_id: id, task_id: id, thread_id: id },
    { sdk_framework: "openai_agents", assistant_run_id: "../../tasks/other" },
    { sdk_framework: "openai_agents", assistant_run_id: id, usage: { sdk_usage: { framework: "claude_agent_sdk", assistant_run_id: id } } },
    { sdk_framework: "openai_agents", assistant_run_id: id, stats: { sdk_usage: { framework: "openai_agents", assistant_run_id: other } } },
    { sdk_framework: "openai_agents", sdk_lifecycle: { event: "approval_required", run_id: id } },
  ]) assert.equal(sdkRunReference(metadata), undefined);
});
test("usage preserves measured zero and cost-only observations without inventing settled charges", () => {
  assert.deepEqual(sdkUsageObservation({ input_tokens: 0, output_tokens: 0, sdk_usage: { state: "reported", cost_state: "reported", provider_cost_usd: 0, accounting_status: "settled" } }), {
    state: "reported", inputTokens: 0, outputTokens: 0, totalTokens: 0, providerCostUsd: 0,
    providerOutcome: "unknown", accountingStatus: "not_asserted",
  });
  const observation = sdkUsageObservation({ output_tokens: 5, sdk_usage: { state: "unavailable", cost_state: "reported", provider_cost_usd: 0.2 }, secret: "not-projected" });
  assert.equal(observation.state, "unavailable"); assert.equal(observation.inputTokens, undefined);
  assert.equal(observation.totalTokens, undefined); assert.equal(observation.providerCostUsd, 0.2);
  assert.equal(JSON.stringify(observation).includes("not-projected"), false);
});
test("partial, inconsistent, unsafe, negative and string counts are never complete token usage", () => {
  for (const counts of [{}, { input_tokens: 2 }, { input_tokens: -1, output_tokens: 2 },
    { input_tokens: "3", output_tokens: 1 }, { input_tokens: 3, output_tokens: 1, total_tokens: 9 },
    { input_tokens: 0.5, output_tokens: 1 }, { input_tokens: Number.MAX_SAFE_INTEGER, output_tokens: 1 },
    { input_tokens: Infinity, output_tokens: 1 }, { input_tokens: 3, output_tokens: 1, total_tokens: null }]) {
    const observation = sdkUsageObservation({ ...counts, sdk_usage: { state: "reported" } });
    assert.equal(observation.state, "unavailable"); assert.equal(observation.totalTokens, undefined);
  }
  assert.equal(sdkUsageObservation({ input_tokens: 3, output_tokens: 1, sdk_usage: { state: "reported" } }).totalTokens, 4);
});
test("status verifies exact owner response identity, expiry and SDK engine without Runtime lineage", () => {
  for (const patch of [{ run_id: other }, { engine: "agent_runtime" }, { session_id: "other" }]) assert.throws(() => sdkRunView(result("running", patch), reference));
  for (const status of ["pending", "queued", "running", "awaiting_approval"]) assert.equal(sdkRunIsActive(sdkRunView(result(status), reference).status), true);
  for (const status of ["completed", "succeeded", "failed", "cancelled", "processing", "anything"]) assert.equal(sdkRunIsActive(sdkRunView(result(status), reference).status), false);
  assert.equal(sdkRunView(result("running", { terminal_reason: "sdk_authority_expired" }), reference).status, "expired");
  assert.equal("harness_thread_id" in sdkRunView(result("running", { harness_thread_id: "not-a-real-sdk-thread" }), reference), false);
  assert.throws(() => sdkCancellationRecorded({ ...cancelled(), task_id: other }, reference));
});
function host() {
  let current = true;
  let reads = 0, posts = 0;
  let read: (signal: AbortSignal) => Promise<unknown> = async () => result();
  let post: (signal: AbortSignal) => Promise<unknown> = async () => cancelled();
  let state: SdkRunControlState | undefined;
  const controller = createSdkRunController(reference, {
    isCurrent: () => current,
    read: async (run, signal) => { assert.equal(run, id); reads++; return read(signal); },
    cancel: async (run, signal) => { assert.equal(run, id); posts++; return post(signal); },
  });
  controller.subscribe(next => { state = next; });
  return { controller, get state() { return state!; }, get reads() { return reads; }, get posts() { return posts; },
    setCurrent(value: boolean) { current = value; }, setRead(value: typeof read) { read = value; }, setPost(value: typeof post) { post = value; } };
}
test("inspection is explicit and repeated reads/cancel clicks send exactly one mutation", async () => {
  const h = host(); assert.equal(h.reads, 0);
  await h.controller.cancel(); assert.equal(h.posts, 0);
  await Promise.all([h.controller.refresh(), h.controller.refresh()]); assert.equal(h.reads, 1);
  await Promise.all([h.controller.cancel(), h.controller.cancel()]);
  assert.equal(h.posts, 1); assert.equal(h.reads, 3); assert.equal(h.state.cancellation, "recorded");
  await h.controller.cancel(); assert.equal(h.posts, 1);
});
test("completion, cancellation, expiry, changed identity and GET errors at click time prevent POST", async () => {
  for (const response of [result("completed"), result("cancelled"), result("running", { terminal_reason: "sdk_authority_expired" }), result("running", { run_id: other }), new Error("denied")]) {
    const h = host(); await h.controller.refresh();
    h.setRead(async () => { if (response instanceof Error) throw response; return response; });
    await h.controller.cancel(); assert.equal(h.posts, 0);
    assert.equal(h.state.pending, null);
  }
});
test("unavailable usage never blocks reading/cancelling an otherwise verified active run", async () => {
  const h = host(); await h.controller.refresh(); assert.equal(h.state.run?.usage.state, "unavailable");
  await h.controller.cancel(); assert.equal(h.posts, 1);
});
test("lost cancellation response reconciles using GET and forbids automatic or repeated POST", async () => {
  const h = host(); await h.controller.refresh();
  h.setPost(async () => { throw new Error("timeout with an unknown result"); });
  await h.controller.cancel(); assert.equal(h.posts, 1); assert.equal(h.reads, 3);
  assert.equal(h.state.cancellation, "unknown"); assert.equal(h.state.run?.status, "running");
  await h.controller.cancel(); await h.controller.refresh(); assert.equal(h.posts, 1);
  h.setRead(async () => result("cancelled")); await h.controller.refresh();
  assert.equal(h.state.cancellation, "recorded");
});
test("usage/status read failure after recorded cancellation does not erase its receipt", async () => {
  const h = host(); await h.controller.refresh();
  h.setPost(async () => { h.setRead(async () => { throw new Error("read unavailable"); }); return cancelled(); });
  await h.controller.cancel(); assert.equal(h.state.cancellation, "recorded");
  assert.equal(h.state.error, "unavailable"); assert.equal(h.state.run, null); assert.equal(h.posts, 1);
});
test("unknown cancellation followed by expiry/completion is reconciled without claiming provider cancellation", async () => {
  for (const status of ["completed", "failed"]) {
    const h = host(); await h.controller.refresh();
    h.setPost(async () => { h.setRead(async () => result(status)); throw new Error("lost reply"); });
    await h.controller.cancel(); assert.equal(h.state.cancellation, "finished");
    assert.equal(h.state.run?.usage.providerOutcome, "unknown"); assert.equal(h.posts, 1);
  }
});
test("account/token/view invalidation during click-time GET cannot cancel with stale identity", async () => {
  const h = host(); await h.controller.refresh();
  h.setRead(async () => { h.setCurrent(false); return result(); });
  await h.controller.cancel(); assert.equal(h.posts, 0);
});
test("unmount aborts pending reads and ignores late results", async () => {
  const h = host(); let signal: AbortSignal | undefined; let resolve: ((v: unknown) => void) | undefined;
  h.setRead(next => { signal = next; return new Promise(done => { resolve = done; }); });
  const request = h.controller.refresh(); h.controller.dispose();
  assert.equal(signal?.aborted, true); resolve!(result()); await request;
  assert.equal(h.state.run, null); await h.controller.cancel(); assert.equal(h.posts, 0);
});
test("unmount during POST aborts it without a mutation retry or stale follow-up read", async () => {
  const h = host(); await h.controller.refresh(); let signal: AbortSignal | undefined;
  h.setPost(async next => { signal = next; h.controller.dispose(); return cancelled(); });
  await h.controller.cancel(); assert.equal(signal?.aborted, true);
  assert.equal(h.posts, 1); assert.equal(h.reads, 2); assert.equal(h.state.cancellation, "unknown");
});
