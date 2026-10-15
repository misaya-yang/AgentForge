"""
API schemas for Assistant service.

Phase 1: Unified session + message + streaming protocol.
- Server as single source of truth for session/history
- Unified SSE event schema
- Enhanced message with tool_calls/tool_results/citations/attachments
"""

from __future__ import annotations

import re
from typing import Any, Literal

from ai_gateway_core.style_presets import StylePreset, resolve_style_preset
from pydantic import BaseModel, Field, field_validator, model_validator

# =============================================================================
# Tool Call Structures (for Agentic workflows)
# =============================================================================


class ToolCallArgument(BaseModel):
    """A single argument in a tool call."""

    name: str = Field(..., description="Argument name")
    value: Any = Field(..., description="Argument value")


class ToolCall(BaseModel):
    """A tool call made by the assistant."""

    id: str = Field(..., description="Unique tool call ID")
    name: str = Field(..., description="Tool name")
    arguments: dict[str, Any] = Field(
        default_factory=dict, description="Tool arguments as key-value pairs"
    )
    status: str = Field(default="pending", description="Status: pending, running, completed, error")


class ToolResult(BaseModel):
    """Result from a tool execution."""

    tool_call_id: str = Field(..., description="ID of the tool call this result belongs to")
    name: str = Field(..., description="Tool name")
    result: Any = Field(default=None, description="Tool execution result")
    error: str | None = Field(default=None, description="Error message if execution failed")
    duration_ms: float | None = Field(default=None, description="Execution time in milliseconds")


# =============================================================================
# Citation Structures (for RAG traceability)
# =============================================================================


class Citation(BaseModel):
    """A citation linking content to source."""

    dataset_id: str = Field(..., description="Source dataset ID")
    document_id: str | None = Field(default=None, description="Source document ID")
    chunk_id: str | None = Field(default=None, description="Source chunk ID")
    source_url: str | None = Field(default=None, description="URL to source if available")
    title: str | None = Field(default=None, description="Source title")
    score: float | None = Field(default=None, description="Relevance score")
    content_preview: str | None = Field(default=None, description="Preview of cited content")


# =============================================================================
# Attachment Structures (for multimodal input)
# =============================================================================


class Attachment(BaseModel):
    """An attachment in a message."""

    id: str = Field(..., description="Attachment ID")
    type: str = Field(..., description="Attachment type: image, file, audio")
    filename: str | None = Field(default=None, description="Original filename")
    url: str | None = Field(default=None, description="URL to access the attachment")
    mime_type: str | None = Field(default=None, description="MIME type")
    size_bytes: int | None = Field(default=None, description="File size in bytes")


# =============================================================================
# Enhanced Message Structure
# =============================================================================


class AssistantMessage(BaseModel):
    """
    A message in the conversation with full agentic support.

    Supports:
    - Tool calls and results
    - Citations from RAG
    - File/image attachments
    - Metadata for tracing
    """

    role: str = Field(..., description="Message role: user, assistant, system, or tool")
    content: str = Field(default="", description="Message text content")

    # Agentic extensions
    tool_calls: list[ToolCall] | None = Field(
        default=None, description="Tool calls made in this message"
    )
    tool_results: list[ToolResult] | None = Field(
        default=None, description="Tool results for tool messages"
    )

    # RAG traceability
    citations: list[Citation] | None = Field(
        default=None, description="Citations linking to sources"
    )

    # Multimodal support
    attachments: list[Attachment] | None = Field(default=None, description="File/image attachments")

    # Metadata
    message_id: str | None = Field(default=None, description="Unique message ID")
    timestamp: str | None = Field(default=None, description="ISO timestamp")
    metadata: dict[str, Any] | None = Field(default=None, description="Additional metadata")


