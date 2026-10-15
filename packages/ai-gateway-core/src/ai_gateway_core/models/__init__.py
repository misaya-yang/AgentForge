"""Shared LLM-chat model types used by Gateway and Agent Runtime adapters."""

from .capabilities import (
    CHAT_COMPLETIONS_WIRE_PROTOCOL,
    RESERVED_OPTION_IDS,
    RESPONSES_V1_WIRE_PROTOCOL,
    SUPPORTED_WIRE_PROTOCOLS,
    ModelCapabilityError,
    ResolvedReasoningOption,
    get_builtin_model_capabilities,
    get_model_capability_adapter,
    list_model_capability_adapters,
    merge_model_capability_profiles,
    resolve_reasoning_option,
    safe_model_capability_profile,
    validate_model_capability_profile,
)
from .chat_message import ChatMessage, normalize_chat_message
from .reasoning_wire import ReasoningWireError, apply_reasoning_wire

__all__ = [
    "CHAT_COMPLETIONS_WIRE_PROTOCOL",
    "ChatMessage",
    "ModelCapabilityError",
    "RESERVED_OPTION_IDS",
    "RESPONSES_V1_WIRE_PROTOCOL",
    "ResolvedReasoningOption",
    "ReasoningWireError",
    "SUPPORTED_WIRE_PROTOCOLS",
    "apply_reasoning_wire",
    "get_builtin_model_capabilities",
    "get_model_capability_adapter",
    "list_model_capability_adapters",
    "merge_model_capability_profiles",
    "normalize_chat_message",
    "resolve_reasoning_option",
    "safe_model_capability_profile",
    "validate_model_capability_profile",
]
