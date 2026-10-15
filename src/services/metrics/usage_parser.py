from __future__ import annotations

from collections import deque
from collections.abc import Iterable
from typing import Any

# Common token field variants across providers.
_INPUT_TOKEN_KEYS = (
    "input_tokens",
    "prompt_tokens",
    "promptTokenCount",
    "inputTokenCount",
)
_OUTPUT_TOKEN_KEYS = (
    "output_tokens",
    "completion_tokens",
    "candidatesTokenCount",
    "outputTokenCount",
)
_TOTAL_TOKEN_KEYS = (
    "total_tokens",
    "totalTokenCount",
)
_ALL_TOKEN_KEYS = set(_INPUT_TOKEN_KEYS + _OUTPUT_TOKEN_KEYS + _TOTAL_TOKEN_KEYS)

_MODEL_KEYS = (
    "model",
    "model_name",
    "model_id",
    "response_model",
    "gen_ai.response.model",
    "gen_ai.request.model",
)
_PROVIDER_KEYS = (
    "provider",
    "provider_name",
    "vendor",
    "gen_ai.provider.name",
)
_ASSISTANT_KEYS = (
    "assistant_id",
    "graph_id",
)


def _to_int(value: Any) -> int | None:
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str):
        raw = value.strip()
        if not raw:
            return None
        try:
            return int(float(raw))
        except ValueError:
            return None
    return None


def _to_text(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, str):
        cleaned = value.strip()
        return cleaned or None
    return None


def _iter_nodes(payload: Any) -> Iterable[Any]:
    queue: deque[Any] = deque([payload])
    seen_ids: set[int] = set()

    while queue:
        current = queue.popleft()
        if isinstance(current, (dict, list, tuple)):
            current_id = id(current)
            if current_id in seen_ids:
                continue
            seen_ids.add(current_id)
        yield current

        if isinstance(current, dict):
            queue.extend(current.values())
        elif isinstance(current, (list, tuple)):
            queue.extend(current)


def _normalize_usage_dict(data: dict[str, Any]) -> dict[str, int] | None:
    if not isinstance(data, dict):
        return None
    if not any(key in data for key in _ALL_TOKEN_KEYS):
        return None

    input_tokens = 0
    output_tokens = 0
    total_tokens: int | None = None

    for key in _INPUT_TOKEN_KEYS:
        parsed = _to_int(data.get(key))
        if parsed is not None:
            input_tokens = max(input_tokens, parsed)
            break
    for key in _OUTPUT_TOKEN_KEYS:
        parsed = _to_int(data.get(key))
        if parsed is not None:
            output_tokens = max(output_tokens, parsed)
            break
    for key in _TOTAL_TOKEN_KEYS:
        parsed = _to_int(data.get(key))
        if parsed is not None:
            total_tokens = parsed
            break

    if total_tokens is None:
        total_tokens = input_tokens + output_tokens
    if total_tokens < input_tokens + output_tokens:
        total_tokens = input_tokens + output_tokens

    return {
        "input_tokens": max(input_tokens, 0),
        "output_tokens": max(output_tokens, 0),
        "total_tokens": max(total_tokens, 0),
    }


def extract_token_usage(payload: Any) -> dict[str, int] | None:
    best: dict[str, int] | None = None
    best_total = -1

    for node in _iter_nodes(payload):
        if not isinstance(node, dict):
            continue
        normalized = _normalize_usage_dict(node)
        if not normalized:
            continue
        total = normalized["total_tokens"]
        if total > best_total:
            best = normalized
            best_total = total

    return best


def extract_string_value(payload: Any, keys: Iterable[str]) -> str | None:
    ordered_keys = tuple(keys)
    for node in _iter_nodes(payload):
        if not isinstance(node, dict):
            continue
        for key in ordered_keys:
            value = _to_text(node.get(key))
            if value:
                return value
    return None


def _extract_gateway_model_field(payload: Any, keys: Iterable[str]) -> str | None:
    ordered_keys = tuple(keys)
    for node in _iter_nodes(payload):
        if not isinstance(node, dict):
            continue
        gateway_model = node.get("gateway_model")
        if not isinstance(gateway_model, dict):
            continue
        for key in ordered_keys:
            value = _to_text(gateway_model.get(key))
            if value:
                return value
    return None


def extract_model(payload: Any) -> str | None:
    return _extract_gateway_model_field(payload, ("model", "model_id")) or extract_string_value(
        payload, _MODEL_KEYS
    )


def extract_provider(payload: Any) -> str | None:
    return _extract_gateway_model_field(payload, ("provider", "provider_id")) or extract_string_value(
        payload, _PROVIDER_KEYS
    )


def extract_assistant_id(payload: Any) -> str | None:
    return extract_string_value(payload, _ASSISTANT_KEYS)
