"""
Stream event model and canonical event type constants.

Mirrors the gateway's SSE event vocabulary (see web/src/pages/assistant/sse-events.ts).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(slots=True)
class StreamEvent:
    """A single server-sent event parsed from the chat stream."""

    event_type: str
    """Event name, e.g. ``"text_delta"`` or ``"done"``."""

    data: dict[str, Any] = field(default_factory=dict)
    """Parsed JSON payload of the event."""

    timestamp: float | None = None
    """Server-assigned epoch timestamp (seconds), if present."""

    def is_text(self) -> bool:
        return self.event_type == EventType.TEXT_DELTA

    def is_done(self) -> bool:
        return self.event_type in {EventType.DONE, EventType.RUN_FINISHED}

    def is_error(self) -> bool:
        return self.event_type in {EventType.ERROR, EventType.RUN_ERROR}

    def is_cancelled(self) -> bool:
        return self.event_type == EventType.CANCELLED

    def is_terminal(self) -> bool:
        return self.is_done() or self.is_error() or self.is_cancelled()

    @property
    def text(self) -> str:
        """Convenience accessor for the text content of a ``text_delta`` event."""
        return str(self.data.get("content", self.data.get("text", "")))


class EventType:
    """Canonical SSE event type constants.

    These mirror the gateway's ``SSEEventType`` enum exactly. Use these
    instead of raw strings for reliable pattern-matching.
    """

    # -- Core stream events --------------------------------------------------
    STARTED: str = "started"
    STATUS: str = "status"
    TEXT_DELTA: str = "text_delta"
    THINKING_DELTA: str = "thinking_delta"
    THINKING_END: str = "thinking_end"
    USAGE: str = "usage"
    FINISH: str = "finish"
    DONE: str = "done"
    ERROR: str = "error"

    # -- Retrieval and files -------------------------------------------------
    CONTEXT_RETRIEVED: str = "context_retrieved"
    WEB_SEARCH_RESULTS: str = "web_search_results"
    FILE_PROCESSED: str = "file_processed"
    RAG_EVALUATION: str = "rag_evaluation"
    CACHE_METRICS: str = "cache_metrics"

    # -- Session metadata ----------------------------------------------------
    SESSION_CREATED: str = "session_created"
    SESSION_UPDATED: str = "session_updated"

    # -- Validation / warnings -----------------------------------------------
    OUTPUT_WARNINGS: str = "output_warnings"

    # -- Run lifecycle -------------------------------------------------------
    RUN_STARTED: str = "run_started"
    RUN_FINISHED: str = "run_finished"
    RUN_ERROR: str = "run_error"
    CANCELLED: str = "cancelled"
    ITEM: str = "item"
    ACTIVITY: str = "activity"

    # -- Step lifecycle ------------------------------------------------------
    STEP_STARTED: str = "step_started"
    STEP_FINISHED: str = "step_finished"

    # -- Tool lifecycle ------------------------------------------------------
    TOOL_CALL_START: str = "tool_call_start"
    TOOL_CALL_ARGS: str = "tool_call_args"
    TOOL_CALL_END: str = "tool_call_end"
    TOOL_CALL_RESULT: str = "tool_call_result"
    TOOL_ERROR: str = "tool_error"

    # -- Legacy aliases (still emitted in some code paths) -------------------
    TOOL_CALL: str = "tool_call"
    TOOL_RESULT: str = "tool_result"
    TASK_PLANNING: str = "task_planning"
    WORKING_MEMORY_UPDATE: str = "working_memory_update"
    MEMORY_LOADED: str = "memory_loaded"

    # -- File generation -----------------------------------------------------
    FILE_CREATING: str = "file_creating"
    FILE_CREATED: str = "file_created"
    ARTIFACT_CREATED: str = "artifact_created"
    OUTLINE_READY: str = "outline_ready"

    # -- Search progress -----------------------------------------------------
    SEARCH_STARTED: str = "search_started"
    SEARCH_PROGRESS: str = "search_progress"
    SEARCH_COMPLETED: str = "search_completed"

    # -- Code / doc / image execution ----------------------------------------
    CODE_EXECUTION_START: str = "code_execution_start"
    CODE_EXECUTION_OUTPUT: str = "code_execution_output"
    CODE_EXECUTION_RESULT: str = "code_execution_result"
    IMAGE_GENERATION_START: str = "image_generation_start"
    IMAGE_GENERATION_RESULT: str = "image_generation_result"
    DOCUMENT_GENERATION_START: str = "document_generation_start"
    DOCUMENT_GENERATION_RESULT: str = "document_generation_result"

    # -- Agent loop phase telemetry ------------------------------------------
    PHASE_STARTED: str = "phase_started"
    PHASE_COMPLETED: str = "phase_completed"
    PHASE_PROGRESS: str = "phase_progress"

    # -- Gateway / context events --------------------------------------------
    CONTEXT_BUDGET: str = "context_budget"
    CONTEXT_COMPACTED: str = "context_compacted"
    QUEUE_STATE: str = "queue_state"
    APPROVAL_REQUIRED: str = "approval_required"
    APPROVAL_RESULT: str = "approval_result"
    GATEWAY_DECISION: str = "gateway_decision"

    # -- Quiz events ---------------------------------------------------------
    QUIZ_STATUS: str = "quiz:status"
    QUIZ_READY: str = "quiz:ready"
    QUIZ_ERROR: str = "quiz:error"

    # -- Sub-Agent lifecycle (ADR-003) ---------------------------------------
    SUBAGENT_STARTED: str = "subagent_started"
    SUBAGENT_STEP: str = "subagent_step"
    SUBAGENT_TEXT_DELTA: str = "subagent_text_delta"
    SUBAGENT_TOOL_START: str = "subagent_tool_start"
    SUBAGENT_TOOL_RESULT: str = "subagent_tool_result"
    SUBAGENT_FINISHED: str = "subagent_finished"

    # -- AG-UI protocol snapshots --------------------------------------------
    TEXT_MESSAGE_START: str = "text_message_start"
    TEXT_MESSAGE_CONTENT: str = "text_message_content"
    TEXT_MESSAGE_END: str = "text_message_end"
    STATE_SNAPSHOT: str = "state_snapshot"
    STATE_DELTA: str = "state_delta"
    MESSAGES_SNAPSHOT: str = "messages_snapshot"
    RAW_EVENT: str = "raw_event"
    CUSTOM_EVENT: str = "custom_event"
