"""Shared provider reasoning wire serialization.

Profiles own option data; this adapter only serializes validated settings. It
is imported by both the Python control runtime and the private Agent model
plane so a capability revision cannot produce two different outbound bodies.
"""

from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any

from .capabilities import ResolvedReasoningOption, resolve_reasoning_option


class ReasoningWireError(ValueError):
    """A capability profile cannot be represented on the selected wire API."""


def _claim_path(body: dict[str, Any], path: tuple[str, ...], value: Any) -> None:
    target = body
    for key in path[:-1]:
        current = target.get(key)
        if current is None:
            current = {}
            target[key] = current
        if not isinstance(current, dict):
            raise ReasoningWireError(f"capability path conflicts at {'.'.join(path)}")
        target = current
    leaf = path[-1]
    if leaf in target:
        raise ReasoningWireError(f"capability path already owned: {'.'.join(path)}")
    target[leaf] = copy.deepcopy(value)


def apply_reasoning_wire(
    body: dict[str, Any],
    profile: Mapping[str, Any],
    requested_option: str | None,
) -> ResolvedReasoningOption:
    resolved = resolve_reasoning_option(profile, requested_option)
    settings = resolved.settings
    adapter_id = resolved.adapter_id

    if adapter_id == "reasoning/none-v1":
        return resolved
    if adapter_id == "reasoning/dashscope-thinking-v1":
        enabled = bool(settings["enabled"])
        _claim_path(body, ("enable_thinking",), enabled)
        budget = settings.get("budget_tokens")
        if enabled and budget is not None:
            _claim_path(body, ("thinking_budget",), int(budget))
        if enabled:
            token_field = "max_output_tokens" if "input" in body else "max_tokens"
            if body.get(token_field) is None:
                body[token_field] = 16384
        return resolved
    if adapter_id == "reasoning/dashscope-responses-effort-v1":
        if "input" in body:
            _claim_path(body, ("reasoning",), {"effort": str(settings["effort"])})
            return resolved
        enabled = bool(settings["chat_enabled"])
        _claim_path(body, ("enable_thinking",), enabled)
        budget = settings.get("chat_budget_tokens")
        if enabled and budget is not None:
            _claim_path(body, ("thinking_budget",), int(budget))
        return resolved
    if adapter_id == "reasoning/deepseek-thinking-effort-v1":
        enabled = bool(settings["enabled"])
        if enabled:
            body.pop("temperature", None)
            body.pop("top_p", None)
        _claim_path(body, ("thinking",), {"type": "enabled" if enabled else "disabled"})
        if enabled and settings.get("effort"):
            _claim_path(body, ("reasoning_effort",), settings["effort"])
        return resolved
    if adapter_id == "reasoning/anthropic-adaptive-v1":
        enabled = bool(settings["enabled"])
        if enabled:
            body.pop("temperature", None)
        _claim_path(body, ("thinking",), {"type": "adaptive" if enabled else "disabled"})
        if enabled and settings.get("effort"):
            _claim_path(body, ("output_config",), {"effort": settings["effort"]})
        return resolved
    if adapter_id == "reasoning/anthropic-budget-v1":
        enabled = bool(settings["enabled"])
        if enabled:
            body.pop("temperature", None)
            _claim_path(
                body,
                ("thinking",),
                {"type": "enabled", "budget_tokens": int(settings["budget_tokens"])},
            )
        else:
            _claim_path(body, ("thinking",), {"type": "disabled"})
        return resolved
    if adapter_id == "reasoning/openai-responses-effort-v1":
        effort = str(settings["effort"])
        if "input" in body:
            _claim_path(body, ("reasoning",), {"effort": effort, "summary": "auto"})
        else:
            _claim_path(body, ("reasoning_effort",), effort)
        return resolved
    if adapter_id == "reasoning/gemini-thinking-v1":
        thinking: dict[str, Any] = {}
        if settings.get("level") is not None:
            thinking["thinkingLevel"] = str(settings["level"]).upper()
        if settings.get("budget_tokens") is not None:
            thinking["thinkingBudget"] = int(settings["budget_tokens"])
        if settings.get("include_thoughts") is not None:
            thinking["includeThoughts"] = bool(settings["include_thoughts"])
        if thinking:
            _claim_path(body, ("generationConfig", "thinkingConfig"), thinking)
        return resolved
    if adapter_id == "reasoning/xai-effort-v1":
        effort = str(settings["effort"])
        if "input" in body:
            _claim_path(body, ("reasoning",), {"effort": effort})
        else:
            _claim_path(body, ("reasoning_effort",), effort)
        return resolved
    if adapter_id == "reasoning/openai-compatible-binary-v1":
        _claim_path(body, ("enable_thinking",), bool(settings["enabled"]))
        return resolved
    raise ReasoningWireError(f"unsupported reasoning adapter: {adapter_id}")


__all__ = ["ReasoningWireError", "apply_reasoning_wire"]
