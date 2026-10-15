"""
AI Assistant Python SDK
=======================

A production-grade async SDK for the AI Gateway's REST API and SSE streaming.

Quick start::

    from ai_assistant import AssistantClient

    async with AssistantClient("my-api-key", base_url="https://gateway.example.com") as client:
        # Non-streaming
        response = await client.chat.send("Hello!")
        print(response.content)

        # Streaming
        async for event in client.chat.stream("Tell me a story"):
            if event.is_text():
                print(event.text, end="", flush=True)
"""

__version__ = "1.0.1"

from ai_assistant.client import AssistantClient
from ai_assistant.exceptions import (
    AssistantError,
    AuthError,
    NotFoundError,
    RateLimitError,
    ServerError,
    StreamError,
    TimeoutError,
    UnknownOutcomeError,
    ValidationError,
)
from ai_assistant.models.events import EventType, StreamEvent

__all__ = [
    # Version
    "__version__",
    # Main client
    "AssistantClient",
    # Events
    "EventType",
    "StreamEvent",
    # Exceptions
    "AssistantError",
    "AuthError",
    "NotFoundError",
    "RateLimitError",
    "ServerError",
    "StreamError",
    "TimeoutError",
    "UnknownOutcomeError",
    "ValidationError",
]