class AssistantChatRequest(BaseModel):
    """Request body for chat endpoint."""

    message: str = Field(..., description="User's message", min_length=1, max_length=200_000)
    session_id: str | None = Field(default=None, description="Session ID for conversation tracking")
    history: list[AssistantMessage] | None = Field(
        default=None,
        max_length=200,
        description="Previous conversation history. If None and session_id provided, auto-loads from session.",
    )

    # Model settings
    model_id: str | None = Field(
        default=None,
        description="Model ID to use; omit for the server-side default model",
    )
    temperature: float = Field(default=0.7, ge=0.0, le=2.0, description="Sampling temperature")
    max_tokens: int | None = Field(
        default=None,
        ge=1,
        le=32_768,
        description="Maximum tokens to generate",
    )

    # Knowledge base settings
    kb_dataset_ids: list[str] = Field(
        default_factory=list,
        max_length=8,
        description="Dataset IDs to search for context (maximum 8)",
    )
    kb_mode: str = Field(default="auto", description="RAG mode: auto, tool, or off")
    kb_top_k: int = Field(default=5, ge=1, le=20, description="Number of KB results to retrieve")
    kb_score_threshold: float = Field(
        default=0.0, ge=0.0, le=1.0, description="Minimum relevance score (0.0 = no filtering)"
    )
    kb_include_images: bool = Field(
        default=False,
        description="Reserved for a future multimodal KB release; must remain false",
    )

    # Web search settings
    web_search_enabled: bool = Field(default=False, description="Enable web search via Tavily")
    web_search_max_results: int = Field(
        default=5, ge=1, le=10, description="Maximum web search results"
    )

    # File attachments
    file_paths: list[str] = Field(
        default_factory=list,
        max_length=20,
        description="Paths to uploaded files for multimodal input (maximum 20)",
    )

    # System prompt
    system_prompt: str | None = Field(
        default=None,
        max_length=100_000,
        description="Custom system prompt",
    )

    @field_validator("file_paths")
    @classmethod
    def validate_file_paths(cls, value: list[str]) -> list[str]:
        if any(len(path) > 1024 for path in value):
            raise ValueError("Each attachment path must be at most 1024 characters")
        return value

    # Task planning settings
    enable_task_planning: bool = Field(
        default=False, description="Enable task planning and tool orchestration"
    )
    thinking_level: str | None = Field(
        default=None,
        max_length=16,
        description=(
            "Legacy reasoning option alias. Prefer reasoning_option for model-specific profiles."
        ),
    )
    reasoning_option: str | None = Field(
        default=None,
        min_length=1,
        max_length=32,
        pattern=r"^[a-z][a-z0-9_-]{0,31}$",
        description="Model-profile reasoning option id; auto inherits the model default",
    )
    confirm_plan: Literal[False] = Field(
        default=False,
        description=(
            "Reserved for durable plan approval and resume; true is rejected in this release"
        ),
    )

    # Assistant gateway policy profile
    execution_profile: Literal["safe", "balanced", "power"] = Field(
        default="safe", description="Execution profile: safe, balanced, power"
    )
    memory_mode: Literal["auto", "strict", "off"] = Field(
        default="auto", description="Memory mode: auto, strict, off"
    )
    os_agent_enabled: bool = Field(
        default=False, description="Enable OS-Agent Lite tools for this request"
    )
    local_node_device_id: str | None = Field(
        default=None,
        min_length=1,
        max_length=128,
        pattern=r"^[A-Za-z0-9][A-Za-z0-9._:-]*$",
        description="Selected Local Node; downstream verifies all authority",
    )
    local_node_grant_ids: list[str] = Field(
        default_factory=list,
        max_length=16,
        description="Selected grant IDs; downstream verifies all authority",
    )
    runtime_mode: Literal["off", "compat", "full"] = Field(
        default="compat",
        description="assistant runtime mode: off, compat, full",
    )
    queue_mode: Literal["collect", "followup", "steer", "interrupt"] = Field(
        default="collect",
        description="Queue mode: collect, followup, steer, interrupt",
    )
    context_detail: bool = Field(
        default=False,
        description="Return detailed context token contributors for this run",
    )
    skills_enabled: bool | None = Field(
        default=None,
        description="Override dynamic skill injection behavior for this request",
    )
    memory_profile: Literal["off", "basic", "hybrid"] | None = Field(
        default=None,
        description="Memory profile override: off, basic, hybrid",
    )
    resume_run_id: str | None = Field(
        default=None,
        description="Resume a paused assistant run after tool approval",
    )
    resume_approval_id: str | None = Field(
        default=None,
        description="Approved tool approval_id binding for resume execution",
    )

    @model_validator(mode="after")
    def _validate_local_node_selection(self) -> AssistantChatRequest:
        grant_ids = self.local_node_grant_ids
        if len(set(grant_ids)) != len(grant_ids) or any(
            re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}", grant_id) is None
            for grant_id in grant_ids
        ):
            raise ValueError("Local Node grant selectors are invalid")
        if bool(self.local_node_device_id) != bool(grant_ids):
            raise ValueError(
                "Local Node selectors require one device and at least one grant"
            )
        return self

    @field_validator("kb_dataset_ids")
    @classmethod
    def validate_kb_dataset_ids(cls, value: list[str]) -> list[str]:
        if any(not dataset_id.strip() or len(dataset_id) > 128 for dataset_id in value):
            raise ValueError("KB dataset IDs must be non-empty and at most 128 characters")
        if len(set(value)) != len(value):
            raise ValueError("KB dataset IDs must be unique")
        return value

    @field_validator("kb_include_images")
    @classmethod
    def reject_unreleased_kb_images(cls, value: bool) -> bool:
        if value:
            raise ValueError("Multimodal knowledge retrieval is not enabled in this release")
        return value

    @field_validator("confirm_plan", mode="before")
    @classmethod
    def reject_unsupported_plan_confirmation(cls, value: Any) -> Any:
        if value is True:
            raise ValueError(
                "Plan confirmation is not supported until durable plan approval and resume "
                "are available; omit confirm_plan or set it to false"
            )
        return value


