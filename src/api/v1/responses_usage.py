"""Usage normalization for the OpenAI-compatible Responses endpoint."""

from typing import Any

from ...services.agent_runtime.model_plane import _estimate_tokens


def responses_usage(
    value: dict[str, Any] | None,
    *,
    input_value: Any = None,
    output_text: str = "",
) -> dict[str, Any]:
    value = value if isinstance(value, dict) else {}

    def token(*names: str) -> int:
        for name in names:
            raw = value.get(name)
            if isinstance(raw, int) and not isinstance(raw, bool) and raw >= 0:
                return raw
        return 0

    input_tokens = token("input_tokens", "prompt_tokens")
    output_tokens = token("output_tokens", "completion_tokens")
    if input_tokens == 0 and input_value not in (None, ""):
        input_tokens = _estimate_tokens(input_value)
    if output_tokens == 0 and output_text:
        output_tokens = _estimate_tokens(output_text)
    details = value.get("input_tokens_details")
    cached_tokens = token("cached_input_tokens")
    if isinstance(details, dict):
        raw_cached = details.get("cached_tokens")
        if isinstance(raw_cached, int) and not isinstance(raw_cached, bool) and raw_cached >= 0:
            cached_tokens = raw_cached
    cached_tokens = min(cached_tokens, input_tokens)
    return {
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": cached_tokens},
        "output_tokens": output_tokens,
        "output_tokens_details": {"reasoning_tokens": token("reasoning_tokens")},
        "total_tokens": input_tokens + output_tokens,
    }
