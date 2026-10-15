"""Public model types for the AI Assistant SDK."""

from ai_assistant.models.events import EventType, StreamEvent
from ai_assistant.models.request import (
    ChatRequest,
    CreateArtifactRequest,
    ImageGenerationRequest,
    SessionCreateRequest,
)
from ai_assistant.models.response import (
    Artifact,
    ChatResponse,
    Dataset,
    GeneratedImage,
    ImageResult,
    Message,
    ModelInfo,
    Session,
    ToolInfo,
)

__all__ = [
    # Events
    "EventType",
    "StreamEvent",
    # Requests
    "ChatRequest",
    "CreateArtifactRequest",
    "ImageGenerationRequest",
    "SessionCreateRequest",
    # Responses
    "Artifact",
    "ChatResponse",
    "Dataset",
    "GeneratedImage",
    "ImageResult",
    "Message",
    "ModelInfo",
    "Session",
    "ToolInfo",
]
