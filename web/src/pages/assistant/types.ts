/**
 * Type definitions for the Assistant page components.
 *
 * Phase 1: Unified session + message + streaming protocol.
 * - Enhanced message structure with tool_calls/tool_results/citations/attachments
 * - Standard SSE event types
 */

import type { FileUploadResponse } from "@/api/files";
import { SSEEventType, type SSEEventTypeValue } from "./sse-events";
import type { ChatTurnState } from "@/features/chat/stream";

export { SSEEventType };
export type { SSEEventTypeValue };

// =============================================================================
// Tool Call Structures (for Agentic workflows)
// =============================================================================

export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: "pending" | "running" | "completed" | "error";
}

export interface ToolResult {
  tool_call_id: string;
  name: string;
  result: unknown;
  error?: string;
  duration_ms?: number;
}

export type ProcessStepStatus = "pending" | "running" | "completed" | "failed";
export type ProcessToolStatus =
  | "pending"
  | "running"
  | "completed"
  | "error"
  | "approval_required";

export interface ProcessStepItem {
  id: string;
  title: string;
  description?: string;
  status: ProcessStepStatus;
  startedAt?: number;
  finishedAt?: number;
  durationMs?: number;
  error?: string;
}

export interface ToolTimelineItem {
  id: string;
  name: string;
  status: ProcessToolStatus;
  startedAt?: number;
  finishedAt?: number;
  durationMs?: number;
  summary?: string;
  error?: string;
  queueState?: string;
  approvalId?: string;
}

export interface ProcessSummaryState {
  collapsed: boolean;
  runId?: string;
  runtimeThreadId?: string;
  status: "running" | "succeeded" | "failed" | "cancelled" | "blocked";
  terminalReason?: "runtime_restart_interrupted";
  startedAt?: number;
  totalDurationMs?: number;
  currentStep?: string;
  isErrorExpanded?: boolean;
  steps: ProcessStepItem[];
  tools: ToolTimelineItem[];
  // Thinking phase timing
  thinkingStartedAt?: number;
  thinkingDurationMs?: number;
  contextBudget?: {
    used_tokens?: number;
    model_context_window?: number;
    dropped_history_messages?: number;
  };
  contextCompacted?: {
    compacted?: boolean;
    dropped_history_messages?: number;
  };
  reasoning?: {
    requested_option?: string;
    effective_option?: string;
    canonical_effort?: string | null;
    adapter_id?: string;
    capability_revision?: number;
    fallback_reason?: string | null;
  };
  modelId?: string;
  temperature?: number;
}

// =============================================================================
// Citation Structures (for RAG traceability)
// =============================================================================

export interface Citation {
  dataset_id: string;
  document_id?: string;
  chunk_id?: string;
  source_url?: string;
  title?: string;
  score?: number;
  content_preview?: string;
}

export interface WebSearchResult {
  title: string;
  url: string;
  content: string;
  score: number;
}

// =============================================================================
// Phase 3: Enhanced Citation with Status
// =============================================================================

export type CitationStatus = "used" | "implicit" | "unused";

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
  status: CitationStatus;
}

export interface RAGQualityBreakdown {
  relevance: number;  // 0-25
  coverage: number;   // 0-25
  usage: number;      // 0-25
  citations: number;  // 0-25
}

export interface RAGEvaluation {
  quality_score: number;  // 0-100
  quality_breakdown: RAGQualityBreakdown;
  chunks_retrieved: number;
  chunks_used: number;
  response_grounding: number;  // 0-1
  citations: RAGCitation[];
  evaluation_time_ms: number;
}

// =============================================================================
// KV-Cache Metrics
// =============================================================================

export interface CacheMetrics {
  layer1_hit: boolean;
  layer2_hit: boolean;
  total_input_tokens: number;
  cached_tokens: number;
  cache_hit_rate: number;
  estimated_savings_usd: number;
  system_prefix_hash: string;
}

// =============================================================================
// Attachment Structures (for multimodal input)
// =============================================================================

export interface MessageAttachment {
  id: string;
  type: "image" | "file" | "audio";
  filename?: string;
  url?: string;
  mime_type?: string;
  size_bytes?: number;
}