class AssistantChatResponse(BaseModel):
    """Response body for non-streaming chat."""

    content: str = Field(..., description="Assistant's response")
    usage: dict[str, int] = Field(default_factory=dict, description="Token usage statistics")
    contexts: list[dict[str, Any]] = Field(
        default_factory=list, description="Retrieved KB contexts"
    )
    duration_ms: float = Field(..., description="Total processing time in milliseconds")
    model_id: str = Field(..., description="Model ID used")
    session_id: str | None = Field(default=None, description="Session ID")
    run_id: str | None = Field(default=None, description="Assistant run ID")


class ModelInfoResponse(BaseModel):
    """Model information."""

    id: str = Field(..., description="Model ID")
    name: str = Field(..., description="Display name")
    provider: str = Field(..., description="Provider name")
    context_window: int = Field(..., description="Context window size in tokens")
    max_output_tokens: int = Field(..., description="Maximum output tokens")
    supports_vision: bool = Field(..., description="Whether model supports vision")
    supports_tools: bool = Field(..., description="Whether model supports tool calling")
    access_level: str = Field(default="public", description="Access level: public, premium, admin")
    input_price_per_1k: float = Field(default=0.0, description="Price per 1K input tokens (USD)")
    output_price_per_1k: float = Field(default=0.0, description="Price per 1K output tokens (USD)")
    effective_capabilities: dict[str, Any] = Field(
        default_factory=dict,
        description="Resolved model capability profile visible to this tenant",
    )
    capability_revision: int = Field(default=1, ge=1)


class ModelsListResponse(BaseModel):
    """Response for listing available models."""

    models: list[ModelInfoResponse] = Field(..., description="List of available models")


class DatasetInfoResponse(BaseModel):
    """Dataset information for KB selection."""

    dataset_id: str = Field(..., description="Dataset ID")
    name: str = Field(..., description="Dataset name")
    description: str | None = Field(default=None, description="Dataset description")
    document_count: int = Field(default=0, description="Number of documents")
    chunk_count: int = Field(default=0, description="Number of chunks")
    embedding_model: str | None = Field(default=None, description="Embedding model used")
    is_multimodal: bool = Field(
        default=False, description="Whether KB supports multimodal (image) retrieval"
    )


