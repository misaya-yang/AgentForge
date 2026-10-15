"""
Request models for the AI Assistant SDK.

All models are plain dataclasses (no Pydantic dependency) with full type
annotations and sensible defaults matching the gateway's expectations.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any, Literal


@dataclass(slots=True)
class ChatRequest:
    """Parameters for a chat completion (streaming or non-streaming)."""

    message: str
    """The user message to send."""

    session_id: str | None = None
    """Existing session to continue.  Omit to let the server create one."""

    history: list[dict[str, str]] | None = None
    """Optional explicit message history (list of ``{role, content}``)."""

    model_id: str | None = None
    """LLM model identifier. Omit to use the server's deployment default —
    override per request for other registered models."""

    temperature: float = 0.7
    max_tokens: int | None = None
    system_prompt: str | None = None

    # Knowledge-base (RAG) options
    kb_dataset_ids: list[str] | None = None
    kb_mode: Literal["auto", "tool", "off"] | None = None
    kb_top_k: int | None = None
    kb_score_threshold: float | None = None
    kb_include_images: bool | None = None

    # Web search
    web_search_enabled: bool = False
    web_search_max_results: int | None = None

    # Files / multimodal
    file_paths: list[str] | None = None

    # Execution profile
    execution_profile: Literal["safe", "balanced", "power"] | None = None

    # Memory
    memory_mode: Literal["auto", "strict", "off"] | None = None

    # OS Agent
    os_agent_enabled: bool | None = None

    # Internal — set automatically by ChatModule
    stream: bool = True

    def to_dict(self) -> dict[str, Any]:
        """Serialise to a JSON-safe dict, omitting ``None`` values."""
        return {k: v for k, v in asdict(self).items() if v is not None}


@dataclass(slots=True)
class SessionCreateRequest:
    """Parameters for creating a new session."""

    title: str | None = None
    metadata: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {}
        meta: dict[str, Any] = dict(self.metadata) if self.metadata else {}
        if self.title:
            meta["title"] = self.title
        if meta:
            payload["metadata"] = meta
        return payload


@dataclass(slots=True)
class ImageGenerationRequest:
    """Parameters for image generation via smart routing."""

    prompt: str
    model_id: str | None = None
    style: str | None = None
    size: str | None = None
    n: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {k: v for k, v in asdict(self).items() if v is not None}


@dataclass(slots=True)
class CreateArtifactRequest:
    """Parameters for creating an artifact from base64-encoded content."""

    session_id: str
    type: str  # image, document, chart, code, file
    format: str  # png, jpg, pdf, json, etc.
    title: str
    filename: str
    content_base64: str
    source: str | None = None  # ai, user, code_execution, image_generation
    message_id: str | None = None
    metadata: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        return {k: v for k, v in asdict(self).items() if v is not None}