// =============================================================================
// Context Structures (for RAG display)
// =============================================================================

export interface RetrievedChunk {
  content: string;
  score: number;
  dataset_id?: string;
  document_id?: string;
  source_version?: number;
  source_hash?: string;
  metadata?: Record<string, unknown>;
  source_url?: string;
  image_url?: string;
}

export interface RetrievedContext {
  dataset_id: string;
  dataset_name: string;
  chunks: RetrievedChunk[];
  query: string;
  took_ms: number;
}

// =============================================================================
// Search Status (for GPT-like "Searching..." display)
// =============================================================================

export type SearchType = "kb" | "web" | "files";
export type SearchState = "searching" | "completed" | "error";

export interface SearchStatusItem {
  type: SearchType;
  state: SearchState;
  query?: string;
  datasets?: string[];
  resultCount?: number;
  durationMs?: number;
  error?: string;
}

// =============================================================================
// Generated Artifact (for inline document preview in messages)
// =============================================================================

export interface GeneratedArtifact {
  id: string;
  type: "document" | "image" | "code" | "file";
  format: string;
  title: string;
  url: string;
  filename?: string;
  mimeType?: string;
  sizeBytes?: number;
  content?: string;  // For markdown preview
}

export interface ArtifactWorkspaceGroup<T = GeneratedArtifact> {
  id: "current_run" | "session_artifacts";
  title: string;
  items: T[];
}

export interface AssistantUiV2Flags {
  enabled: boolean;
  mobileBottomSheet: boolean;
  framelessConversation: boolean;
}

// =============================================================================
// Enhanced Chat Message (Phase 1)
// =============================================================================

export interface ChatMessage {
  runtimeRunId?: string;
  id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  createdAt?: string;
  parts?: Array<{
    id: string;
    type: "text" | "tool_call" | "tool_result";
    content: string;
    createdAt: string;
  }>;
  status?: ChatTurnState;
  outcomeUncertain?: boolean;
  diagnosticId?: string;
  sourceAccessRevoked?: boolean;
  meta?: Record<string, unknown>;

  // Streaming state
  isStreaming?: boolean;

  // Image generation state (GPT-style)
  isGeneratingImage?: boolean;
  imageTaskId?: string;
  imageTurnId?: string;
  imageGenerationPrompt?: string;

  // Search status (for GPT-like "Searching..." display)
  searchStatus?: SearchStatusItem[];

  // Agent phase status (for ReAct thinking display)
  agentPhase?: AgentPhaseStatus;

  // Pre-tool thinking content (displayed as collapsible "Thought Process")
  thinkingContent?: string;
  // Streaming thinking state (real-time thinking display)
  streamingThinkingContent?: string;
  isThinkingStreaming?: boolean;

  // Internal: artifact IDs from session metadata (used by hydrateMessageArtifacts)
  _artifactIds?: string[];

  // Sub-agents spawned during this message's execution
  activeSubAgents?: SubAgentState[];

  // Agentic extensions
  toolCalls?: ToolCall[];
  toolResults?: ToolResult[];
  processSummary?: ProcessSummaryState;

  // RAG context
  contexts?: RetrievedContext[];
  citations?: Citation[];

  // Phase 3: RAG evaluation and enhanced citations
  ragEvaluation?: RAGEvaluation;
  ragCitations?: RAGCitation[];

  // KV-Cache metrics
  cacheMetrics?: CacheMetrics;

  // Web search
  webSearchResults?: WebSearchResult[];

  // Multimodal
  attachments?: MessageAttachment[];

  // Quiz data (rendered as interactive card in chat)
  quizData?: QuizData;
  quizResult?: QuizAttemptResult;

  // Generated artifacts (documents, images, etc.)
  generatedArtifacts?: GeneratedArtifact[];

  // Metadata
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    total_tokens?: number;
  };
  durationMs?: number;
  /** First user-visible reasoning, tool, or text token. */
  firstTokenMs?: number;
  /** First final-answer text token, kept separate from visible reasoning TTFT. */
  firstTextTokenMs?: number;
  modelId?: string;
  timestamp?: string;
}

// =============================================================================
// Quiz Types
// =============================================================================

export interface QuizOption {
  label: string;
  text: string;
}