class DatasetsListResponse(BaseModel):
    """Response for listing available datasets."""

    datasets: list[DatasetInfoResponse] = Field(..., description="List of available datasets")


# =============================================================================
# Unified SSE Event Schema (Phase 1)
# =============================================================================


class SSEEventType:
    """Standard SSE event types for assistant streaming."""

    # Content streaming
    TEXT_DELTA = "text_delta"  # Incremental text content
    THINKING_DELTA = "thinking_delta"  # Model reasoning (for CoT display)

    # Tool execution
    TOOL_CALL = "tool_call"  # Tool invocation started
    TOOL_RESULT = "tool_result"  # Tool execution completed

    # RAG context
    CONTEXT_RETRIEVED = "context_retrieved"  # KB search results
    WEB_SEARCH_RESULTS = "web_search_results"  # Web search results
    CONTEXT_BUDGET = "context_budget"  # Context token budget and usage
    CONTEXT_COMPACTED = "context_compacted"  # Context compaction executed
    CONTEXT_DETAIL = "context_detail"  # Detailed token/cost breakdown by contributor
    MEMORY_RETRIEVED = "memory_retrieved"  # Hybrid memory retrieval summary
    MEMORY_REFLECTION_SCHEDULED = "memory_reflection_scheduled"  # Reflection job created

    # Phase 3: RAG evaluation
    RAG_EVALUATION = "rag_evaluation"  # RAG quality metrics and citations

    # Gateway / queue / approval
    QUEUE_STATE = "queue_state"  # Command queue state update
    QUEUE_STEERED = "queue_steered"  # Queue steering update
    APPROVAL_REQUIRED = "approval_required"  # Tool call needs approval
    APPROVAL_RESULT = "approval_result"  # Approval decision applied
    GATEWAY_DECISION = "gateway_decision"  # Gateway policy decision
    SANDBOX_DECISION = "sandbox_decision"  # Sandbox policy decision
    SKILL_SELECTED = "skill_selected"  # Skill selected for current query
    SKILL_LOADED = "skill_loaded"  # Skill metadata loaded
    SKILL_CREATE_PENDING_APPROVAL = "skill_create_pending_approval"  # Skill proposal pending

    # KV-Cache metrics
    CACHE_METRICS = "cache_metrics"  # Cache performance metrics

    # Session/state
    SESSION_CREATED = "session_created"  # New session created
    SESSION_UPDATED = "session_updated"  # Session metadata updated

    # Completion
    USAGE = "usage"  # Token usage statistics
    FINISH = "finish"  # Generation finished (may continue with tools)
    DONE = "done"  # Complete response done
    ERROR = "error"  # Error occurred


class ContextRetrievedEvent(BaseModel):
    """Data for context_retrieved event."""

    dataset_id: str
    dataset_name: str
    chunks: list[dict[str, Any]]
    query: str
    took_ms: float


class WebSearchResultsEvent(BaseModel):
    """Data for web_search_results event."""

    query: str
    results: list[dict[str, Any]]
    answer: str | None = None
    took_ms: float


class ToolCallEvent(BaseModel):
    """Data for tool_call event."""

    id: str
    name: str
    arguments: dict[str, Any]
    status: str = "pending"


class ToolResultEvent(BaseModel):
    """Data for tool_result event."""

    tool_call_id: str
    name: str
    result: Any
    error: str | None = None
    duration_ms: float | None = None


class UsageEvent(BaseModel):
    """Data for usage event."""

    input_tokens: int | None = None
    output_tokens: int | None = None
    total_tokens: int | None = None
    cached_tokens: int | None = None


class DoneEvent(BaseModel):
    """Data for done event."""

    session_id: str
    total_length: int
    duration_ms: float
    model_id: str
    kb_datasets_used: list[str] = Field(default_factory=list)
    context_truncated: bool = False
    # Phase 3: RAG quality summary
    rag_quality: float | None = Field(default=None, description="RAG quality score 0-100")
    citations_count: int = Field(default=0, description="Number of citations generated")


