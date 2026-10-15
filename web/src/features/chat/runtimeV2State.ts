export interface AgentV2Event {
  schema_version: "agent-event/v2";
  thread_id: string;
  sequence: number;
  event: {
    id: string;
    key: string;
    type: string;
    item_id: string | null;
    turn_id: string | null;
    status: string | null;
    payload: Record<string, unknown>;
  };
  timestamp: string;
}

export interface ProjectedRuntimeEvent {
  event_type: string;
  data: unknown;
  timestamp: number;
}

export interface PendingRuntimeApproval {
  approvalId: string;
  toolId: string;
  toolName: string;
  reason?: string;
  runId?: string;
  threadId?: string;
}

export interface RuntimeV2RunSnapshot {
  lastSequence: number;
  terminalStatus?: "succeeded" | "failed" | "cancelled";
  pendingApproval?: PendingRuntimeApproval;
  rejectedApproval: boolean;
}

type JsonRecord = Record<string, unknown>;

function asRecord(value: unknown): JsonRecord | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as JsonRecord
    : undefined;
}

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function textFromContent(value: unknown): string {
  if (typeof value === "string") return value;
  if (!Array.isArray(value)) return "";
  return value
    .map((part) => {
      const item = asRecord(part);
      return typeof item?.text === "string"
        ? item.text
        : typeof item?.content === "string"
          ? item.content
          : "";
    })
    .join("");
}

function parseJsonRecord(value: unknown): JsonRecord | undefined {
  if (typeof value !== "string" || !value.trim().startsWith("{")) return undefined;
  try {
    return asRecord(JSON.parse(value));
  } catch {
    return undefined;
  }
}

function findArtifactRecord(value: unknown, depth = 0): JsonRecord | undefined {
  if (depth > 4) return undefined;
  const record = asRecord(value) ?? parseJsonRecord(value);
  if (!record) return undefined;
  if (nonEmptyString(record.artifact_id) || nonEmptyString(record.artifactId)) {
    return record;
  }
  for (const key of ["artifact", "result", "output", "broker_response", "data", "payload"]) {
    const nested = findArtifactRecord(record[key], depth + 1);
    if (nested) return nested;
  }
  for (const key of ["content", "content_items", "contentItems"]) {
    const items = record[key];
    if (!Array.isArray(items)) continue;
    for (const item of items) {
      const nestedRecord = asRecord(item);
      const nested = findArtifactRecord(
        nestedRecord?.text ?? nestedRecord?.content ?? item,
        depth + 1,
      );
      if (nested) return nested;
    }
  }
  return undefined;
}

function artifactFormat(artifact: JsonRecord): string {
  const explicit = nonEmptyString(artifact.format);
  if (explicit) return explicit.toLowerCase();
  const filename = nonEmptyString(artifact.filename);
  const extension = filename?.match(/\.([a-z0-9]+)$/i)?.[1];
  if (extension) return extension.toLowerCase();
  const mimeType = nonEmptyString(artifact.mime_type) ?? nonEmptyString(artifact.mimeType);
  if (mimeType?.includes("wordprocessingml")) return "docx";
  if (mimeType === "application/pdf") return "pdf";
  return "file";
}

function artifactEventData(
  artifact: JsonRecord,
  common: JsonRecord,
): JsonRecord | undefined {
  const artifactId = nonEmptyString(artifact.artifact_id) ?? nonEmptyString(artifact.artifactId);
  if (!artifactId) return undefined;
  const format = artifactFormat(artifact);
  const filename = nonEmptyString(artifact.filename) ?? `artifact.${format}`;
  const downloadUrl =
    nonEmptyString(artifact.download_url) ??
    nonEmptyString(artifact.download_path) ??
    `/api/v1/assistant/artifacts/${encodeURIComponent(artifactId)}/download`;
  const explicitType = nonEmptyString(artifact.type) ?? nonEmptyString(artifact.artifact_type);
  const type = explicitType ?? (
    ["doc", "docx", "md", "pdf", "ppt", "pptx", "txt", "xlsx"].includes(format)
      ? "document"
      : "file"
  );
  return {
    ...common,
    ...artifact,
    artifact_id: artifactId,
    type,
    format,
    filename,
    title: nonEmptyString(artifact.title) ?? filename,
    download_url: downloadUrl,
  };
}

