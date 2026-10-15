/** Only sanitized lifecycle fields become UI state; raw arguments never do. */
export const SDK_TOOLS_PROFILE = "governed_capabilities_v1";
const events = new Set(["approval_required", "approval_result", "tool_result"]);
const terminal = new Set(["succeeded", "failed", "cancelled", "timeout", "side_effect_unknown"]);
const decisions = new Set(["rejected", "cancelled", "expired", "approved", "consumed"]);

export interface SdkToolActivity {
  runId: string;
  toolCallId: string;
  tool: string;
  approvalId?: string;
  approvalStatus?: string;
  approvalResolutionStatus?: string;
  executionStatus?: string;
  executionId?: string;
  lastSequence?: number;
}

interface LifecycleChunk {
  event_type?: string;
  chunk_index?: number;
  metadata?: Record<string, unknown>;
}

export function isSdkLifecycleChunk(chunk: LifecycleChunk): boolean {
  return chunk.metadata?.sdk_tools_profile === SDK_TOOLS_PROFILE && events.has(chunk.event_type || "");
}

function nonempty(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

export function reduceSdkToolActivity(current: readonly SdkToolActivity[], chunk: LifecycleChunk): SdkToolActivity[] | null {
  if (!isSdkLifecycleChunk(chunk)) return null;
  const raw = chunk.metadata?.sdk_lifecycle;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const data = raw as Record<string, unknown>;
  if (data.event !== chunk.event_type || !nonempty(data.run_id) ||
      !nonempty(data.tool_call_id) || !nonempty(data.tool)) return null;
  const previous = current.find((item) => item.runId === data.run_id && item.toolCallId === data.tool_call_id);
  if (previous && (previous.tool !== data.tool ||
      (typeof chunk.chunk_index === "number" && previous.lastSequence !== undefined && chunk.chunk_index <= previous.lastSequence))) return null;
  const next: SdkToolActivity = {
    ...previous, runId: data.run_id, toolCallId: data.tool_call_id, tool: data.tool,
    ...(typeof chunk.chunk_index === "number" ? { lastSequence: chunk.chunk_index } : {}),
  };
  if (data.event === "approval_required") {
    if (!nonempty(data.approval_id) || previous?.executionStatus ||
        (previous?.approvalStatus && previous.approvalStatus !== "pending") ||
        (previous?.approvalId && previous.approvalId !== data.approval_id)) return null;
    next.approvalId = data.approval_id;
    next.approvalStatus = "pending";
  } else if (data.event === "approval_result") {
    if (!nonempty(data.approval_id) || typeof data.status !== "string" || !terminal.has(data.status) ||
        (previous?.approvalId && previous.approvalId !== data.approval_id)) return null;
    next.approvalId = data.approval_id;
    next.approvalStatus = typeof data.approval_status === "string" && decisions.has(data.approval_status)
      ? data.approval_status : data.status;
    next.approvalResolutionStatus = data.status;
    // An approval resolving does not establish that the tool executed.
  } else {
    if (typeof data.status !== "string" || !terminal.has(data.status)) return null;
    next.executionStatus = data.status;
    if (nonempty(data.execution_id)) next.executionId = data.execution_id;
  }
  return previous ? current.map((item) => item === previous ? next : item) : [...current, next];
}

/** GET approvals have decision states distinct from execution lifecycle terminal states. */
export function sdkApprovalDisplayStatus(status: string, expiresInSeconds?: unknown, active?: unknown): string {
  if (status === "pending" && typeof expiresInSeconds === "number" && Number.isFinite(expiresInSeconds) && expiresInSeconds <= 0) return "expired";
  if (status === "pending" && active === false) return "inactive";
  return ["pending", "approved", "rejected", "denied", "expired", "cancelled", "consumed",
    "succeeded", "failed", "timeout", "side_effect_unknown", "inactive"].includes(status) ? status : "unknown";
}

export function sdkApprovalPreviewIsActionable(preview: unknown, toolName: string): boolean {
  if (!preview || typeof preview !== "object" || Array.isArray(preview)) return false;
  const value = preview as Record<string, unknown>;
  return value.can_approve === true && value.tool_name === toolName && value.effect === "write" &&
    nonempty(value.target) && nonempty(value.authorization_scope) && nonempty(value.arguments_hash) &&
    typeof value.expires_in_seconds === "number" && Number.isFinite(value.expires_in_seconds) && value.expires_in_seconds > 0 &&
    Array.isArray(value.parameters) && value.parameters.every((item) => item && typeof item === "object" &&
      nonempty(item.name) && typeof item.value === "string");
}