# =============================================================================
# Phase 3: RAG Evaluation Event
# =============================================================================


class RAGCitation(BaseModel):
    """A citation linking response to source (Phase 3)."""

    citation_id: str = Field(..., description="Unique citation ID")
    chunk_id: str = Field(..., description="Source chunk ID")
    dataset_id: str = Field(..., description="Source dataset ID")
    dataset_name: str = Field(..., description="Dataset display name")
    source_url: str | None = Field(default=None, description="URL to source")
    source_title: str | None = Field(default=None, description="Source title")
    cited_text: str = Field(default="", description="Text that was cited")
    context_preview: str = Field(default="", description="Preview of source context")
    relevance_score: float = Field(default=0.0, description="Relevance score")
    status: str = Field(default="implicit", description="Citation status: used, implicit, unused")


class RAGQualityBreakdown(BaseModel):
    """Breakdown of RAG quality score components."""

    relevance: float = Field(default=0.0, description="Relevance score (0-25)")
    coverage: float = Field(default=0.0, description="Coverage score (0-25)")
    usage: float = Field(default=0.0, description="Usage score (0-25)")
    citations: float = Field(default=0.0, description="Citation score (0-25)")


class RAGEvaluationEvent(BaseModel):
    """Data for rag_evaluation event (Phase 3)."""

    quality_score: float = Field(..., description="Overall quality score 0-100")
    quality_breakdown: RAGQualityBreakdown = Field(..., description="Score breakdown by component")
    chunks_retrieved: int = Field(..., description="Total chunks retrieved")
    chunks_used: int = Field(..., description="Chunks used in response")
    response_grounding: float = Field(..., description="What % of response is grounded (0-1)")
    citations: list[RAGCitation] = Field(default_factory=list, description="Citations extracted")
    evaluation_time_ms: float = Field(..., description="Time taken for evaluation")


# =============================================================================
# KV-Cache Metrics Event
# =============================================================================


class CacheMetricsEvent(BaseModel):
    """Cache performance metrics for KV-cache optimization monitoring."""

    layer1_hit: bool = Field(
        default=False, description="Whether Layer 1 (system prefix) cache was hit"
    )
    layer2_hit: bool = Field(
        default=False, description="Whether Layer 2 (session context) cache was hit"
    )
    total_input_tokens: int = Field(default=0, ge=0, description="Total input tokens")
    cached_tokens: int = Field(default=0, ge=0, description="Number of tokens served from cache")
    cache_hit_rate: float = Field(default=0.0, ge=0.0, le=1.0, description="Cache hit rate (0-1)")
    estimated_savings_usd: float = Field(
        default=0.0, ge=0.0, description="Estimated cost savings in USD"
    )
    system_prefix_hash: str = Field(
        default="", description="Hash of Layer 1 cache key for debugging"
    )


class ErrorEvent(BaseModel):
    """Data for error event."""

    message: str
    code: str | None = None
    recoverable: bool = True


class StreamEventData(BaseModel):
    """
    SSE event data structure.

    Event types:
    - text_delta: Incremental text content (data: str)
    - thinking_delta: Model reasoning display (data: str)
    - tool_call: Tool invocation (data: ToolCallEvent)
    - tool_result: Tool execution result (data: ToolResultEvent)
    - context_retrieved: KB search results (data: ContextRetrievedEvent)
    - web_search_results: Web search results (data: WebSearchResultsEvent)
    - rag_evaluation: RAG quality metrics (data: RAGEvaluationEvent)
    - cache_metrics: KV-cache performance metrics (data: CacheMetricsEvent)
    - usage: Token usage (data: UsageEvent)
    - finish: Generation finished (data: {"reason": str})
    - done: Complete response (data: DoneEvent)
    - error: Error occurred (data: ErrorEvent)
    """

    event_type: str = Field(..., description="Event type from SSEEventType")
    data: Any = Field(default=None, description="Event payload (type depends on event_type)")
    timestamp: float = Field(..., description="Event timestamp (Unix epoch)")


