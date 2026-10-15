/** SDK identities are supplied by Gateway metadata, never request/task/thread IDs. */
export type SdkEngine = "openai_agents" | "claude_agent_sdk";
export interface SdkRunReference { runId: string; engine: SdkEngine; sessionId?: string }
export type SdkRunState = "pending" | "queued" | "running" | "awaiting_approval" |
  "completed" | "succeeded" | "failed" | "cancelled" | "expired" | "unknown";
export interface SdkUsageObservation {
  state: "reported" | "unavailable";
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  providerCostUsd?: number;
  providerOutcome: "completed" | "unknown";
  accountingStatus: "not_asserted";
}
export interface SdkRunView extends SdkRunReference {
  status: SdkRunState;
  usage: SdkUsageObservation;
}
function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
function engine(value: unknown): value is SdkEngine {
  return value === "openai_agents" || value === "claude_agent_sdk";
}
function runId(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}

/** Accept current stream metadata and server-owned history stats, including zero-text approvals. */
export function sdkRunReference(metadata: unknown, sessionId?: string): SdkRunReference | undefined {
  const meta = record(metadata);
  const observations = [record(record(meta.usage).sdk_usage), record(record(meta.stats).sdk_usage)];
  const frameworks = [meta.sdk_framework, ...observations.map(item => item.framework)].filter(value => value !== undefined);
  const framework = frameworks[0];
  if (!engine(framework) || frameworks.some(value => value !== framework)) return undefined;
  const lifecycle = record(meta.sdk_lifecycle);
  const lifecycleId = meta.sdk_tools_profile === "governed_capabilities_v1" &&
    ["approval_required", "approval_result", "tool_result"].includes(String(lifecycle.event)) ? lifecycle.run_id : undefined;
  const ids = [meta.assistant_run_id, ...observations.map(item => item.assistant_run_id), lifecycleId]
    .filter(value => value !== undefined);
  const id = ids[0];
  if (!runId(id) || ids.some(value => !runId(value) || value.toLowerCase() !== id.toLowerCase())) return undefined;
  return { runId: id.toLowerCase(), engine: framework, ...(sessionId ? { sessionId } : {}) };
}

function count(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
}

/** Absent/partial usage is unavailable, never an estimate or a fabricated zero. */
export function sdkUsageObservation(value: unknown): SdkUsageObservation {
  const usage = record(value);
  const sdk = record(usage.sdk_usage);
  const inputTokens = count(usage.input_tokens);
  const outputTokens = count(usage.output_tokens);
  const sum = inputTokens !== undefined && outputTokens !== undefined ? inputTokens + outputTokens : undefined;
  const reported = sdk.state === "reported" && sum !== undefined && Number.isSafeInteger(sum) &&
    (usage.total_tokens === undefined || count(usage.total_tokens) === sum);
  return {
    state: reported ? "reported" : "unavailable",
    ...(inputTokens !== undefined ? { inputTokens } : {}),
    ...(outputTokens !== undefined ? { outputTokens } : {}),
    ...(reported ? { totalTokens: sum } : {}),
    ...(sdk.cost_state === "reported" && typeof sdk.provider_cost_usd === "number" &&
      Number.isFinite(sdk.provider_cost_usd) && sdk.provider_cost_usd >= 0 ? { providerCostUsd: sdk.provider_cost_usd } : {}),
    providerOutcome: sdk.provider_outcome === "completed" ? "completed" : "unknown",
    accountingStatus: "not_asserted",
  };
}

export function sdkRunIsActive(status: SdkRunState): boolean {
  return ["pending", "queued", "running", "awaiting_approval"].includes(status);
}

/** The authenticated server remains authority; mismatched responses cannot enable cancellation. */
export function sdkRunView(response: unknown, expected: SdkRunReference): SdkRunView {
  const run = record(record(response).run);
  if (!runId(run.run_id) || run.run_id.toLowerCase() !== expected.runId || run.engine !== expected.engine ||
    (expected.sessionId !== undefined && run.session_id !== expected.sessionId)) throw new Error("sdk_run_identity_mismatch");
  const known = ["pending", "queued", "running", "awaiting_approval", "completed", "succeeded", "failed", "cancelled"];
  const status = run.terminal_reason === "sdk_authority_expired" ? "expired" :
    typeof run.status === "string" && known.includes(run.status) ? run.status as SdkRunState : "unknown";
  return { ...expected, status, usage: sdkUsageObservation(run.usage) };
}

export function sdkCancellationRecorded(response: unknown, expected: SdkRunReference): boolean {
  const result = record(response);
  if (result.task_id !== expected.runId || typeof result.cancelled !== "boolean" ||
    (expected.sessionId !== undefined && result.session_id !== expected.sessionId)) throw new Error("sdk_cancel_identity_mismatch");
  return result.cancelled;
}
