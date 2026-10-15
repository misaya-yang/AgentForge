from __future__ import annotations

import random
import re
from collections.abc import Iterable
from typing import Any

ERROR_TYPE_TIMEOUT = "timeout"
ERROR_TYPE_PROVIDER = "provider_error"
ERROR_TYPE_RATE_LIMIT = "rate_limit"
ERROR_TYPE_AUTH = "auth_error"
ERROR_TYPE_TOOL = "tool_error"
ERROR_TYPE_CONTENT_FILTER = "content_filter"
ERROR_TYPE_UNKNOWN = "unknown"

KNOWN_ERROR_TYPES = {
    ERROR_TYPE_TIMEOUT,
    ERROR_TYPE_PROVIDER,
    ERROR_TYPE_RATE_LIMIT,
    ERROR_TYPE_AUTH,
    ERROR_TYPE_TOOL,
    ERROR_TYPE_CONTENT_FILTER,
    ERROR_TYPE_UNKNOWN,
}


def classify_error_type(
    *,
    status: str,
    status_code: int | None = None,
    upstream_error_type: str = "",
    upstream_error_message: str = "",
) -> str | None:
    """Classify request errors into dashboard-friendly buckets."""
    normalized_status = (status or "").lower().strip()
    if normalized_status == "success" and (status_code is None or status_code < 400):
        return None

    et = (upstream_error_type or "").lower()
    msg = (upstream_error_message or "").lower()
    combined = f"{et} {msg}"

    if status_code in {408, 504} or _contains_any(
        combined, ("timeout", "timed out", "deadline exceeded")
    ):
        return ERROR_TYPE_TIMEOUT

    if status_code == 429 or _contains_any(
        combined,
        ("rate limit", "too many requests", "quota", "throttle", "resource exhausted", "429"),
    ):
        return ERROR_TYPE_RATE_LIMIT

    if status_code in {401, 403} or _contains_any(
        combined,
        ("auth", "unauthorized", "forbidden", "permission denied", "invalid api key", "token"),
    ):
        return ERROR_TYPE_AUTH

    if _contains_any(
        combined,
        (
            "content filter",
            "safety",
            "blocked by policy",
            "moderation",
            "harm",
            "policy violation",
        ),
    ):
        return ERROR_TYPE_CONTENT_FILTER

    if _contains_any(combined, ("tool", "function call", "function_call", "tool_call")):
        return ERROR_TYPE_TOOL

    if status_code is not None and status_code >= 500:
        return ERROR_TYPE_PROVIDER

    if _contains_any(
        combined, ("upstream", "provider", "gateway", "bad gateway", "service unavailable")
    ):
        return ERROR_TYPE_PROVIDER

    return ERROR_TYPE_UNKNOWN


def extract_duration_breakdown(payload: Any) -> dict[str, Any]:
    """
    Extract latency breakdown fields from nested payload structures.

    Returns a dict with all expected breakdown keys; unknown values are omitted.
    """
    if payload is None:
        return {}

    flat: dict[str, Any] = {}
    _flatten(payload, flat)

    tool_breakdown = _extract_tool_breakdown(payload)

    result: dict[str, Any] = {}
    first_token = _pick_number(
        flat,
        (
            "first_token_latency_ms",
            "first_token_ms",
            "ttfb_ms",
            "time_to_first_token_ms",
        ),
    )
    if first_token is not None:
        result["first_token_latency_ms"] = first_token

    llm = _pick_number(
        flat,
        (
            "llm_inference_duration_ms",
            "llm_inference_ms",
            "model_inference_ms",
            "llm_latency_ms",
        ),
    )
    if llm is not None:
        result["llm_inference_duration_ms"] = llm

    retrieval = _pick_number(
        flat,
        (
            "retrieval_duration_ms",
            "retrieval_ms",
            "knowledge_retrieval_ms",
            "search_knowledge_duration_ms",
            "rag_retrieval_ms",
        ),
    )
    if retrieval is not None:
        result["retrieval_duration_ms"] = retrieval

    tool_total = _pick_number(
        flat,
        (
            "tool_call_duration_ms",
            "tool_duration_ms",
            "tools_duration_ms",
            "tool_calls_duration_ms",
        ),
    )
    if tool_total is not None:
        result["tool_call_duration_ms"] = tool_total

    overhead = _pick_number(
        flat,
        (
            "agent_or_graph_overhead_ms",
            "agent_overhead_ms",
            "graph_overhead_ms",
            "orchestration_overhead_ms",
        ),
    )
    if overhead is not None:
        result["agent_or_graph_overhead_ms"] = overhead

    if tool_breakdown:
        result["tool_call_breakdown"] = tool_breakdown
        if "tool_call_duration_ms" not in result:
            result["tool_call_duration_ms"] = int(sum(tool_breakdown.values()))

    return result