class AssistantConfigResponse(BaseModel):
    """Response for getting assistant configuration."""

    default_model_id: str = Field(..., description="Default model ID")
    available_providers: list[str] = Field(..., description="List of configured providers")
    kb_enabled: bool = Field(..., description="Whether KB integration is enabled")
    web_search_enabled: bool = Field(default=False, description="Whether web search is available")
    tools_available: list[str] = Field(default_factory=list, description="List of available tools")


# =============================================================================
# Session History with Enhanced Messages
# =============================================================================


class EnhancedSessionMessage(BaseModel):
    """A message in session history with full context."""

    message_id: str
    role: str
    content: str
    timestamp: str

    # Agentic extensions
    tool_calls: list[ToolCall] | None = None
    tool_results: list[ToolResult] | None = None

    # RAG context
    citations: list[Citation] | None = None
    retrieved_contexts: list[ContextRetrievedEvent] | None = None

    # Attachments
    attachments: list[Attachment] | None = None

    # Metadata
    model_id: str | None = None
    usage: UsageEvent | None = None
    metadata: dict[str, Any] | None = None


# =============================================================================
# Image Generation (Smart Routing)
# =============================================================================


# Upper bound for reference_image strings on image-generation requests.
# 6,000,000 base64 chars ≈ 4.5 MiB decoded; anything larger should be
# compressed client-side before hitting us. Must stay ≤ nginx
# `client_max_body_size` so this Pydantic 422 fires *before* the proxy
# throws a generic 413 — keep them aligned if nginx is ever tuned.
_REFERENCE_IMAGE_MAX_CHARS = 6_000_000


class ImageGenerationRequest(BaseModel):
    """Request for image generation (single-turn or multi-turn).

    Multi-turn mode (recommended): provide session_id. The backend builds the full
    conversation history (including previously generated images) and sends to Gemini.
    The model decides whether to edit a previous image or create something new.

    Single-turn mode: omit session_id. Just prompt → image, no history.
    """

    prompt: str = Field(..., description="Text description or edit instruction", min_length=1)
    model_id: str = Field(..., description="Current model ID to determine provider routing")
    style: StylePreset = Field(
        default=StylePreset.DEFAULT,
        description=(
            "Visual style preset. Works across all providers: injected as a prompt "
            "modifier for Gemini/Doubao, mapped to a native tag for DashScope. "
            "Supported: default, realistic, anime, abstract, oil_paint, watercolor, "
            "3d_render, pixel_art, sketch, comic."
        ),
    )
    size: str | None = Field(default="1024*1024", description="Image size")
    n: int = Field(default=1, ge=1, le=4, description="Number of images to generate")
    session_id: str | None = Field(
        default=None,
        description="Image chat session ID. Enables multi-turn editing with full conversation history.",
    )
    reference_artifact_id: str | None = Field(
        default=None,
        description=(
            "Stable artifact ID from an earlier generation (preferred for stateless "
            "edits). The server looks up bytes directly via ArtifactStorage — no URL "
            "fetch, no SSRF surface. Caller's tenant + user_id must match the "
            "artifact's owner."
        ),
    )
    reference_blob_id: str | None = Field(
        default=None,
        description="Object-store blob id from /image-blobs/* for user-provided reference images.",
    )
    reference_image: str | None = Field(
        default=None,
        max_length=_REFERENCE_IMAGE_MAX_CHARS,
        description=(
            "Previous image to edit (base64 or data URL). Use only when the prior "
            "image is local-only (e.g. a user upload that never went through us). "
            "Prefer ``reference_artifact_id`` (server-side lookup) or "
            "``reference_image_url`` (we fetch via SSRF-safe client). Gemini-only. "
            "Max 6,000,000 chars (~4.5 MiB decoded)."
        ),
    )
    reference_image_url: str | None = Field(
        default=None,
        description=(
            "URL of a prior image. AS fetches it via SSRF-safe client (DNS pinning + "
            "private/loopback rejection + 8 MB streaming cap). Only http(s); private/"
            "loopback/link-local rejected. Prefer ``reference_artifact_id`` when "
            "possible — that path doesn't fetch a URL at all."
        ),
    )
    add_watermark: bool = Field(
        default=True, description="Add AI-generated watermark to output images"
    )
    app_user_id: str | None = None
    app_tenant_id: str | None = None
    parent_artifact_id: str | None = None
    expected_parent_artifact_id: str | None = None
    client_request_id: str | None = Field(default=None, max_length=128)
    return_variants: list[str] | None = None
    allow_branch: bool = False

    @field_validator("style", mode="before")
    @classmethod
    def _coerce_style(cls, value: Any) -> StylePreset:
        """Accept legacy strings (e.g. ``"oil"``, ``"<photography>"``) and unknowns.

        Style is a hint, not a security boundary — silent normalisation to
        DEFAULT keeps old clients working while new clients get the enum.
        """
        return resolve_style_preset(value)


