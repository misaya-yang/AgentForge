import type { RequestBody, Defined, RequestQuery } from "@/api/contracts";
import type { SSEConnectionState } from "@/lib/sse";
import type { SdkApprovalBinding } from "@/features/chat/stream/sdkApprovalBinding";
/**
 * Assistant API client.
 *
 * Phase 1: Unified session + message + streaming protocol.
 * Provides access to the GPT-like assistant with multi-model support and KB integration.
 */

import { api } from "@/lib/api";
import type { AgentApprovalPreview } from "@/api/agentThreads";
import { SSEEventType, type SSEEventTypeValue } from "@/pages/assistant/sse-events";
import type { ModelCapabilityProfile } from "@/api/models";

export { SSEEventType };
export type { SSEEventTypeValue };

// =============================================================================
// Model and Dataset Types
// =============================================================================

export type ModelAccessLevel = "public" | "premium" | "admin";

export interface ModelInfo {
  id: string;
  name: string;
  provider: string;
  context_window: number;
  max_output_tokens: number;
  supports_vision: boolean;
  supports_tools: boolean;
  access_level?: ModelAccessLevel;
  input_price_per_1k?: number;
  output_price_per_1k?: number;
  effective_capabilities?: ModelCapabilityProfile;
  capability_revision?: number;
}

export interface DatasetInfo {
  dataset_id: string;
  name: string;
  description?: string | null;
  document_count: number;
  chunk_count: number;
  embedding_model?: string | null;
  is_multimodal: boolean;
}

export interface AssistantConfig {
  default_model_id: string;
  available_providers: string[];
  kb_enabled: boolean;
  web_search_enabled: boolean;
}

export interface AssistantMessage {
  role: string;
  content: string;
}

export type ChatRequest = RequestBody<"POST /api/v2/agent/threads/{thread}/turns"> & Pick<RequestBody<"POST /api/v2/agent/threads">, "session_id">;

export interface WebSearchResult {
  title: string;
  url: string;
  content: string;
  score: number;
}

export interface WebSearchResults {
  query: string;
  answer?: string;
  results: WebSearchResult[];
  response_time_ms: number;
}

export interface ChatResponse {
  content: string;
  usage: {
    input_tokens?: number;
    output_tokens?: number;
  };
  contexts: Array<{
    dataset_id: string;
    dataset_name: string;
    chunks: Array<{
      content: string;
      score: number;
      metadata?: Record<string, unknown>;
      source_url?: string;
    }>;
  }>;
  duration_ms: number;
  model_id: string;
  session_id?: string;
  run_id?: string;
}

export interface StreamEvent {
  event_type: string;
  data: unknown;
  timestamp: number;
}

export interface RetrievedContext {
  dataset_id: string;
  dataset_name: string;
  chunks: Array<{
    content: string;
    score: number;
    metadata?: Record<string, unknown>;
    source_url?: string;
  }>;
  query: string;
  took_ms: number;
}

// =============================================================================
// Phase 3: RAG Evaluation Types
// =============================================================================

export interface RAGCitation {
  citation_id: string;
  chunk_id: string;
  dataset_id: string;
  dataset_name: string;
  source_url?: string;
  source_title?: string;
  cited_text: string;
  context_preview: string;
  relevance_score: number;
  status: "used" | "implicit" | "unused";
}

export interface RAGQualityBreakdown {
  relevance: number;
  coverage: number;
  usage: number;
  citations: number;
}

export interface RAGEvaluation {
  quality_score: number;
  quality_breakdown: RAGQualityBreakdown;
  chunks_retrieved: number;
  chunks_used: number;
  response_grounding: number;
  citations: RAGCitation[];
  evaluation_time_ms: number;
}

// =============================================================================
// Code Execution Types
// =============================================================================

export interface CodeExecutionStart {
  execution_id: string;
  language: string;
  code: string;
}

export interface CodeExecutionOutput {
  execution_id: string;
  output: string;
}

export interface CodeExecutionResultData {
  execution_id: string;
  success: boolean;
  stdout: string;
  stderr: string;
  execution_time_ms: number;
  output_files: Array<{
    filename: string;
    content_type: string;
    size_bytes: number;
  }>;
}

export interface ArtifactCreated {
  artifact_id: string;
  type: "code" | "chart" | "table" | "file";
  format: string;
  title: string;
  url: string;
}

