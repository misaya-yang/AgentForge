/**
 * Canonical SSE event names for Assistant chat stream.
 *
 * Keep this file as the single source of truth for frontend event names.
 */
export const SSEEventType = {
  // Core stream events
  STARTED: "started",
  STATUS: "status",
  TEXT_DELTA: "text_delta",
  THINKING_START: "thinking_start",
  THINKING_DELTA: "thinking_delta",
  THINKING_END: "thinking_end",
  USAGE: "usage",
  FINISH: "finish",
  DONE: "done",
  ERROR: "error",

  // Retrieval and files
  CONTEXT_RETRIEVED: "context_retrieved",
  RAG_RETRIEVAL_STARTED: "rag_retrieval_started",
  RAG_RETRIEVAL_COMPLETED: "rag_retrieval_completed",
  RAG_RETRIEVAL_FAILED: "rag_retrieval_failed",
  WEB_SEARCH_RESULTS: "web_search_results",
  FILE_PROCESSED: "file_processed",
  RAG_EVALUATION: "rag_evaluation",
  CACHE_METRICS: "cache_metrics",

  // Session metadata
  SESSION_CREATED: "session_created",
  SESSION_UPDATED: "session_updated",

  // Validation / warnings
  OUTPUT_WARNINGS: "output_warnings",

  // Run lifecycle
  RUN_STARTED: "run_started",
  RUN_FINISHED: "run_finished",
  RUN_ERROR: "run_error",
  CANCELLED: "cancelled",
  ITEM: "item",
  ACTIVITY: "activity",

  // Step lifecycle
  STEP_STARTED: "step_started",
  STEP_FINISHED: "step_finished",

  // Tool lifecycle
  TOOL_CALL_START: "tool_call_start",
  TOOL_CALL_ARGS: "tool_call_args",
  TOOL_CALL_END: "tool_call_end",
  TOOL_CALL_RESULT: "tool_call_result",
  TOOL_ERROR: "tool_error",

  // Legacy aliases still emitted in some paths
  TOOL_CALL: "tool_call",
  TOOL_RESULT: "tool_result",
  TASK_PLANNING: "task_planning",
  PLAN_UPDATE: "plan_update",
  WORKING_MEMORY_UPDATE: "working_memory_update",
  MEMORY_LOADED: "memory_loaded",

  // File generation
  FILE_CREATING: "file_creating",
  FILE_CREATED: "file_created",
  ARTIFACT_CREATED: "artifact_created",
  OUTLINE_READY: "outline_ready",

  // Search progress
  SEARCH_STARTED: "search_started",
  SEARCH_PROGRESS: "search_progress",
  SEARCH_COMPLETED: "search_completed",

  // Code/doc/image execution
  CODE_EXECUTION_START: "code_execution_start",
  CODE_EXECUTION_OUTPUT: "code_execution_output",
  CODE_EXECUTION_RESULT: "code_execution_result",
  IMAGE_GENERATION_START: "image_generation_start",
  IMAGE_GENERATION_RESULT: "image_generation_result",
  DOCUMENT_GENERATION_START: "document_generation_start",
  DOCUMENT_GENERATION_RESULT: "document_generation_result",

  // Agent loop phase telemetry
  PHASE_STARTED: "phase_started",
  PHASE_COMPLETED: "phase_completed",
  PHASE_PROGRESS: "phase_progress",

  // New gateway/context events
  CONTEXT_BUDGET: "context_budget",
  CONTEXT_COMPACTED: "context_compacted",
  CONTEXT_COMPACTION: "context_compaction",
  QUEUE_STATE: "queue_state",
  APPROVAL_REQUIRED: "approval_required",
  APPROVAL_RESULT: "approval_result",
  SIDE_EFFECT_UNKNOWN: "side_effect_unknown",
  GATEWAY_DECISION: "gateway_decision",

  // Quiz events
  QUIZ_STATUS: "quiz:status",
  QUIZ_READY: "quiz:ready",
  QUIZ_ERROR: "quiz:error",

  // Sub-Agent lifecycle (ADR-003)
  SUBAGENT_STARTED: "subagent_started",
  SUBAGENT_STEP: "subagent_step",
  SUBAGENT_TEXT_DELTA: "subagent_text_delta",
  SUBAGENT_TOOL_START: "subagent_tool_start",
  SUBAGENT_TOOL_RESULT: "subagent_tool_result",
  SUBAGENT_FINISHED: "subagent_finished",

  // AG-UI snapshots
  TEXT_MESSAGE_START: "text_message_start",
  TEXT_MESSAGE_CONTENT: "text_message_content",
  TEXT_MESSAGE_END: "text_message_end",
  STATE_SNAPSHOT: "state_snapshot",
  STATE_DELTA: "state_delta",
  MESSAGES_SNAPSHOT: "messages_snapshot",
  RAW_EVENT: "raw_event",
  CUSTOM_EVENT: "custom_event",
} as const;

export type SSEEventTypeValue = (typeof SSEEventType)[keyof typeof SSEEventType];