function projectRuntimeItem(
  item: JsonRecord,
  event: AgentV2Event,
  timestamp: number,
): ProjectedRuntimeEvent | null {
  const payload = asRecord(item.payload) ?? item;
  const itemType = typeof item.type === "string" ? item.type : "";
  const payloadType = typeof payload.type === "string" ? payload.type : "";
  const common = {
    thread_id: event.thread_id,
    item_id: event.event.item_id,
    turn_id: event.event.turn_id,
  };

  const content = textFromContent(payload.content) ||
    (typeof payload.message === "string" ? payload.message : "") ||
    (typeof payload.text === "string" ? payload.text : "");
  if (content && (payload.role === "assistant" || payloadType === "agent_message")) {
    return { event_type: "text_delta", data: { ...common, content }, timestamp };
  }
  if (content && (payload.role === "reasoning" || payloadType === "reasoning")) {
    return { event_type: "thinking_delta", data: { ...common, content }, timestamp };
  }

  // Approval and artifact items can also carry a tool name. Classify their
  // semantic type before the generic tool branch so durable replay cannot
  // turn an actionable approval/file into an inert timeline entry.
  const approvalId = nonEmptyString(payload.approval_id) ?? nonEmptyString(payload.approvalId);
  if (approvalId || itemType === "approval_request" || payloadType === "approval_request") {
    return {
      event_type: "approval_required",
      data: { ...common, ...payload, approval_id: approvalId },
      timestamp,
    };
  }

  const artifact = findArtifactRecord(payload);
  if (artifact || itemType === "artifact" || payloadType === "artifact") {
    const data = artifactEventData(artifact ?? payload, common);
    if (data) return { event_type: "artifact_created", data, timestamp };
  }

  const toolName = typeof payload.name === "string"
    ? payload.name
    : typeof payload.tool === "string" ? payload.tool : undefined;
  const toolCallId = typeof item.id === "string"
    ? item.id
    : typeof payload.id === "string" ? payload.id : undefined;
  if (toolName || ["function_call", "tool_use", "command_execution", "mcp_tool_call"].includes(itemType)) {
    const terminal = ["completed", "succeeded", "failed", "error", "cancelled"].includes(
      String(item.status ?? payload.status ?? "").toLowerCase(),
    );
    return {
      event_type: terminal ? "tool_call_result" : "tool_call_start",
      data: {
        ...common,
        tool_call_id: toolCallId,
        tool_name: toolName ?? itemType,
        arguments: payload.arguments ?? payload.input,
        result: payload.result ?? payload.output,
        status: item.status ?? payload.status,
      },
      timestamp,
    };
  }

  if (itemType === "activity" || payloadType === "activity" || payloadType === "event_msg") {
    return { event_type: "activity", data: { ...common, ...payload }, timestamp };
  }
  return null;
}

export function projectAgentV2Event(
  event: AgentV2Event,
  runtimeSessionId?: string,
): ProjectedRuntimeEvent | null {
  const payload = asRecord(event.event.payload) ?? {};
  const nestedEventType = typeof payload.event_type === "string" ? payload.event_type : "";
  const timestamp = Date.parse(event.timestamp) / 1000;
  if (nestedEventType) {
    const rawData = payload.data;
    const data = asRecord(rawData) ?? {};
    if (nestedEventType === "rollout/item" || nestedEventType === "item") {
      const projectedItem = asRecord(rawData);
      return projectedItem ? projectRuntimeItem(projectedItem, event, timestamp) : null;
    }
    const lifecycleData = nestedEventType === "run_started"
      ? {
          ...data,
          ...(data.session_id || !runtimeSessionId ? {} : { session_id: runtimeSessionId }),
          task_id: null,
          runtime: "agent_runtime_v2",
          reasoning: {
            requested_option: data.requested_reasoning_option,
            effective_option: data.effective_reasoning_option,
            adapter_id: data.reasoning_adapter_id,
            capability_revision: data.capability_revision,
            fallback_reason: data.reasoning_fallback_reason,
          },
        }
      : data;
    const projectedData = (
      (nestedEventType === "text_delta" || nestedEventType === "thinking_delta")
      && typeof data.content === "string"
    )
      ? data.content
      : (nestedEventType === "text_delta" || nestedEventType === "thinking_delta") && typeof rawData === "string"
        ? rawData
        : lifecycleData;
    return { event_type: nestedEventType, data: projectedData, timestamp };
  }
  if (payload.type === "response_item" || payload.type === "activity" || payload.type === "event_msg") {
    return projectRuntimeItem(payload, event, timestamp);
  }
  return null;
}

export function projectAgentV2Events(
  event: AgentV2Event,
  runtimeSessionId?: string,
): ProjectedRuntimeEvent[] {
  const primary = projectAgentV2Event(event, runtimeSessionId);
  const payload = asRecord(event.event.payload) ?? {};
  const rawData = asRecord(payload.data) ?? payload;
  const artifact = findArtifactRecord(rawData);
  const artifactData = artifactEventData(artifact ?? {}, {
    thread_id: event.thread_id,
    item_id: event.event.item_id,
    turn_id: event.event.turn_id,
  });
  if (!artifactData || primary?.event_type === "artifact_created") {
    return primary ? [primary] : [];
  }
  const artifactEvent: ProjectedRuntimeEvent = {
    event_type: "artifact_created",
    data: artifactData,
    timestamp: Date.parse(event.timestamp) / 1000,
  };
  return primary ? [primary, artifactEvent] : [artifactEvent];
}