// =============================================================================
// Agentic Workflow Event Types
// =============================================================================

/**
 * Task planning event data - sent when agent creates an execution plan.
 */
export interface TaskPlanningEvent {
  goal: string;
  tasks: Array<{
    id: string;
    description: string;
    type: string;
    dependencies: string[];
  }>;
  parallel_groups: string[][];
}

/**
 * Working memory update event data - sent during task execution to report progress.
 */
export interface WorkingMemoryUpdateEvent {
  goal?: string;
  tasks: Array<{
    id: string;
    description: string;
    status: 'pending' | 'in_progress' | 'completed' | 'failed' | 'blocked';
    result?: string;
    error?: string;
  }>;
  collected_info: Array<{
    key: string;
    value: string;
    source: string;
  }>;
  notes: string[];
  progress: {
    total: number;
    completed: number;
    failed: number;
    percentage: number;
  };
}

/**
 * Memory loaded event data - sent when user preferences are loaded.
 */
export interface MemoryLoadedEvent {
  preferences_loaded: boolean;
}

/**
 * Tool error event data - sent when a tool execution fails.
 */
export interface ToolErrorEvent {
  tool_name: string;
  error_type: string;
  error_message: string;
  suggestion?: string;
  recoverable: boolean;
  context?: Record<string, unknown>;
}

/**
 * Cache metrics event data - KV-Cache performance metrics.
 */
export interface CacheMetricsEvent {
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
  input_tokens: number;
  output_tokens: number;
  cache_hit_rate?: number;
}

/**
 * Output warnings event data - validation warnings about the response.
 */

// =============================================================================
// AG-UI Protocol Event Data Types
// =============================================================================

/**
 * AG-UI Run Started Event - Agent execution begins
 */
export interface AGUIRunStartedEvent {
  run_id: string;
  timestamp: number;
}

/**
 * AG-UI Run Finished Event - Agent execution completed
 */
export interface AGUIRunFinishedEvent {
  run_id: string;
  timestamp: number;
}

/**
 * AG-UI Run Error Event - Agent execution failed
 */
export interface AGUIRunErrorEvent {
  run_id: string;
  error: string;
  code?: string;
  timestamp: number;
}

/**
 * AG-UI Step Started Event - Sub-task begins (Manus-style)
 */
export interface AGUIStepStartedEvent {
  step_id: string;
  title: string;
  description?: string;
  icon?: 'search' | 'web' | 'kb' | 'code' | 'image' | 'doc' | 'file' | 'brain' | 'ppt';
  parent_step_id?: string;
  timestamp: number;
}

/**
 * AG-UI Step Finished Event - Sub-task completed
 */
export interface AGUIStepFinishedEvent {
  step_id: string;
  status: 'completed' | 'failed' | 'skipped';
  result?: string;
  error?: string;
  duration_ms?: number;
  timestamp: number;
}

/**
 * AG-UI Tool Call Start Event
 */
export interface AGUIToolCallStartEvent {
  tool_call_id: string;
  tool_name: string;
  step_id?: string;  // Link to parent step
  timestamp: number;
}

/**
 * AG-UI Tool Call Args Event - Stream tool arguments
 */
export interface AGUIToolCallArgsEvent {
  tool_call_id: string;
  args_delta: string;  // Incremental JSON string
  timestamp: number;
}

/**
 * AG-UI Tool Call End Event
 */
export interface AGUIToolCallEndEvent {
  tool_call_id: string;
  timestamp: number;
}

/**
 * AG-UI Tool Call Result Event
 */
export interface AGUIToolCallResultEvent {
  tool_call_id: string;
  result: unknown;
  success: boolean;
  duration_ms?: number;
  timestamp: number;
}

/**
 * AG-UI State Snapshot Event - Full state
 */
export interface AGUIStateSnapshotEvent {
  state: Record<string, unknown>;
  timestamp: number;
}

/**
 * AG-UI State Delta Event - Incremental update (RFC6902 JSON Patch)
 */
export interface AGUIStateDeltaEvent {
  delta: Array<{
    op: 'add' | 'remove' | 'replace' | 'move' | 'copy' | 'test';
    path: string;
    value?: unknown;
    from?: string;
  }>;
  timestamp: number;
}

/**
 * Custom Event: File Creating
 */