def ensure_duration_breakdown(
    *,
    request_total_duration_ms: int,
    first_token_latency_ms: int = 0,
    llm_inference_duration_ms: int = 0,
    retrieval_duration_ms: int = 0,
    tool_call_duration_ms: int = 0,
    agent_or_graph_overhead_ms: int = 0,
) -> dict[str, int]:
    """Normalize and fill duration breakdown values."""
    total = max(int(request_total_duration_ms or 0), 0)
    first = _normalize_int(first_token_latency_ms)
    llm = _normalize_int(llm_inference_duration_ms)
    retrieval = _normalize_int(retrieval_duration_ms)
    tool = _normalize_int(tool_call_duration_ms)
    overhead = _normalize_int(agent_or_graph_overhead_ms)

    consumed = llm + retrieval + tool + overhead
    if total > 0 and consumed > total:
        overflow = consumed - total
        overhead = max(overhead - overflow, 0)
        consumed = llm + retrieval + tool + overhead

    if total > 0 and consumed < total:
        overhead += total - consumed

    if first <= 0 and total > 0:
        first = min(total, llm if llm > 0 else total)

    return {
        "request_total_duration_ms": total,
        "first_token_latency_ms": first,
        "llm_inference_duration_ms": llm,
        "retrieval_duration_ms": retrieval,
        "tool_call_duration_ms": tool,
        "agent_or_graph_overhead_ms": overhead,
    }


def should_sample_trace(
    *,
    status: str,
    request_total_duration_ms: int,
    p95_threshold_ms: int,
    normal_sample_rate: float,
) -> tuple[bool, str]:
    """Tail-sampling decision for persisted traces."""
    normalized_status = (status or "").lower().strip()
    if normalized_status != "success":
        return True, "failure"

    total = max(int(request_total_duration_ms or 0), 0)
    p95 = max(int(p95_threshold_ms or 0), 0)
    if p95 > 0 and total >= p95:
        return True, "slow_request"

    rate = max(0.0, min(float(normal_sample_rate), 1.0))
    if rate <= 0:
        return False, "not_sampled"

    return random.random() < rate, "baseline"


def _contains_any(value: str, needles: Iterable[str]) -> bool:
    v = value or ""
    return any(n in v for n in needles)


def _normalize_int(value: Any) -> int:
    try:
        return max(int(float(value or 0)), 0)
    except (TypeError, ValueError):
        return 0


def _flatten(value: Any, output: dict[str, Any], prefix: str = "") -> None:
    if isinstance(value, dict):
        for k, v in value.items():
            key = str(k).strip().lower()
            if not key:
                continue
            path = f"{prefix}.{key}" if prefix else key
            output[path] = v
            _flatten(v, output, path)
    elif isinstance(value, list):
        for idx, item in enumerate(value):
            _flatten(item, output, f"{prefix}[{idx}]")


def _pick_number(flat: dict[str, Any], keys: Iterable[str]) -> int | None:
    normalized = {k.lower() for k in keys}
    for key, value in flat.items():
        base = key.split(".")[-1]
        base = re.sub(r"\[[0-9]+\]", "", base)
        if base in normalized:
            try:
                return max(int(float(value)), 0)
            except (TypeError, ValueError):
                continue
    return None


def _extract_tool_breakdown(payload: Any) -> dict[str, int]:
    result: dict[str, int] = {}

    def _walk(node: Any) -> None:
        if isinstance(node, dict):
            if "tool_name" in node or "name" in node:
                raw_name = node.get("tool_name") or node.get("name")
                raw_duration = (
                    node.get("duration_ms")
                    or node.get("tool_call_duration_ms")
                    or node.get("elapsed_ms")
                )
                if raw_name and raw_duration is not None:
                    try:
                        duration = max(int(float(raw_duration)), 0)
                    except (TypeError, ValueError):
                        duration = 0
                    if duration > 0:
                        name = str(raw_name)
                        result[name] = result.get(name, 0) + duration

            for child in node.values():
                _walk(child)
        elif isinstance(node, list):
            for item in node:
                _walk(item)

    _walk(payload)
    return result
