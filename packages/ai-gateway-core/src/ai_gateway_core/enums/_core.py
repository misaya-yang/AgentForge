from enum import Enum


class ServiceType(str, Enum):
    CONVERSATIONAL = "conversational"
    GENERATIVE = "generative"
    PROCESSING = "processing"
    EMBEDDING = "embedding"
    CLASSIFICATION = "classification"
    LANGGRAPH = "langgraph"
    CUSTOM = "custom"


class InvocationMode(str, Enum):
    SYNC = "sync"
    ASYNC = "async"
    STREAM = "stream"
    WEBHOOK = "webhook"


class TransportType(str, Enum):
    HTTP = "http"
    GRPC = "grpc"
    WEBSOCKET = "websocket"
    MESSAGE_QUEUE = "mq"
    IN_PROCESS = "in_process"


# Compatibility alias for SDK/plugins released before the transport rename.
ConnectorType = TransportType


class ContentType(str, Enum):
    TEXT = "text"
    IMAGE = "image"
    AUDIO = "audio"
    VIDEO = "video"
    FILE = "file"
    JSON = "json"
    TOOL_CALL = "tool_call"
    TOOL_RESULT = "tool_result"


class StreamEventType(str, Enum):
    """Streaming event types.

    AG-UI Protocol compatible event types for Manus-style workflow visualization.
    Reference: https://docs.ag-ui.com/concepts/architecture
    """

    # === Legacy events (backwards compatible) ===
    TEXT_DELTA = "text_delta"
    TOOL_CALL_START = "tool_call_start"
    TOOL_CALL_DELTA = "tool_call_delta"
    TOOL_CALL_END = "tool_call_end"
    TOOL_RESULT = "tool_result"
    THINKING = "thinking"
    FINAL = "final"
    STREAM_END = "stream_end"

    # === AG-UI Lifecycle Events ===
    RUN_STARTED = "run_started"  # Agent execution begins
    RUN_FINISHED = "run_finished"  # Agent execution completed successfully
    RUN_ERROR = "run_error"  # Agent execution failed
    STEP_STARTED = "step_started"  # Sub-task/step begins
    STEP_FINISHED = "step_finished"  # Sub-task/step completed

    # === AG-UI Text Message Events ===
    TEXT_MESSAGE_START = "text_message_start"  # Begin new message
    TEXT_MESSAGE_CONTENT = "text_message_content"  # Stream text token
    TEXT_MESSAGE_END = "text_message_end"  # End message

    # === AG-UI Tool Call Events ===
    TOOL_CALL_ARGS = "tool_call_args"  # Stream tool arguments
    TOOL_CALL_RESULT = "tool_call_result"  # Tool execution result

    # === AG-UI State Management Events ===
    STATE_SNAPSHOT = "state_snapshot"  # Full state snapshot
    STATE_DELTA = "state_delta"  # Incremental state update (JSON Patch)
    MESSAGES_SNAPSHOT = "messages_snapshot"  # Full conversation history

    # === AG-UI Special Events ===
    RAW_EVENT = "raw_event"  # Passthrough external events
    CUSTOM_EVENT = "custom_event"  # Application-specific events

    # === Custom Events for AI Gateway ===
    ARTIFACT_CREATED = "artifact_created"  # File/artifact generated
    FILE_CREATING = "file_creating"  # File creation in progress
    FILE_CREATED = "file_created"  # File creation completed
    SEARCH_STARTED = "search_started"  # Search operation started
    SEARCH_PROGRESS = "search_progress"  # Search progress update
    SEARCH_COMPLETED = "search_completed"  # Search completed

    # === PPT/Document Generation Events (Manus-style) ===
    OUTLINE_READY = "outline_ready"  # Slide outline parsed from content
    DOCUMENT_GENERATION_START = "document_generation_start"  # Doc/PPT generation started
    DOCUMENT_GENERATION_RESULT = "document_generation_result"  # Doc/PPT generation result

    # === Code Execution Events ===
    CODE_EXECUTION_START = "code_execution_start"  # Code execution started
    CODE_EXECUTION_RESULT = "code_execution_result"  # Code execution result

    # === Image Generation Events ===
    IMAGE_GENERATION_START = "image_generation_start"  # Image generation started
    IMAGE_GENERATION_RESULT = "image_generation_result"  # Image generation result

    # === Tool Error Event ===
    TOOL_ERROR = "tool_error"  # Tool execution error with recovery info

    # === ReAct Thinking Events (for transparent reasoning) ===
    THINKING_START = "thinking_start"  # Start of LLM reasoning
    THINKING_DELTA = "thinking_delta"  # Streaming thinking content
    THINKING_END = "thinking_end"  # End of thinking with full content
    THINKING_ERROR = "thinking_error"  # Thinking generation failed

    # === ReAct Phase Status ===
    STATUS = "status"  # Phase status update (analyzing, thinking, executing, observing)

    # === Agent Loop Phase Events (Phase 1 Optimization) ===
    PHASE_STARTED = "phase_started"  # AgentLoop phase started
    PHASE_COMPLETED = "phase_completed"  # AgentLoop phase completed
    PHASE_PROGRESS = "phase_progress"  # AgentLoop phase progress update

    # === Enhanced Error Events (Phase 1 Optimization) ===
    ERROR = "error"  # Structured error event with severity
    BUDGET_WARNING = "budget_warning"  # Token budget warning (80% threshold)
    BUDGET_EXCEEDED = "budget_exceeded"  # Token budget exceeded, stopping
    CANCELLED = "cancelled"  # Task cancelled by user

    # === Context Retrieved (for RAG visualization) ===
    CONTEXT_RETRIEVED = "context_retrieved"  # RAG context retrieved
    WEB_SEARCH_RESULTS = "web_search_results"  # Web search results received
    RAG_EVALUATION = "rag_evaluation"  # RAG quality evaluation result
    CONTEXT_BUDGET = "context_budget"  # Context budget usage snapshot
    CONTEXT_COMPACTED = "context_compacted"  # Context compaction details
    CONTEXT_DETAIL = "context_detail"  # Context cost contributor detail
    MEMORY_RETRIEVED = "memory_retrieved"  # Hybrid memory retrieval summary
    MEMORY_REFLECTION_SCHEDULED = "memory_reflection_scheduled"  # Reflection job scheduled
    QUEUE_STATE = "queue_state"  # Assistant command queue state
    QUEUE_STEERED = "queue_steered"  # Queue steering mode update
    APPROVAL_REQUIRED = "approval_required"  # Tool call requires approval
    APPROVAL_RESULT = "approval_result"  # Approval accepted/rejected
    GATEWAY_DECISION = "gateway_decision"  # Gateway policy decision
    SANDBOX_DECISION = "sandbox_decision"  # Sandbox policy result
    SKILL_SELECTED = "skill_selected"  # Skill selected for request
    SKILL_LOADED = "skill_loaded"  # Skill metadata loaded
    SKILL_CREATE_PENDING_APPROVAL = "skill_create_pending_approval"  # Skill proposal pending
    MEMORY_LOADED = "memory_loaded"  # Memory loading completed
    DONE = "done"  # Execution complete