export interface FileCreatingEvent {
  step_id?: string;
  filename: string;
  type: string;  // pptx, docx, pdf, etc.
  timestamp: number;
}

/**
 * Custom Event: File Created
 */
export interface FileCreatedEvent {
  step_id?: string;
  filename: string;
  type: string;
  size_bytes?: number;
  artifact_id?: string;
  download_url?: string;
  timestamp: number;
}

/**
 * Custom Event: Search Started
 */
export interface SearchStartedEvent {
  step_id?: string;
  search_id: string;
  query: string;
  source: 'kb' | 'web' | 'file';
  timestamp: number;
}

/**
 * Custom Event: Search Progress
 */
export interface SearchProgressEvent {
  search_id: string;
  progress: number;  // 0-100
  results_found?: number;
  timestamp: number;
}

/**
 * Custom Event: Search Completed
 */
export interface SearchCompletedEvent {
  search_id: string;
  results_count: number;
  duration_ms: number;
  timestamp: number;
}

// =============================================================================
// End AG-UI Protocol Event Data Types
// =============================================================================
export interface OutputWarningsEvent {
  warnings: Array<{
    type: string;
    message: string;
    severity: 'low' | 'medium' | 'high';
  }>;
}

// =============================================================================
// Phase 1 Optimization: Agent Loop Phase Events
// =============================================================================

/**
 * Error severity levels for structured error handling.
 */
export type ErrorSeverity = 'info' | 'warning' | 'error' | 'fatal';

/**
 * Agent Loop phase names.
 */
export type AgentLoopPhase =
  | 'memory_loading'
  | 'scenario_analysis'
  | 'task_planning'
  | 'rag_retrieval'
  | 'context_building'
  | 'execution'
  | 'context_compression'
  | 'generation_storage';

/**
 * Phase started event - emitted when an AgentLoop phase begins.
 */
export interface PhaseStartedEvent {
  phase_index: number;      // 1-8
  total_phases: number;     // 8
  phase_name: AgentLoopPhase;
  display_name: string;     // Chinese display name
  status: 'started';
  timestamp?: number;
}

/**
 * Phase completed event - emitted when an AgentLoop phase finishes.
 */
export interface PhaseCompletedEvent {
  phase_index: number;
  total_phases: number;
  phase_name: AgentLoopPhase;
  display_name: string;
  status: 'completed';
  duration_ms: number;
  timestamp?: number;
}

/**
 * Run started event with task_id for cancellation tracking.
 */
export interface RunStartedWithTaskEvent {
  session_id: string;
  task_id: string | null;   // Used for cancellation
  request_id: string;
  timestamp?: number;
}

/**
 * Structured error event - provides detailed error information.
 */
export interface StructuredErrorEvent {
  code: string;
  message: string;
  severity: ErrorSeverity;
  recoverable: boolean;
  phase?: AgentLoopPhase;
  suggestion?: string;
  details?: Record<string, unknown>;
  timestamp?: number;
}

/**
 * Cancelled event - emitted when a task is cancelled by user.
 */
export interface CancelledEvent {
  reason: string;
  phase?: AgentLoopPhase;
  timestamp?: number;
}

// =========================================================================
// Session Types
// =========================================================================

export interface AssistantSession {
  session_id: string;
  user_id: string;
  tenant_id: string;
  service_id?: string;
  created_at?: string;
  updated_at?: string;
  metadata?: {
    title?: string;
    [key: string]: unknown;
  };
  message_count: number;
}

export interface SessionHistoryMessage {
  role: string;
  content: string;
  timestamp?: string;
  metadata?: Record<string, unknown>;
}

export interface SessionHistoryResponse {
  session_id: string;
  messages: SessionHistoryMessage[];
  total: number;
}

/**
 * List available LLM models.
 */
export async function listModels(options?: { signal?: AbortSignal }): Promise<ModelInfo[]> {
  const { data } = await api.get<{ models: ModelInfo[] }>("/api/v1/assistant/models", { signal: options?.signal });
  return data.models;
}

/**
 * List available knowledge base datasets.
 */
export async function listDatasets(): Promise<DatasetInfo[]> {
  const { data } = await api.get<{ datasets: DatasetInfo[] }>("/api/v1/assistant/datasets");
  return data.datasets;
}

/**
 * Get assistant configuration.
 */