class GeneratedImage(BaseModel):
    """A generated image result."""

    url: str = Field(
        ...,
        description=(
            "Presigned S3 URL when artifact storage is configured (preferred). "
            "Falls back to ``data:image/...;base64,...`` only when storage is "
            "unavailable (dev/test)."
        ),
    )
    width: int | None = Field(default=None, description="Image width")
    height: int | None = Field(default=None, description="Image height")
    artifact_id: str | None = Field(
        default=None,
        description=(
            "Stable ID for this generated image. Pass back as "
            "``reference_artifact_id`` to edit it later — fastest + safest path "
            "(no URL fetch, no base64 re-upload)."
        ),
    )


class ImageGenerationResponse(BaseModel):
    """Response for image generation."""

    success: bool = Field(..., description="Whether generation succeeded")
    images: list[GeneratedImage] = Field(default_factory=list, description="Generated images")
    task_id: str | None = None
    status: str | None = None
    provider: str | None = Field(
        default=None, description="Provider used for generation (dashscope/google)"
    )
    effective_model_id: str | None = Field(default=None, description="Configured image model used by the provider")
    duration_ms: float | None = Field(default=None, description="Generation time in milliseconds")
    error: str | None = Field(default=None, description="Error message if failed")
    error_code: str | None = None
    session_id: str | None = Field(
        default=None,
        description="Echo of session_id when stateful multi-turn was used.",
    )
    turn_id: str | None = None
    parent_artifact_id: str | None = None
    output_artifact_id: str | None = None
    client_request_id: str | None = None
    idempotent_replay: bool = False
    variants: dict[str, str] | None = None
    latest_advanced: bool = True


# =============================================================================
# Async Image Generation (background task with polling)
# =============================================================================


class AsyncImageGenerationRequest(BaseModel):
    """Request to submit an async image generation task."""

    prompt: str = Field(..., description="Text description of the image to generate", min_length=1)
    model_id: str = Field(..., description="Model ID to determine provider routing")
    style: StylePreset = Field(
        default=StylePreset.DEFAULT,
        description=(
            "Visual style preset. Works across all providers. Supported: default, "
            "realistic, anime, abstract, oil_paint, watercolor, 3d_render, "
            "pixel_art, sketch, comic."
        ),
    )
    size: str | None = Field(default="1024*1024", description="Image size")
    n: int = Field(default=1, ge=1, le=4, description="Number of images to generate")
    session_id: str | None = Field(
        default=None, description="Session ID for stateful multi-turn editing"
    )
    reference_artifact_id: str | None = Field(
        default=None,
        description=(
            "Stable artifact ID from an earlier generation (preferred for stateless "
            "edits). Server-side lookup; tenant + user_id checked against artifact owner."
        ),
    )
    reference_blob_id: str | None = Field(
        default=None,
        description="Object-store blob id from /image-blobs/* for user-provided reference images.",
    )
    reference_image: str | None = Field(
        default=None,
        max_length=_REFERENCE_IMAGE_MAX_CHARS,
        description=(
            "Previous image as base64 / data URL. Prefer ``reference_artifact_id`` or "
            "``reference_image_url`` — direct base64 transit costs ~1.3 MB / request. "
            "Gemini-only. Max 6,000,000 chars (~4.5 MiB decoded)."
        ),
    )
    reference_image_url: str | None = Field(
        default=None,
        description=(
            "URL of a prior image. AS fetches via SSRF-safe client. Only http(s); "
            "private/loopback rejected; 8 MB streaming cap."
        ),
    )
    add_watermark: bool = Field(
        default=True, description="Add AI-generated watermark to output images"
    )
    app_user_id: str | None = None
    app_tenant_id: str | None = None
    parent_artifact_id: str | None = None
    expected_parent_artifact_id: str | None = None
    client_request_id: str | None = Field(default=None, max_length=128)
    return_variants: list[str] | None = None
    allow_branch: bool = False
    callback_url: str | None = Field(
        default=None, description="URL to POST results when generation completes"
    )

    @field_validator("style", mode="before")
    @classmethod
    def _coerce_style(cls, value: Any) -> StylePreset:
        return resolve_style_preset(value)