export interface QuizQuestionData {
  id: string;
  question_num: number;
  question_type: string;
  question_text: string;
  options: QuizOption[];
}

export interface QuizData {
  quiz_id: string;
  title: string;
  description?: string;
  topic?: string;
  difficulty?: string;
  question_count: number;
  questions: QuizQuestionData[];
}

export interface QuizAttemptResult {
  attempt_id: string;
  total_score: number;
  correct_count: number;
  total_count: number;
  per_question: Array<{
    question_num: number;
    question_id: string;
    correct: boolean;
    user_answer: string;
    correct_answer: string;
    explanation?: string;
  }>;
}

// =============================================================================
// SSE Event Data Types
// =============================================================================

export interface ContextRetrievedEventData {
  dataset_id: string;
  dataset_name: string;
  chunks: RetrievedChunk[];
  query: string;
  took_ms: number;
}

export interface ToolCallEventData {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: string;
}

export interface ToolResultEventData {
  tool_call_id: string;
  name: string;
  result: unknown;
  error?: string;
  duration_ms?: number;
}

export interface UsageEventData {
  input_tokens?: number;
  output_tokens?: number;
  total_tokens?: number;
  cached_tokens?: number;
}

export interface DoneEventData {
  session_id: string;
  total_length: number;
  duration_ms: number;
  model_id: string;
  kb_datasets_used: string[];
  context_truncated: boolean;
  // Phase 3: RAG quality summary
  rag_quality?: number;
  citations_count?: number;
}

// Phase 3: RAG Evaluation Event Data
export interface RAGEvaluationEventData {
  quality_score: number;
  quality_breakdown: RAGQualityBreakdown;
  chunks_retrieved: number;
  chunks_used: number;
  response_grounding: number;
  citations: RAGCitation[];
  evaluation_time_ms: number;
}

// KV-Cache metrics event data
export interface CacheMetricsEventData {
  layer1_hit: boolean;
  layer2_hit: boolean;
  total_input_tokens: number;
  cached_tokens: number;
  cache_hit_rate: number;
  estimated_savings_usd: number;
  system_prefix_hash: string;
}

export interface ErrorEventData {
  message: string;
  code?: string;
  recoverable: boolean;
}

// File processed event data (after backend processes uploaded files)
export interface FileProcessedEventData {
  image_count: number;
  text_length: number;
  description_count: number;
  requires_rag: boolean;
  file_count?: number;
  file_metadata?: Array<{
    file_path: string;
    file_type?: string;
    error?: string;
  }>;
}

export interface ContextBudgetEventData {
  used_tokens?: number;
  model_context_window?: number;
  dropped_history_messages?: number;
}

export interface ContextCompactedEventData {
  compacted?: boolean;
  dropped_history_messages?: number;
}

export interface QueueStateEventData {
  tool_id?: string;
  tool_name?: string;
  command_id?: string;
  state?: string;
}

export interface ApprovalRequiredEventData {
  tool_id?: string;
  tool_name?: string;
  approval_id?: string;
  reason?: string;
}

export interface ApprovalResultEventData {
  tool_id?: string;
  approval_id?: string;
  approved?: boolean;
  reason?: string;
}

export interface GatewayDecisionEventData {
  tool_id?: string;
  tool_name?: string;
  policy_profile?: string;
  decision?: string;
  reason?: string;
}

// Task planning event data - sent when agent creates an execution plan
export interface TaskPlanningEventData {
  goal: string;
  tasks: Array<{
    id: string;
    description: string;
    type: string;
    dependencies: string[];
  }>;
  parallel_groups: string[][];
}

