"""
Response models for the AI Assistant SDK.

These dataclasses represent the JSON payloads returned by the gateway.
Factory methods ``from_dict`` allow safe construction from raw dicts
without crashing on extra / missing fields.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

# ---------------------------------------------------------------------------
# Chat
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class ChatResponse:
    """Response from a non-streaming chat completion."""

    content: str
    session_id: str
    model_id: str | None = None
    run_id: str | None = None
    usage: dict[str, int] | None = None
    duration_ms: float | None = None
    contexts: list[dict[str, Any]] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ChatResponse:
        return cls(
            content=data.get("content", ""),
            session_id=data.get("session_id", ""),
            model_id=data.get("model_id"),
            run_id=data.get("run_id"),
            usage=data.get("usage"),
            duration_ms=data.get("duration_ms"),
            contexts=data.get("contexts"),
        )


# ---------------------------------------------------------------------------
# Sessions
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class Session:
    """An assistant conversation session."""

    session_id: str
    user_id: str = ""
    tenant_id: str = ""
    service_id: str | None = None
    created_at: str | None = None
    updated_at: str | None = None
    metadata: dict[str, Any] | None = None
    message_count: int = 0

    @property
    def title(self) -> str | None:
        if self.metadata and "title" in self.metadata:
            return str(self.metadata["title"])
        return None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Session:
        return cls(
            session_id=data.get("session_id", ""),
            user_id=data.get("user_id", ""),
            tenant_id=data.get("tenant_id", ""),
            service_id=data.get("service_id"),
            created_at=data.get("created_at"),
            updated_at=data.get("updated_at"),
            metadata=data.get("metadata"),
            message_count=data.get("message_count", 0),
        )


# ---------------------------------------------------------------------------
# Messages
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class Message:
    """A single message in a session's history."""

    role: str
    content: str
    timestamp: str | None = None
    metadata: dict[str, Any] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Message:
        return cls(
            role=data.get("role", ""),
            content=data.get("content", ""),
            timestamp=data.get("timestamp"),
            metadata=data.get("metadata"),
        )


# ---------------------------------------------------------------------------
# Artifacts
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class Artifact:
    """A generated or uploaded artifact (image, document, code, etc.)."""

    artifact_id: str
    session_id: str
    type: str  # image, document, chart, code, file
    format: str  # png, jpg, pdf, json, etc.
    title: str
    filename: str
    size_bytes: int = 0
    mime_type: str | None = None
    source: str | None = None  # ai, user, code_execution, image_generation, document_generation
    storage_key: str | None = None
    message_id: str | None = None
    tenant_id: str | None = None
    user_id: str | None = None
    metadata: dict[str, Any] | None = None
    download_url: str | None = None
    created_at: str | None = None
    updated_at: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Artifact:
        return cls(
            artifact_id=data.get("artifact_id", ""),
            session_id=data.get("session_id", ""),
            type=data.get("type", ""),
            format=data.get("format", ""),
            title=data.get("title", ""),
            filename=data.get("filename", ""),
            size_bytes=data.get("size_bytes", 0),
            mime_type=data.get("mime_type"),
            source=data.get("source"),
            storage_key=data.get("storage_key"),
            message_id=data.get("message_id"),
            tenant_id=data.get("tenant_id"),
            user_id=data.get("user_id"),
            metadata=data.get("metadata"),
            download_url=data.get("download_url"),
            created_at=data.get("created_at"),
            updated_at=data.get("updated_at"),
        )


# ---------------------------------------------------------------------------
# Images
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class GeneratedImage:
    """A single generated image."""

    url: str
    width: int | None = None
    height: int | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> GeneratedImage:
        return cls(
            url=data.get("url", ""),
            width=data.get("width"),
            height=data.get("height"),
        )


@dataclass(slots=True)
class ImageResult:
    """Result of an image generation request."""

    success: bool
    images: list[GeneratedImage] = field(default_factory=list)
    provider: str = ""
    duration_ms: float = 0.0
    error: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ImageResult:
        images = [GeneratedImage.from_dict(img) for img in data.get("images", [])]
        return cls(
            success=data.get("success", False),
            images=images,
            provider=data.get("provider", ""),
            duration_ms=data.get("duration_ms", 0.0),
            error=data.get("error"),
        )


# ---------------------------------------------------------------------------
# Knowledge / Datasets
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class Dataset:
    """A knowledge-base dataset available for RAG queries."""

    dataset_id: str
    name: str
    description: str | None = None
    document_count: int = 0
    chunk_count: int = 0
    embedding_model: str | None = None
    is_multimodal: bool = False

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Dataset:
        return cls(
            dataset_id=data.get("dataset_id", ""),
            name=data.get("name", ""),
            description=data.get("description"),
            document_count=data.get("document_count", 0),
            chunk_count=data.get("chunk_count", 0),
            embedding_model=data.get("embedding_model"),
            is_multimodal=data.get("is_multimodal", False),
        )


# ---------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class ToolInfo:
    """Information about an available tool / skill."""

    name: str
    description: str = ""
    enabled: bool = True
    category: str | None = None
    parameters: dict[str, Any] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ToolInfo:
        return cls(
            name=data.get("name", ""),
            description=data.get("description", ""),
            enabled=data.get("enabled", True),
            category=data.get("category"),
            parameters=data.get("parameters"),
        )


# ---------------------------------------------------------------------------
# Models (LLM catalogue)
# ---------------------------------------------------------------------------

@dataclass(slots=True)
class ModelInfo:
    """Metadata for an available LLM model."""

    id: str
    name: str
    provider: str
    context_window: int = 0
    max_output_tokens: int = 0
    supports_vision: bool = False
    supports_tools: bool = False
    access_level: str | None = None
    input_price_per_1k: float | None = None
    output_price_per_1k: float | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ModelInfo:
        return cls(
            id=data.get("id", ""),
            name=data.get("name", ""),
            provider=data.get("provider", ""),
            context_window=data.get("context_window", 0),
            max_output_tokens=data.get("max_output_tokens", 0),
            supports_vision=data.get("supports_vision", False),
            supports_tools=data.get("supports_tools", False),
            access_level=data.get("access_level"),
            input_price_per_1k=data.get("input_price_per_1k"),
            output_price_per_1k=data.get("output_price_per_1k"),
        )