export function createRuntimeV2RunSnapshot(afterSequence = 0): RuntimeV2RunSnapshot {
  return { lastSequence: Math.max(0, afterSequence), rejectedApproval: false };
}

function eventToolId(data: JsonRecord): string {
  for (const key of ["tool_id", "tool_call_id", "call_id", "id"]) {
    const value = nonEmptyString(data[key]);
    if (value) return value;
  }
  return "";
}

export function reduceRuntimeV2RunSnapshot(
  state: RuntimeV2RunSnapshot,
  event: AgentV2Event,
): RuntimeV2RunSnapshot {
  if (!Number.isFinite(event.sequence) || event.sequence <= state.lastSequence) return state;
  let next: RuntimeV2RunSnapshot = { ...state, lastSequence: event.sequence };
  for (const projected of projectAgentV2Events(event)) {
    const data = asRecord(projected.data) ?? {};
    if (projected.event_type === "approval_required") {
      const approvalId = nonEmptyString(data.approval_id);
      if (!approvalId) continue;
      const toolId = eventToolId(data) || approvalId;
      next = {
        ...next,
        rejectedApproval: false,
        pendingApproval: {
          approvalId,
          toolId,
          toolName: nonEmptyString(data.tool_name) ?? toolId,
          reason: nonEmptyString(data.reason),
          runId: nonEmptyString(data.run_id),
          threadId: nonEmptyString(data.thread_id) ?? event.thread_id,
        },
      };
    } else if (projected.event_type === "approval_result") {
      const approvalId = nonEmptyString(data.approval_id);
      const matches = !approvalId || !next.pendingApproval || next.pendingApproval.approvalId === approvalId;
      if (matches) {
        next = {
          ...next,
          pendingApproval: undefined,
          rejectedApproval:
            data.approved === false || String(data.status ?? "").toLowerCase() === "rejected",
        };
      }
    } else if (projected.event_type === "run_finished") {
      next = { ...next, terminalStatus: "succeeded", pendingApproval: undefined };
    } else if (projected.event_type === "run_error") {
      const status = String(data.status ?? asRecord(data.terminal_envelope)?.status ?? "failed").toLowerCase();
      next = {
        ...next,
        terminalStatus: status === "cancelled" ? "cancelled" : "failed",
        pendingApproval: undefined,
      };
    } else if (projected.event_type === "cancelled") {
      next = { ...next, terminalStatus: "cancelled", pendingApproval: undefined };
    }
  }
  return next;
}

export function isExpectedApprovalRejection(
  runErrorData: Record<string, unknown> | null | undefined,
  rejectedApproval: boolean,
): boolean {
  if (rejectedApproval) return true;
  const terminal = asRecord(runErrorData?.terminal_envelope);
  const detail = [
    runErrorData?.error,
    runErrorData?.message,
    runErrorData?.status,
    terminal?.exit_reason,
  ]
    .filter((value): value is string => typeof value === "string")
    .join(" ")
    .toLowerCase();
  return /(approval|capability).*(reject|declin|deni)|(?:reject|declin|deni).*(approval|capability)/.test(detail);
}

export function afterSequenceFromEventsUrl(url: string): number {
  const query = url.split("#", 1)[0]?.split("?", 2)[1] ?? "";
  const raw = new URLSearchParams(query).get("after_sequence");
  const parsed = raw == null ? 0 : Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

export function withAfterSequence(url: string, afterSequence: number): string {
  const hashIndex = url.indexOf("#");
  const hash = hashIndex >= 0 ? url.slice(hashIndex) : "";
  const withoutHash = hashIndex >= 0 ? url.slice(0, hashIndex) : url;
  const queryIndex = withoutHash.indexOf("?");
  const path = queryIndex >= 0 ? withoutHash.slice(0, queryIndex) : withoutHash;
  const params = new URLSearchParams(queryIndex >= 0 ? withoutHash.slice(queryIndex + 1) : "");
  params.set("after_sequence", String(Math.max(0, Math.trunc(afterSequence))));
  return `${path}?${params.toString()}${hash}`;
}

export function shouldReconnectRuntimeV2Stream(error: unknown): boolean {
  if (!(error instanceof Error)) return true;
  const status = error.message.match(/SSE request failed:\s*(\d{3})/)?.[1];
  if (!status) return true;
  const code = Number.parseInt(status, 10);
  return code === 408 || code === 425 || code === 429 || code >= 500;
}

// Closing an assistant view is a transport-only operation. The durable turn
// continues; only an explicit Stop may interrupt it.
export const ASSISTANT_VIEW_DETACH_REASON = "assistant_view_detach";
