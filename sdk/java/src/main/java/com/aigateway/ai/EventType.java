package com.aigateway.ai;

/**
 * Canonical SSE event type constants.
 *
 * <p>These mirror the gateway's {@code SSEEventType} enum exactly and match
 * the Python SDK's {@code EventType} class. Use these instead of raw strings
 * for reliable pattern-matching in {@link StreamHandler} implementations.
 */
public final class EventType {

    private EventType() {} // constants only

    // -- Core stream events ---------------------------------------------------
    public static final String STARTED = "started";
    public static final String STATUS = "status";
    public static final String TEXT_DELTA = "text_delta";
    public static final String THINKING_DELTA = "thinking_delta";
    public static final String THINKING_END = "thinking_end";
    public static final String USAGE = "usage";
    public static final String FINISH = "finish";
    public static final String DONE = "done";
    public static final String ERROR = "error";

    // -- Retrieval and files --------------------------------------------------
    public static final String CONTEXT_RETRIEVED = "context_retrieved";
    public static final String WEB_SEARCH_RESULTS = "web_search_results";
    public static final String FILE_PROCESSED = "file_processed";
    public static final String RAG_EVALUATION = "rag_evaluation";
    public static final String CACHE_METRICS = "cache_metrics";

    // -- Session metadata -----------------------------------------------------
    public static final String SESSION_CREATED = "session_created";
    public static final String SESSION_UPDATED = "session_updated";

    // -- Validation / warnings ------------------------------------------------
    public static final String OUTPUT_WARNINGS = "output_warnings";

    // -- Run lifecycle --------------------------------------------------------
    public static final String RUN_STARTED = "run_started";
    public static final String RUN_FINISHED = "run_finished";
    public static final String RUN_ERROR = "run_error";
    public static final String CANCELLED = "cancelled";

    // -- Step lifecycle -------------------------------------------------------
    public static final String STEP_STARTED = "step_started";
    public static final String STEP_FINISHED = "step_finished";

    // -- Tool lifecycle -------------------------------------------------------
    public static final String TOOL_CALL_START = "tool_call_start";
    public static final String TOOL_CALL_ARGS = "tool_call_args";
    public static final String TOOL_CALL_END = "tool_call_end";
    public static final String TOOL_CALL_RESULT = "tool_call_result";
    public static final String TOOL_ERROR = "tool_error";

    // -- Legacy aliases (still emitted in some code paths) --------------------
    public static final String TOOL_CALL = "tool_call";
    public static final String TOOL_RESULT = "tool_result";
    public static final String TASK_PLANNING = "task_planning";
    public static final String WORKING_MEMORY_UPDATE = "working_memory_update";
    public static final String MEMORY_LOADED = "memory_loaded";

    // -- File generation ------------------------------------------------------
    public static final String FILE_CREATING = "file_creating";
    public static final String FILE_CREATED = "file_created";
    public static final String ARTIFACT_CREATED = "artifact_created";
    public static final String OUTLINE_READY = "outline_ready";

    // -- Search progress ------------------------------------------------------
    public static final String SEARCH_STARTED = "search_started";
    public static final String SEARCH_PROGRESS = "search_progress";
    public static final String SEARCH_COMPLETED = "search_completed";

    // -- Code / doc / image execution -----------------------------------------
    public static final String CODE_EXECUTION_START = "code_execution_start";
    public static final String CODE_EXECUTION_OUTPUT = "code_execution_output";
    public static final String CODE_EXECUTION_RESULT = "code_execution_result";
    public static final String IMAGE_GENERATION_START = "image_generation_start";
    public static final String IMAGE_GENERATION_RESULT = "image_generation_result";
    public static final String DOCUMENT_GENERATION_START = "document_generation_start";
    public static final String DOCUMENT_GENERATION_RESULT = "document_generation_result";

    // -- Agent loop phase telemetry -------------------------------------------
    public static final String PHASE_STARTED = "phase_started";
    public static final String PHASE_COMPLETED = "phase_completed";
    public static final String PHASE_PROGRESS = "phase_progress";

    // -- Gateway / context events ---------------------------------------------
    public static final String CONTEXT_BUDGET = "context_budget";
    public static final String CONTEXT_COMPACTED = "context_compacted";
    public static final String QUEUE_STATE = "queue_state";
    public static final String APPROVAL_REQUIRED = "approval_required";
    public static final String APPROVAL_RESULT = "approval_result";
    public static final String GATEWAY_DECISION = "gateway_decision";

    // -- Quiz events ----------------------------------------------------------
    public static final String QUIZ_STATUS = "quiz:status";
    public static final String QUIZ_READY = "quiz:ready";
    public static final String QUIZ_ERROR = "quiz:error";

    // -- Sub-Agent lifecycle (ADR-003) ----------------------------------------
    public static final String SUBAGENT_STARTED = "subagent_started";
    public static final String SUBAGENT_STEP = "subagent_step";
    public static final String SUBAGENT_TEXT_DELTA = "subagent_text_delta";
    public static final String SUBAGENT_TOOL_START = "subagent_tool_start";
    public static final String SUBAGENT_TOOL_RESULT = "subagent_tool_result";
    public static final String SUBAGENT_FINISHED = "subagent_finished";

    // -- AG-UI protocol snapshots ---------------------------------------------
    public static final String TEXT_MESSAGE_START = "text_message_start";
    public static final String TEXT_MESSAGE_CONTENT = "text_message_content";
    public static final String TEXT_MESSAGE_END = "text_message_end";
    public static final String STATE_SNAPSHOT = "state_snapshot";
    public static final String STATE_DELTA = "state_delta";
    public static final String MESSAGES_SNAPSHOT = "messages_snapshot";
    public static final String RAW_EVENT = "raw_event";
    public static final String CUSTOM_EVENT = "custom_event";
}