class DatasetVisibility(str, Enum):
    PRIVATE = "private"
    TENANT = "tenant"
    PUBLIC = "public"


class DocumentStatus(str, Enum):
    UPLOADED = "uploaded"
    PARSING = "parsing"
    CLEANING = "cleaning"
    SPLITTING = "splitting"
    SEGMENTING = "segmenting"  # alias for splitting
    EMBEDDING = "embedding"
    INDEXING = "indexing"
    COMPLETED = "completed"
    FAILED = "failed"
    PAUSED = "paused"


class SegmentStatus(str, Enum):
    """Segment indexing status"""

    WAITING = "waiting"
    INDEXING = "indexing"
    COMPLETED = "completed"
    ERROR = "error"


class DatasetPermission(str, Enum):
    OWNER = "owner"
    EDITOR = "editor"
    VIEWER = "viewer"


class RetrievalMethod(str, Enum):
    """Retrieval method types - matches Dify's RetrievalMethod"""

    SEMANTIC_SEARCH = "semantic_search"
    FULL_TEXT_SEARCH = "full_text_search"
    HYBRID_SEARCH = "hybrid_search"


class DataSourceType(str, Enum):
    """Document data source types"""

    UPLOAD_FILE = "upload_file"
    NOTION_IMPORT = "notion_import"
    WEBSITE_CRAWL = "website_crawl"
    TEXT_INPUT = "text"
    URL_IMPORT = "url"


class ModelProvider(str, Enum):
    """Supported LLM providers shared across Gateway and model adapters
    so gateway routes (e.g. ``/health/providers``) can enumerate them
    without importing from a runtime implementation."""

    OPENAI = "openai"
    ANTHROPIC = "anthropic"
    DEEPSEEK = "deepseek"
    DASHSCOPE = "dashscope"
    GOOGLE = "google"
    GOOGLE_VERTEX = "google-vertex"


class ModelAccessLevel(str, Enum):
    """Model access permission tiers. Shared to keep gateway's admin
    role-check consistent with Runtime gating."""

    PUBLIC = "public"
    PREMIUM = "premium"
    ADMIN = "admin"


class RAGMode(str, Enum):
    """RAG behavior modes used by the assistant chat flow.

    ``AUTO`` = retrieve on every user turn. ``TOOL`` = expose KB as a
    callable tool and let the model decide. ``DISABLED`` = no KB
    retrieval regardless of dataset selection."""

    AUTO = "auto"
    TOOL = "tool"
    DISABLED = "off"


class StylePreset(str, Enum):
    """User-facing image style presets accepted by ``/generate-image``.

    Values are the wire-format strings exchanged with the frontend; keep
    original naming (``3d_render`` not ``render_3d``)."""

    DEFAULT = "default"
    REALISTIC = "realistic"
    ANIME = "anime"
    ABSTRACT = "abstract"
    OIL_PAINT = "oil_paint"
    WATERCOLOR = "watercolor"
    RENDER_3D = "3d_render"
    PIXEL_ART = "pixel_art"
    SKETCH = "sketch"
    COMIC = "comic"


class ToolCategory(str, Enum):
    """Tool categorisation used by the assistant tool registry + admin policy UI.

    Values are the serialised strings exchanged over the wire; do not
    rename without updating frontend filters."""

    RETRIEVAL = "retrieval"
    GENERATION = "generation"
    ANALYSIS = "analysis"
    INTEGRATION = "integration"
    UTILITY = "utility"
    SKILL = "skill"
    MCP = "mcp"
