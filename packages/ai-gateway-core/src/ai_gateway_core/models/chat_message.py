"""Shared ``ChatMessage`` dataclass.

Canonical home for the wire-level LLM chat-message shape used by service-neutral
downstream modules.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class ChatMessage:
    """A chat message passed to an LLM provider.

    Fields match the OpenAI wire format. ``thought_signature`` is a
    Gemini-3 extension carrying the encrypted reasoning pointer that must
    ride alongside the turn for multi-turn function calling."""

    role: str
    content: str
    name: str | None = None
    tool_calls: list[dict[str, Any]] | None = None
    tool_call_id: str | None = None
    images: list[str] | None = None
    thought_signature: str | None = None
    # Provider-native assistant content blocks that must be replayed verbatim
    # for protocols such as Anthropic server-tool pause/resume. Other
    # providers ignore this optional extension.
    provider_content_blocks: list[dict[str, Any]] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ChatMessage:
        return cls(
            role=data.get("role", "user"),
            content=data.get("content", ""),
            name=data.get("name"),
            tool_calls=data.get("tool_calls"),
            tool_call_id=data.get("tool_call_id"),
            images=data.get("images"),
            thought_signature=data.get("thought_signature"),
            provider_content_blocks=data.get("provider_content_blocks"),
        )


def normalize_chat_message(msg: Any) -> ChatMessage:
    """Coerce a dict or ChatMessage into a ChatMessage.

    Used by ``ModelRegistry.chat_completion`` to accept either shape from
    callers can pass native dicts without materialising the dataclass first."""
    if isinstance(msg, ChatMessage):
        return msg
    if isinstance(msg, dict):
        return ChatMessage.from_dict(msg)
    raise TypeError(f"Expected ChatMessage or dict, got {type(msg)}")