export async function getConfig(options?: { signal?: AbortSignal }): Promise<AssistantConfig> {
  const { data } = await api.get<AssistantConfig>("/api/v1/assistant/config", { signal: options?.signal });
  return data;
}

/**
 * Streaming chat completion (SSE).
 *
 * Returns an async generator that yields StreamEvent objects.
 * Event types:
 * - context_retrieved: KB search results
 * - text_delta: Incremental text content
 * - tool_call: Tool invocation
 * - usage: Token usage statistics
 * - done: Stream completion
 * - error: Error occurred
 */
export async function* chatStream(
  request: ChatRequest,
  signal?: AbortSignal,
  options: { onConnectionState?: (state: SSEConnectionState) => void } = {},
): AsyncGenerator<StreamEvent, void, void> {
  const { streamAgentRuntimeV2 } = await import("@/api/agentThreads");
  yield* streamAgentRuntimeV2(request, signal, options);
}

/**
 * Group models by provider.
 */
export function groupModelsByProvider(models: ModelInfo[]): Record<string, ModelInfo[]> {
  const grouped: Record<string, ModelInfo[]> = {};
  for (const model of models) {
    if (!grouped[model.provider]) {
      grouped[model.provider] = [];
    }
    grouped[model.provider].push(model);
  }
  return grouped;
}

/**
 * Get provider display name.
 */
export function getProviderDisplayName(provider: string): string {
  const names: Record<string, string> = {
    openai: "OpenAI",
    anthropic: "Anthropic",
    deepseek: "DeepSeek",
    dashscope: "Qwen/DashScope",
    google: "Google Gemini",
    // Vertex is wired as a separate provider entry (same wire protocol,
    // different host + key format). Labeled distinctly so users can tell
    // a model routed through AI Studio apart from the Vertex mirror.
    "google-vertex": "Google Vertex AI",
  };
  return names[provider] || provider;
}


// =========================================================================
// Session Management APIs
// =========================================================================




/**
 * Delete a session.
 */
export async function deleteSession(sessionId: string): Promise<void> {
  await api.delete(`/api/v1/assistant/sessions/${sessionId}`);
}

/**
 * Get session message history.
 */
export async function getSessionHistory(
  sessionId: string,
  limit = 100
): Promise<SessionHistoryResponse> {
  const { data } = await api.get<SessionHistoryResponse>(
    `/api/v1/assistant/sessions/${sessionId}/history?limit=${limit}`
  );
  return data;
}



// =========================================================================
// Artifacts API
// =========================================================================

export interface ArtifactInfo {
  artifact_id: string;
  session_id: string;
  message_id?: string;
  tenant_id: string;
  user_id: string;
  type: "image" | "document" | "chart" | "code" | "file";
  format: string;
  title: string;
  filename: string;
  storage_key: string;
  size_bytes: number;
  ready?: boolean;
  variant?: string;
  parent_artifact_id?: string | null;
  mime_type?: string;
  source: "ai" | "user" | "code_execution" | "image_generation" | "document_generation";
  metadata?: Record<string, unknown>;
  download_url?: string;
  created_at: string;
  updated_at: string;
}

/**
 * Get artifacts for a session.
 */
export async function getSessionArtifacts(sessionId: string, signal?: AbortSignal): Promise<ArtifactInfo[]> {
  const { data } = await api.get<{ artifacts: ArtifactInfo[]; total: number }>(
    `/api/v1/assistant/sessions/${sessionId}/artifacts`, { signal }
  );
  return data.artifacts;
}



/**
 * Create an artifact from base64 content.
 * Used for saving generated images, documents, etc.
 */
export type CreateArtifactRequest = Defined<RequestBody<"POST /api/v1/assistant/artifacts">, "session_id" | "type" | "format" | "title" | "filename" | "content_base64">;

export async function createArtifact(request: CreateArtifactRequest): Promise<ArtifactInfo> {
  const { data } = await api.post<ArtifactInfo>("/api/v1/assistant/artifacts", (request satisfies RequestBody<"POST /api/v1/assistant/artifacts">));
  return data;
}

/**
 * Get artifact download URL (redirects to presigned URL).
 */
export function getArtifactDownloadUrl(artifactId: string): string {
  // This URL will redirect to the actual presigned URL
  return `/api/v1/assistant/artifacts/${artifactId}/download`;
}


// =========================================================================
// Conversation Sharing API
// =========================================================================

