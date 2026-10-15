from __future__ import annotations

import re
from datetime import datetime
from typing import Any, Literal

ExportFormat = Literal["openinference", "otel", "langsmith-jsonl"]

EXPORT_REDACTION_POLICY: dict[str, str] = {
    "headers": "authorization, cookie, set-cookie, and api key headers are removed",
    "credentials": "bearer tokens, passwords, secrets, and URL userinfo are redacted",
    "payloads": "export payloads use already-bounded trace previews plus defensive redaction",
}

_SENSITIVE_EXACT_KEYS = {
    "authorization",
    "cookie",
    "set_cookie",
    "api_key",
    "apikey",
    "x_api_key",
    "secret",
    "password",
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "auth_token",
    "bearer_token",
}
_EXPORT_REDACTION_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (
        re.compile(r"(?i)(authorization\s*[:=]\s*bearer\s+)[^\s,;]+"),
        r"\1[redacted]",
    ),
    (re.compile(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+"), r"\1[redacted]"),
    (
        re.compile(
            r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|"
            r"token|password|secret|cookie)\b\s*[:=]\s*[^,\s;&]+"
        ),
        r"\1=[redacted]",
    ),
    (re.compile(r"(?i)(://[^:\s/@]+:)[^@\s/]+(@)"), r"\1[redacted]\2"),
)


def _iso(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, datetime):
        return value.isoformat()
    return str(value)


def _duration_ns(ms: int | None) -> int:
    return int(ms or 0) * 1_000_000


def _redact_export_text(value: Any) -> str:
    text = "" if value is None else str(value)
    for pattern, replacement in _EXPORT_REDACTION_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


def _is_sensitive_export_key(key: Any) -> bool:
    normalized = str(key).lower().replace("-", "_").replace(".", "_")
    if normalized in _SENSITIVE_EXACT_KEYS:
        return True
    return normalized.endswith("_token") and not normalized.endswith("_tokens")


def _safe_export_value(value: Any) -> Any:
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, dict):
        return {
            str(key): "[redacted]" if _is_sensitive_export_key(key) else _safe_export_value(item)
            for key, item in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [_safe_export_value(item) for item in value]
    return _redact_export_text(value)


def _gen_ai_attributes(trace: dict[str, Any]) -> dict[str, Any]:
    return _safe_export_value({
        "gen_ai.operation.name": trace.get("workflow_kind") or "chat",
        "gen_ai.system": trace.get("provider") or "",
        "gen_ai.request.model": trace.get("model_id") or "",
        "gen_ai.response.model": trace.get("model_id") or "",
        "gen_ai.usage.input_tokens": int(trace.get("input_tokens") or 0),
        "gen_ai.usage.output_tokens": int(trace.get("output_tokens") or 0),
        "gen_ai.usage.total_tokens": int(trace.get("total_tokens") or 0),
        "session.id": trace.get("thread_id") or trace.get("session_id") or "",
        "user.id": trace.get("user_id") or "",
        "ai_gateway.trace_family": trace.get("trace_family") or "",
        "ai_gateway.request_id": trace.get("request_id") or "",
        "ai_gateway.run_id": trace.get("run_id") or "",
    })


def export_trace(detail: dict[str, Any], export_format: ExportFormat) -> dict[str, Any] | list[dict[str, Any]]:
    if export_format == "otel":
        return to_otel(detail)
    if export_format == "openinference":
        return to_openinference(detail)
    return to_langsmith_jsonl(detail)


def to_otel(detail: dict[str, Any]) -> dict[str, Any]:
    trace = detail["trace"]
    spans = detail.get("spans") or []
    events = detail.get("events") or []
    root_span_id = str(trace.get("trace_id") or "")
    return {
        "resource": {
            "service.name": "ai-gateway",
            "ai_gateway.schema_version": "ate-03",
        },
        "trace": {
            "trace_id": str(trace.get("trace_id") or ""),
            "span_id": root_span_id,
            "parent_span_id": None,
            "name": trace.get("workflow_kind") or "ai_assistant_chat",
            "kind": "INTERNAL",
            "status": trace.get("status"),
            "start_time": _iso(trace.get("started_at")),
            "end_time": _iso(trace.get("ended_at")),
            "duration_ns": _duration_ns(trace.get("total_latency_ms")),
            "attributes": {
                **_gen_ai_attributes(trace),
                "ai_gateway.input_preview": _redact_export_text(trace.get("input_preview") or ""),
                "ai_gateway.output_preview": _redact_export_text(trace.get("output_preview") or ""),
            },
            "events": [
                {
                    "name": event.get("event_type"),
                    "time": _iso(event.get("occurred_at")),
                    "attributes": _safe_export_value(event.get("payload") or {}),
                }
                for event in events
            ],
        },
        "spans": [
            {
                "trace_id": str(span.get("trace_id") or trace.get("trace_id") or ""),
                "span_id": str(span.get("span_id") or ""),
                "parent_span_id": span.get("parent_span_id") or root_span_id,
                "name": span.get("name") or span.get("span_kind"),
                "kind": "INTERNAL",
                "status": span.get("status"),
                "start_time": _iso(span.get("started_at")),
                "end_time": _iso(span.get("ended_at")),
                "duration_ns": _duration_ns(span.get("duration_ms")),
                "attributes": _safe_export_value(span.get("attributes") or {}),
            }
            for span in spans
        ],
    }


def to_openinference(detail: dict[str, Any]) -> dict[str, Any]:
    trace = detail["trace"]
    return {
        "trace_id": str(trace.get("trace_id") or ""),
        "project_name": "ai-gateway",
        "schema_url": "https://arize-ai.github.io/openinference/spec/semantic_conventions.html",
        "root": {
            "openinference.span.kind": "AGENT",
            "input.value": _redact_export_text(trace.get("input_preview") or ""),
            "output.value": _redact_export_text(trace.get("output_preview") or ""),
            "metadata": _safe_export_value(trace.get("metadata") or {}),
            "attributes": _gen_ai_attributes(trace),
        },
        "spans": [
            {
                "span_id": str(span.get("span_id") or ""),
                "parent_span_id": span.get("parent_span_id"),
                "name": span.get("name") or span.get("span_kind"),
                "openinference.span.kind": _openinference_kind(span.get("span_kind")),
                "input.value": _redact_export_text(span.get("input_preview") or ""),
                "output.value": _redact_export_text(span.get("output_preview") or ""),
                "attributes": _safe_export_value(span.get("attributes") or {}),
                "status": span.get("status"),
            }
            for span in detail.get("spans") or []
        ],
    }


def to_langsmith_jsonl(detail: dict[str, Any]) -> list[dict[str, Any]]:
    trace = detail["trace"]
    root = {
        "id": str(trace.get("trace_id") or ""),
        "name": trace.get("workflow_kind") or "ai_assistant_chat",
        "run_type": "chain",
        "session_name": trace.get("thread_id") or trace.get("session_id"),
        "inputs": {"preview": _redact_export_text(trace.get("input_preview") or "")},
        "outputs": {"preview": _redact_export_text(trace.get("output_preview") or "")},
        "start_time": _iso(trace.get("started_at")),
        "end_time": _iso(trace.get("ended_at")),
        "extra": {
            "metadata": _safe_export_value(trace.get("metadata") or {}),
            "tags": [trace.get("trace_family") or "assistant"],
            "ai_gateway": _gen_ai_attributes(trace),
        },
    }
    children = [
        {
            "id": str(span.get("span_id") or ""),
            "parent_run_id": span.get("parent_span_id") or root["id"],
            "name": span.get("name") or span.get("span_kind"),
            "run_type": _langsmith_run_type(span.get("span_kind")),
            "inputs": {"preview": _redact_export_text(span.get("input_preview") or "")},
            "outputs": {"preview": _redact_export_text(span.get("output_preview") or "")},
            "start_time": _iso(span.get("started_at")),
            "end_time": _iso(span.get("ended_at")),
            "extra": {"metadata": _safe_export_value(span.get("attributes") or {})},
        }
        for span in detail.get("spans") or []
    ]
    return [root, *children]


def _openinference_kind(span_kind: Any) -> str:
    mapping = {
        "model_invocation": "LLM",
        "tool_execution": "TOOL",
        "retriever": "RETRIEVER",
        "embedding": "EMBEDDING",
        "reranker": "RERANKER",
        "context_building": "CHAIN",
        "lifecycle": "AGENT",
    }
    return mapping.get(str(span_kind or ""), "CHAIN")


def _langsmith_run_type(span_kind: Any) -> str:
    mapping = {
        "model_invocation": "llm",
        "tool_execution": "tool",
        "retriever": "retriever",
        "embedding": "embedding",
        "reranker": "chain",
        "context_building": "chain",
    }
    return mapping.get(str(span_kind or ""), "chain")