class AsyncImageTaskSubmitResponse(BaseModel):
    """Response after submitting an async image task."""

    task_id: str = Field(..., description="Unique task ID for polling")
    status: str = Field(default="pending", description="Initial task status")
    message: str = Field(default="Image generation task submitted")


class AsyncImageArtifact(BaseModel):
    """An image artifact from completed generation."""

    artifact_id: str | None = Field(default=None, description="Artifact ID if saved")
    download_url: str | None = Field(default=None, description="Presigned download URL")
    url: str = Field(..., description="Presigned S3 URL; data URL only in dev/test fallback")
    width: int | None = None
    height: int | None = None


class AsyncImageTaskStatusResponse(BaseModel):
    """Response for polling async image task status."""

    task_id: str = Field(..., description="Task ID")
    status: str = Field(..., description="pending | running | completed | failed")
    progress: int = Field(default=0, description="Progress percentage 0-100")
    prompt: str = Field(..., description="Original prompt")
    model_id: str = Field(..., description="Model used")
    provider: str | None = Field(default=None, description="Provider used")
    effective_model_id: str | None = Field(default=None, description="Configured image model used by the provider")
    images: list[AsyncImageArtifact] = Field(
        default_factory=list, description="Generated images (when completed)"
    )
    duration_ms: float | None = Field(default=None, description="Total duration when completed")
    error: str | None = Field(default=None, description="Error message if failed")
    error_code: str | None = None
    created_at: str = Field(..., description="Task creation time ISO8601")
    completed_at: str | None = Field(default=None, description="Completion time ISO8601")
    turn_id: str | None = None
    session_id: str | None = None
    parent_artifact_id: str | None = None
    output_artifact_id: str | None = None
    client_request_id: str | None = None
    latest_advanced: bool | None = None


class ImageBlobUploadUrlRequest(BaseModel):
    filename: str = Field(default="reference.png", max_length=255)
    mime_type: str = Field(default="image/png")
    byte_size: int | None = Field(default=None, ge=1)
    content_sha256: str | None = Field(default=None, min_length=64, max_length=64)


class ImageBlobUploadUrlResponse(BaseModel):
    blob_id: str
    upload_url: str
    method: str = "PUT"
    headers: dict[str, str] = Field(default_factory=dict)
    fields: dict[str, str] | None = None
    storage_key: str
    expires_at: str


class ImageBlobCompleteRequest(BaseModel):
    blob_id: str
    content_sha256: str | None = Field(default=None, min_length=64, max_length=64)
    byte_size: int | None = Field(default=None, ge=1)
    mime_type: str | None = None


class ImageBlobResponse(BaseModel):
    blob_id: str
    status: str
    content_sha256: str | None = None
    byte_size: int | None = None
    mime_type: str
    storage_key: str


class ImageBlobFetchUrlRequest(BaseModel):
    url: str
    mime_type: str | None = None