export interface ShareInfo {
  share_code: string;
  share_url: string;
  title: string | null;
  message_count: number;
  artifact_count: number;
  created_at: string;
  expires_at: string | null;
  audience: "public" | "internal";
}

export interface ConversationSharePreview {
  title: string;
  messages: Array<{ role: "user" | "assistant"; content: string; quiz_data?: unknown }>;
  artifacts: Array<{ artifact_id: string; title: string; filename: string; type: string }>;
  message_count: number;
  artifact_count: number;
  audience: "public" | "internal";
  expires_days: number | null;
  preview_hash: string;
}

export interface ExistingConversationShare extends ShareInfo {
  session_id: string;
  is_active: boolean;
  view_count: number;
}

export async function previewConversationShare(
  sessionId: string,
  options: RequestQuery<"GET /api/v1/assistant/sessions/{session_id}/share-preview">,
): Promise<ConversationSharePreview> {
  const { data } = await api.get<ConversationSharePreview>(
    `/api/v1/assistant/sessions/${sessionId}/share-preview`,
    { params: (options satisfies RequestQuery<"GET /api/v1/assistant/sessions/{session_id}/share-preview">) },
  );
  return data;
}

export async function listConversationShares(sessionId: string): Promise<ExistingConversationShare[]> {
  const { data } = await api.get<{ shares: ExistingConversationShare[] }>("/api/v1/assistant/shares", {
    params: ({ session_id: sessionId } satisfies RequestQuery<"GET /api/v1/assistant/shares">),
  });
  return data.shares;
}

export async function revokeConversationShare(shareCode: string): Promise<void> {
  await api.delete(`/api/v1/assistant/shares/${shareCode}`);
}

export async function createConversationShare(
  sessionId: string,
  options: RequestBody<"POST /api/v1/assistant/sessions/{session_id}/share">
): Promise<ShareInfo> {
  const { data } = await api.post<ShareInfo>(
    `/api/v1/assistant/sessions/${sessionId}/share`,
    (options satisfies RequestBody<"POST /api/v1/assistant/sessions/{session_id}/share">),
  );
  return data;
}





// =============================================================================
// Run lifecycle + tool approval
// =============================================================================

export type ToolApprovalRequest = Pick<RequestBody<"POST /api/v1/assistant/approvals/{approval_id}">, "approved" | "reason"> & (SdkApprovalBinding | {
  expected_run_id?: never;
  expected_tool_call_id?: never;
  expected_arguments_hash?: never;
});

export interface ToolApprovalResponse {
  approval: {
    approval_id: string;
    tool_call_id?: string;
    status: string;
    tool_name?: string;
    run_id?: string;
    reason?: string | null;
  };
}

export type RunResumeRequest = RequestBody<"POST /api/v1/assistant/runs/{run_id}/resume">;

export interface RunResumeResponse {
  resume: {
    run_id: string;
    status: "ready" | "blocked";
    reason?: string | null;
    recoverable?: boolean;
    checkpoint?: Record<string, unknown>;
  };
}

export interface AssistantRunCheckpoint {
  phase?: string;
  approval_id?: string | null;
  pending_tool?: Record<string, unknown> | null;
}

export interface AssistantRunStatus {
  run_id?: string;
  session_id?: string;
  status?: string;
  engine?: "agent_runtime" | "openai_agents" | "claude_agent_sdk";
  terminal_reason?: "runtime_restart_interrupted" | "sdk_authority_expired" | null;
  /** Observed usage only; SDK accounting_status never asserts a settled charge. */
  usage?: Record<string, unknown> | null;
  harness_thread_id?: string | null;
  harness_turn_id?: string | null;
  checkpoint?: AssistantRunCheckpoint | null;
}

/** Authenticated action details for native SDK and the existing Runtime approval path. */
export async function getAssistantToolApproval(approvalId: string): Promise<ToolApprovalResponse & { preview: AgentApprovalPreview }> {
  const { data } = await api.get<ToolApprovalResponse & { preview: AgentApprovalPreview }>(
    `/api/v1/assistant/approvals/${encodeURIComponent(approvalId)}`,
  );
  return data;
}

export async function approveToolCall(
  approvalId: string,
  body: ToolApprovalRequest,
): Promise<ToolApprovalResponse> {
  const { data } = await api.post<ToolApprovalResponse>(
    `/api/v1/assistant/approvals/${encodeURIComponent(approvalId)}`,
    (body satisfies RequestBody<"POST /api/v1/assistant/approvals/{approval_id}">),
  );
  return data;
}

