import type { RequestQuery, RequestBody } from "@/api/contracts";
import type { TokenProvenance } from "@/features/chat/stream/tokenObservation";
import { api } from "@/lib/api";

// 会话配置（知识库、模型等）
export type SessionConfig = NonNullable<RequestBody<"PATCH /api/v1/sessions/{session_id}">["config"]> & {
  selected_model?: string;
  selected_provider?: string;
  selected_datasets?: string[];
  web_search_enabled?: boolean;
  thinking_level?: "off" | "low" | "medium" | "high";
  reasoning_option?: string;
  temperature?: number;
  selected_style?: string;
  execution_profile?: "safe" | "balanced" | "power";
  memory_mode?: "auto" | "strict" | "off";
  os_agent_enabled?: boolean;
  local_node_device_id?: string;
  local_node_grant_ids?: string[];
};

export interface SessionSummary {
  session_id: string;
  user_id: string;
  tenant_id: string;
  service_id?: string | null;
  created_at: string;
  updated_at?: string | null;
  metadata?: Record<string, unknown> | null;
  config?: SessionConfig | null;
}

export interface SessionMessageToolCall {
  tool_call_id: string;
  name: string;
  arguments: string;
  result?: string | null;
  status?: "pending" | "running" | "completed" | "error";
}

// Context chunk from KB retrieval
export interface SessionContextChunk {
  content: string;
  metadata?: Record<string, unknown>;
  score?: number;
  document_id?: string;
  document_name?: string;
}

// Retrieved context from a dataset
export interface SessionRetrievedContext {
  dataset_id: string;
  dataset_name: string;
  chunks: SessionContextChunk[];
  query: string;
  took_ms: number;
  avg_score?: number;
  top_score?: number;
}

export type SessionMessageMetadata = NonNullable<RequestBody<"POST /api/v1/sessions/{session_id}/messages">["metadata"]> & {
  // Gateway's local stream observation; interrupted does not prove a remote stop.
  gateway_stream_status?: "completed" | "failed" | "interrupted";
  // Local stream observation only; run inspection remains the durable authority.
  sdk_stream_status?: "completed" | "failed" | "interrupted";
  source_access_revoked?: boolean;
  tool_calls?: SessionMessageToolCall[];
  stats?: {
    reported_routing?: unknown;
    input_tokens?: number;
    output_tokens?: number;
    total_tokens?: number;
    duration_ms?: number;
    first_token_ms?: number;
    // Playground display provenance only; never an accounting/settlement receipt.
    token_usage_source?: "reported" | "unknown";
    token_provenance?: TokenProvenance;
    text_token_estimate?: {
      input_tokens: number;
      output_tokens: number;
      total_tokens: number;
    };
  };
  // KB retrieval contexts
  contexts?: SessionRetrievedContext[] | null;
  // Model and usage info
  model_id?: string;
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
  };
  // User message attachments (files uploaded with the message)
  attachments?: Array<{
    type: "image" | "file";
    url: string;
    filename: string;
  }>;
  // Image-generation artifacts referenced by the assistant message
  artifact_ids?: string[];
  source_kind?: "image_generation";
  process_summary?: Record<string, unknown>;
};

export interface SessionMessage {
  role: "user" | "assistant" | string;
  content: unknown;
  timestamp: string;
  metadata?: SessionMessageMetadata | null;
}

export async function listSessions(params?: RequestQuery<"GET /api/v1/sessions">): Promise<SessionSummary[]> {
  // Use generic sessions endpoint which returns an array
  const { data } = await api.get<SessionSummary[]>("/api/v1/sessions", { params: ((params ?? {}) satisfies RequestQuery<"GET /api/v1/sessions">) });
  return data;
}

export async function createSession(body?: {
  session_id?: string;
  service_id?: string;
  metadata?: Record<string, unknown>;
  config?: SessionConfig;
}): Promise<{ session_id: string }> {
  // Use generic sessions endpoint
  const { data } = await api.post<{ session_id: string }>("/api/v1/sessions", (body || {} satisfies RequestBody<"POST /api/v1/sessions">));
  return data;
}

export async function getSession(sessionId: string): Promise<SessionSummary> {
  const { data } = await api.get<SessionSummary>(`/api/v1/sessions/${sessionId}`);
  return data;
}

export async function deleteSession(sessionId: string): Promise<void> {
  await api.delete(`/api/v1/sessions/${sessionId}`);
}

export async function updateSession(
  sessionId: string,
  body: RequestBody<"PATCH /api/v1/sessions/{session_id}">
): Promise<void> {
  await api.patch(`/api/v1/sessions/${sessionId}`, (body satisfies RequestBody<"PATCH /api/v1/sessions/{session_id}">));
}

export async function addSessionMessage(
  sessionId: string,
  body: RequestBody<"POST /api/v1/sessions/{session_id}/messages">
): Promise<void> {
  await api.post(`/api/v1/sessions/${sessionId}/messages`, (body satisfies RequestBody<"POST /api/v1/sessions/{session_id}/messages">));
}

export async function getSessionHistory(
  sessionId: string,
  params?: RequestQuery<"GET /api/v1/sessions/{session_id}/history">
): Promise<SessionMessage[]> {
  const { data } = await api.get<SessionMessage[]>(`/api/v1/sessions/${sessionId}/history`, {
    params: ((params ?? {}) satisfies RequestQuery<"GET /api/v1/sessions/{session_id}/history">),
  });
  return data;
}
