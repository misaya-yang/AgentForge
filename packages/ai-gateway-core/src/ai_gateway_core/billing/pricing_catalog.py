"""
Canonical model pricing catalog used across gateway services.

Prices are in USD per 1K tokens and aligned to official provider pricing pages
as of 2026-02-11.
"""

from __future__ import annotations

from decimal import Decimal
from typing import Any

# 1 USD = 100 cents = 1,000,000 microcents.
MICROCENTS_PER_USD = 1_000_000


def _d(value: str) -> Decimal:
    return Decimal(value)


DEFAULT_TOKEN_PRICING_PER_1K_USD: dict[str, dict[str, Any]] = {
    # OpenAI
    "gpt-4o": {"input": _d("0.0025"), "output": _d("0.01"), "provider": "openai"},
    "gpt-4o-mini": {"input": _d("0.00015"), "output": _d("0.0006"), "provider": "openai"},
    "o1": {"input": _d("0.015"), "output": _d("0.06"), "provider": "openai"},
    "o1-mini": {"input": _d("0.0011"), "output": _d("0.0044"), "provider": "openai"},
    "gpt-4-turbo": {"input": _d("0.01"), "output": _d("0.03"), "provider": "openai"},
    "gpt-4": {"input": _d("0.03"), "output": _d("0.06"), "provider": "openai"},
    "gpt-3.5-turbo": {"input": _d("0.0005"), "output": _d("0.0015"), "provider": "openai"},
    # Anthropic
    "claude-sonnet-4-20250514": {
        "input": _d("0.003"),
        "output": _d("0.015"),
        "provider": "anthropic",
    },
    "claude-3-5-sonnet-20241022": {
        "input": _d("0.003"),
        "output": _d("0.015"),
        "provider": "anthropic",
    },
    "claude-3-5-haiku-20241022": {
        "input": _d("0.0008"),
        "output": _d("0.004"),
        "provider": "anthropic",
    },
    "claude-3-opus": {"input": _d("0.015"), "output": _d("0.075"), "provider": "anthropic"},
    "claude-3-sonnet": {"input": _d("0.003"), "output": _d("0.015"), "provider": "anthropic"},
    "claude-3-5-sonnet": {
        "input": _d("0.003"),
        "output": _d("0.015"),
        "provider": "anthropic",
    },
    "claude-3-haiku": {"input": _d("0.00025"), "output": _d("0.00125"), "provider": "anthropic"},
    # DeepSeek (defaulting to cache-miss rate)
    "deepseek-chat": {"input": _d("0.00028"), "output": _d("0.00042"), "provider": "deepseek"},
    "deepseek-reasoner": {
        "input": _d("0.00028"),
        "output": _d("0.00042"),
        "provider": "deepseek",
    },
    "deepseek-coder": {"input": _d("0.00028"), "output": _d("0.00042"), "provider": "deepseek"},
    # DashScope / Qwen (global baseline defaults)
    "qwen-turbo": {"input": _d("0.0003"), "output": _d("0.0006"), "provider": "dashscope"},
    "qwen-plus": {"input": _d("0.0004"), "output": _d("0.0012"), "provider": "dashscope"},
    "qwen-max": {"input": _d("0.0012"), "output": _d("0.006"), "provider": "dashscope"},
    "qwen-vl-max": {"input": _d("0.00023"), "output": _d("0.000574"), "provider": "dashscope"},
    "qwen-vl-plus": {"input": _d("0.008"), "output": _d("0.008"), "provider": "dashscope"},
    "qwen3.7-plus": {"input": _d("0.0005"), "output": _d("0.003"), "provider": "dashscope"},
    "qwen3.6-plus": {"input": _d("0"), "output": _d("0"), "provider": "dashscope"},
    # Google Gemini
    "gemini-3-pro-preview": {
        "input": _d("0.002"),
        "output": _d("0.012"),
        "provider": "google",
    },
    "gemini-3-flash-preview": {
        "input": _d("0.0005"),
        "output": _d("0.003"),
        "provider": "google",
    },
    "gemini-2.5-pro": {"input": _d("0.00125"), "output": _d("0.01"), "provider": "google"},
    "gemini-2.5-flash": {"input": _d("0.0003"), "output": _d("0.0025"), "provider": "google"},
    "gemini-2.5-flash-lite": {
        "input": _d("0.0001"),
        "output": _d("0.0004"),
        "provider": "google",
    },
    "gemini-2.0-flash": {"input": _d("0.00015"), "output": _d("0.0006"), "provider": "google"},
    # Fallback
    "default": {"input": _d("0.001"), "output": _d("0.002"), "provider": "unknown"},
}