export async function getAssistantRunStatus(runId: string, options?: { signal?: AbortSignal; timeout?: number }): Promise<{ run: AssistantRunStatus }> {
  const { data } = await api.get<{ run: AssistantRunStatus }>(
    `/api/v1/assistant/runs/${encodeURIComponent(runId)}`,
    options,
  );
  return data;
}

export async function prepareAssistantRunResume(
  runId: string,
  body: RunResumeRequest = {},
): Promise<RunResumeResponse> {
  const { data } = await api.post<RunResumeResponse>(
    `/api/v1/assistant/runs/${runId}/resume`,
    (body satisfies RequestBody<"POST /api/v1/assistant/runs/{run_id}/resume">),
  );
  return data;
}

// =========================================================================
// Image Generation API (Smart Routing)
// =========================================================================

export type ImageGenerationRequest = Defined<RequestBody<"POST /api/v1/assistant/generate-image">, "prompt" | "model_id">;

export interface GeneratedImage {
  url: string;
  artifact_id?: string;
  width?: number;
  height?: number;
}

export interface ImageGenerationResponse {
  success: boolean;
  images: GeneratedImage[];
  provider: string;
  effective_model_id?: string | null;
  duration_ms?: number;
  session_id?: string;
  output_artifact_id?: string;
  error?: string;
  error_code?: string;
  task_id?: string;
}

export interface ImageTaskObservation {
  taskId: string;
  sessionId: string;
  status: string;
  progress: number | null;
  effectiveModel: string | null;
}

/** Read a known task receipt. This does not poll the provider or repeat generation. */
export async function readImageTask(taskId: string, sessionId: string, turnId?: string, signal?: AbortSignal): Promise<ImageTaskObservation> {
  if (!/^imt_[0-9a-f]{20}$/i.test(taskId) || !sessionId.trim()) throw new Error("Invalid image task identity");
  const { data } = await api.get<unknown>(`/api/v1/assistant/image-task/${encodeURIComponent(taskId)}`, { signal });
  if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error("Invalid image task receipt");
  const row = data as Record<string, unknown>;
  if (row.task_id !== taskId || row.session_id !== sessionId || (turnId && row.turn_id !== turnId)
    || typeof row.status !== "string" || !row.status.trim()) throw new Error("Unconfirmed image task receipt");
  return { taskId, sessionId, status: row.status,
    progress: typeof row.progress === "number" && Number.isFinite(row.progress) && row.progress >= 0 && row.progress <= 100 ? row.progress : null,
    effectiveModel: typeof row.effective_model_id === "string" && row.effective_model_id.trim() ? row.effective_model_id : null };
}

/**
 * Generate images with smart routing based on current model provider.
 *
 * Routes to:
 * - DashScope Wanx for DashScope models (Qwen)
 * - Gemini Native Image for Google models
 * - Fallback to DashScope for other providers
 */
export async function generateImage(request: ImageGenerationRequest): Promise<ImageGenerationResponse> {
  const { data } = await api.post<ImageGenerationResponse>("/api/v1/assistant/generate-image", (request satisfies RequestBody<"POST /api/v1/assistant/generate-image">));
  return data;
}


// =========================================================================
// Task Cancellation API (Phase 1 Optimization)
// =========================================================================

export type TaskCancelRequest = RequestBody<"POST /api/v1/assistant/tasks/{task_id}/cancel">;

export interface TaskCancelResponse {
  task_id: string;
  session_id: string;
  cancelled: boolean;
  message: string;
}

/**
 * Cancel a running assistant task.
 *
 * @param taskId - The task ID from the run_started event
 * @param reason - Optional cancellation reason
 * @returns TaskCancelResponse with cancellation status
 */
export async function cancelTask(taskId: string, reason?: string, options?: { signal?: AbortSignal; timeout?: number }): Promise<TaskCancelResponse> {
  const body: TaskCancelRequest = reason ? { reason } : {};
  const { data } = await api.post<TaskCancelResponse>(
    `/api/v1/assistant/tasks/${encodeURIComponent(taskId)}/cancel`,
    (body satisfies RequestBody<"POST /api/v1/assistant/tasks/{task_id}/cancel">),
    options
  );
  return data;
}