// Working memory update event data - sent during task execution to report progress
export interface WorkingMemoryUpdateEventData {
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

// Memory loaded event data - sent when user preferences are loaded
export interface MemoryLoadedEventData {
  preferences_loaded: boolean;
}

// Tool error event data - sent when a tool execution fails
export interface ToolErrorEventData {
  tool_name: string;
  error_type: string;
  error_message: string;
  suggestion?: string;
  recoverable: boolean;
  context?: Record<string, unknown>;
}

// Output warnings event data - validation warnings about the response
export interface OutputWarningsEventData {
  warnings: Array<{
    type: string;
    message: string;
    severity: 'low' | 'medium' | 'high';
  }>;
}

export interface StreamEvent {
  event_type: SSEEventTypeValue;
  data: unknown;
  timestamp: number;
}

// =============================================================================
// File Upload Types
// =============================================================================

export interface UploadedFile {
  file: File;
  status: "pending" | "compressing" | "uploading" | "success" | "error";
  response?: FileUploadResponse;
  error?: string;
  progress?: number;
}

// =============================================================================
// UI Helper Types
// =============================================================================

export interface SuggestedPrompt {
  icon: React.ReactNode;
  titleKey: string;
  promptKey: string;
  categoryKey: string;
}

export interface SessionSummary {
  session_id: string;
  title?: string;
  message_count: number;
  created_at?: string;
  updated_at?: string;
  last_message_preview?: string;
}

// =============================================================================
// Code Execution State (for Artifacts Panel)
// =============================================================================

export interface OutputFile {
  filename: string;
  content_base64: string;
  mime_type: string | null;
  size_bytes: number;
}

export interface CodeExecutionState {
  isExecuting: boolean;
  executionId: string | null;
  code: string | null;
  output: string;
  executionTimeMs: number | null;
  status: "idle" | "running" | "success" | "error" | "timeout";
  outputFiles: OutputFile[];
}

// =============================================================================
// Artifact Data (for Artifacts Panel)
// =============================================================================

export interface ArtifactData {
  id: string;
  type: "code" | "chart" | "table" | "file";
  format: string;
  title: string;
  url?: string;
  content?: string;
  createdAt: Date;
}

// =============================================================================
// Sub-Agent Types (ADR-003)
// =============================================================================

export const SUB_AGENT_TYPES = ["explore", "task", "plan"] as const;
export type SubAgentType = (typeof SUB_AGENT_TYPES)[number];

export const SUB_AGENT_STATUSES = [
  "running",
  "completed",
  "failed",
  "cancelled",
  "blocked",
  "partial",
] as const;
export type SubAgentStatus = (typeof SUB_AGENT_STATUSES)[number];

export function isSubAgentType(value: unknown): value is SubAgentType {
  return typeof value === "string" && SUB_AGENT_TYPES.some((agentType) => agentType === value);
}

export function isSubAgentStatus(value: unknown): value is SubAgentStatus {
  return typeof value === "string" && SUB_AGENT_STATUSES.some((status) => status === value);
}

export interface SubAgentStep {
  toolName: string;
  callId: string;
  status: SubAgentStatus;
  summary?: string;
  durationMs?: number;
}

export interface SubAgentEvidence {
  evidenceId?: string;
  toolName?: string;
  callId?: string;
  status?: string;
  summary?: string;
}

export interface SubAgentState {
  agentId: string;
  agentType: SubAgentType;
  description: string;
  status: SubAgentStatus;
  /** Host-selected profile identity. This is display metadata, not model reasoning. */
  profileId?: string;
  profileName?: string;
  sourcePlugin?: string;
  definitionSha256?: string;
  delegationId?: string;
  taskId?: string;
  parentTaskId?: string;
  dispatchIndex?: number;
  attemptId?: string;
  depth?: number;
  lineage?: string[];
  currentStep?: string;
  currentStepStatus?: SubAgentStatus;
  turnsCompleted?: number;
  toolCallsMade?: number;
  steps: SubAgentStep[];
  resultSummary?: string;
  evidence?: SubAgentEvidence[];
  limitations?: string[];
  usage?: Record<string, number>;
  structuredResult?: unknown;
  effectiveExecution?: {
    modelId?: string;
    toolNames?: string[];
    toolCategories?: string[];
    extensions?: number;
    stopReason?: string;
  };
  error?: string;
  durationMs?: number;
  /** Client receipt timestamps used only for elapsed/completed UI labels. */
  startedAtMs?: number;
  finishedAtMs?: number;
  startedMonotonicMs?: number;
  finishedMonotonicMs?: number;
}

// =============================================================================
// ReAct Phase Types (for Agent thinking status display)
// =============================================================================

export type ReActPhase =
  | "analyzing"    // Initial task analysis
  | "thinking"     // Reasoning about next step
  | "planning"     // Creating execution plan
  | "executing"    // Running a tool
  | "observing"    // Analyzing tool result
  | "writing"      // Generating content
  | "completing";  // Finishing up

export interface AgentPhaseStatus {
  phase: ReActPhase;
  message: string;
  isDocumentTask?: boolean;
  taskId?: string;
}

// =============================================================================
// Task Panel Types (for Agentic workflow progress)
// =============================================================================

export type AgentTaskStatus = "pending" | "in_progress" | "completed" | "failed";

export interface AgentTask {
  id: string;
  description: string;
  status: AgentTaskStatus;
  result?: string;
  error?: string;
}

export interface CollectedInfo {
  key: string;
  value: string;
  source: string;
}

// =============================================================================
// Parallel Execution Types (for Tool Execution Visualization)
// =============================================================================

export type ToolExecutionStatus = "pending" | "running" | "completed" | "failed";

export interface ToolExecution {
  id: string;
  tool: string;
  status: ToolExecutionStatus;
  progress?: number;
  duration?: number;
  result?: string;
  error?: string;
}

export interface ParallelGroup {
  groupId: number;
  executions: ToolExecution[];
}

export interface ParallelExecutionViewProps {
  groups: ParallelGroup[];
  currentGroup: number;
}

// =============================================================================
// Manus-style Slide Outline Types (for PPT preview)
// =============================================================================

export interface SlideOutlineItem {
  number: number;
  title: string;
  subtitle?: string;
  type: "title" | "content" | "two_column" | "section" | "blank";
  bulletCount?: number;
}

export interface SlideOutline {
  title: string;
  subtitle?: string;
  slides: SlideOutlineItem[];
  theme?: string;
  totalSlides: number;
}

export interface OutlineReadyEventData {
  outline: SlideOutline;
  format: "pptx" | "docx" | "pdf";
}

// Step event data for AG-UI workflow tracking
export interface StepStartedEventData {
  step_id: string;
  title: string;
  description?: string;
  icon?: string;
  timestamp: number;
}

export interface StepFinishedEventData {
  step_id: string;
  status: "completed" | "failed";
  duration_ms?: number;
  timestamp: number;
}

// =============================================================================
// Manus-style Agentic Task Types
// =============================================================================

export type ManusTaskStatus = "pending" | "in_progress" | "completed" | "failed" | "blocked" | "skipped";
export type ManusTaskIcon = "search" | "web" | "kb" | "code" | "image" | "doc" | "ppt" | "file" | "brain";

export interface ManusSubTask {
  id: string;
  label: string;
  status: ManusTaskStatus;
  icon?: ManusTaskIcon;
  durationMs?: number;
}

export interface ManusTask {
  id: string;
  title: string;
  description?: string;
  status: ManusTaskStatus;
  icon?: ManusTaskIcon;
  subTasks?: ManusSubTask[];
  result?: string;
  error?: string;
  durationMs?: number;
  // For slide outline preview
  slideOutline?: SlideOutline;
}

// =============================================================================
// Working Memory State (for useChatSession hook)
// =============================================================================

export interface WorkingMemoryTask {
  id: string;
  description: string;
  status: ManusTaskStatus;
  icon?: string;
  result?: string;
  error?: string;
  startTime?: number;
  endTime?: number;
  durationMs?: number;
  currentTool?: string;
  // Search-related fields
  searchStatus?: string;  // "searching" | "completed" | "error"
  searchQuery?: string;
  searchSource?: "kb" | "web" | "file";
  searchProgress?: number;
  searchResultsFound?: number;
  searchResultsCount?: number;
  searchDurationMs?: number;
  subTasks?: Array<{
    id: string;
    name?: string;
    label?: string;
    status: string;
    endTime?: number;
    durationMs?: number;
    searchResultsCount?: number;
    searchDurationMs?: number;
  }>;
  slideOutline?: SlideOutline;
}

export interface WorkingMemoryCollectedInfo {
  key: string;
  value: string;
  source: string;
}

export interface WorkingMemory {
  goal?: string;
  tasks: WorkingMemoryTask[];
  collectedInfo: WorkingMemoryCollectedInfo[];
  notes: string[];
  runId?: string;
  error?: string;
}