MODEL_ID_ALIASES: dict[str, str] = {
    "gemini-3.0-pro": "gemini-3-pro-preview",
    "gemini-3.0-flash": "gemini-3-flash-preview",
    "claude-3-opus-20240229": "claude-3-opus",
    "claude-3-sonnet-20240229": "claude-3-sonnet",
    "claude-3-haiku-20240307": "claude-3-haiku",
}


def _normalize_model_id(model: str) -> str:
    normalized = str(model or "").strip()
    if not normalized:
        return ""

    prefixes = (
        "models/",
        "model/",
        "model:",
        "openai/",
        "anthropic/",
        "google/",
        "vertex/",
        "deepseek/",
        "dashscope/",
        "aliyun/",
    )

    while normalized:
        matched_prefix = None
        normalized_lower = normalized.lower()
        for prefix in prefixes:
            if normalized_lower.startswith(prefix):
                matched_prefix = prefix
                break
        if matched_prefix is None:
            break
        normalized = normalized[len(matched_prefix) :].strip()

    return normalized.strip()


def resolve_pricing_with_status(model: str) -> tuple[dict[str, Any], str]:
    """Resolve pricing and classify whether the match was exact, inferred, or unknown."""
    normalized = _normalize_model_id(model)
    if not normalized:
        return DEFAULT_TOKEN_PRICING_PER_1K_USD["default"], "unknown"

    canonical = (
        MODEL_ID_ALIASES.get(normalized)
        or MODEL_ID_ALIASES.get(normalized.lower())
        or normalized
    )
    canonical_lower = canonical.lower()

    for known_model, pricing in DEFAULT_TOKEN_PRICING_PER_1K_USD.items():
        if known_model.lower() == canonical_lower:
            return pricing, "catalog"

    matched: tuple[int, dict[str, Any]] | None = None
    for known_model, pricing in DEFAULT_TOKEN_PRICING_PER_1K_USD.items():
        if known_model == "default":
            continue
        known_model_lower = known_model.lower()
        # SPO-05 gate: only "requested starts with the catalog entry" may
        # match (variants like gpt-4o-mini → gpt-4o). The reverse direction
        # is forbidden: "gpt-4" must never inherit gpt-4o pricing.
        if canonical_lower.startswith(known_model_lower):
            score = len(known_model)
            if matched is None or score > matched[0]:
                matched = (score, pricing)
    if matched:
        return matched[1], "provider_model"

    return DEFAULT_TOKEN_PRICING_PER_1K_USD["default"], "unknown"


def resolve_pricing(model: str) -> dict[str, Any]:
    """Resolve pricing for a model with aliases and prefix fallback."""
    pricing, _status = resolve_pricing_with_status(model)
    return pricing


def to_model_pricing_defaults() -> dict[str, dict[str, Any]]:
    """Build DEFAULT_PRICING shape expected by ModelPricingService."""
    return {
        model: {
            "input": float(pricing["input"]),
            "output": float(pricing["output"]),
            "provider": pricing["provider"],
        }
        for model, pricing in DEFAULT_TOKEN_PRICING_PER_1K_USD.items()
    }


def usd_to_microcents(value: Decimal | float | int | str) -> int:
    """Convert USD value to integer microcents."""
    return int(round(Decimal(str(value)) * MICROCENTS_PER_USD))


def microcents_to_usd(value: int | float | Decimal | str) -> float:
    """Convert microcents to USD."""
    return float(Decimal(str(value)) / MICROCENTS_PER_USD)


def calculate_token_cost_cents(
    model_id: str,
    input_tokens: int,
    output_tokens: int,
) -> tuple[int, str]:
    """Return total token cost in USD cents and pricing match status."""
    pricing, status = resolve_pricing_with_status(model_id)
    input_rate = Decimal(str(pricing.get("input", 0)))
    output_rate = Decimal(str(pricing.get("output", 0)))
    cost_usd = (Decimal(max(0, input_tokens)) / 1000) * input_rate + (
        Decimal(max(0, output_tokens)) / 1000
    ) * output_rate
    return max(0, int(round(cost_usd * 100))), status
