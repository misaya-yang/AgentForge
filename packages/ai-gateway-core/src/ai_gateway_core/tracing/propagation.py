"""Safe outbound correlation headers for internal HTTP hops."""

from __future__ import annotations

import uuid
from collections.abc import Mapping

_CORRELATION_HEADERS = {
    "run_id": "x-ai-run-id",
    "turn_id": "x-ai-turn-id",
    "execution_id": "x-ai-execution-id",
}
_MAX_CORRELATION_ID_LENGTH = 128
_SAFE_CORRELATION_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.:"
)


def _safe_correlation_id(value: object) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        return ""
    if len(normalized) > _MAX_CORRELATION_ID_LENGTH or any(
        character not in _SAFE_CORRELATION_CHARS for character in normalized
    ):
        raise ValueError("internal correlation id is malformed")
    return normalized


def _safe_request_id(value: object) -> str:
    normalized = str(value or "").strip()
    if not normalized or len(normalized) > 64:
        return ""
    if any(character not in _SAFE_CORRELATION_CHARS - {":"} for character in normalized):
        return ""
    return normalized


def internal_http_headers(
    existing: Mapping[str, str] | None = None,
    *,
    run_id: object = None,
    turn_id: object = None,
    execution_id: object = None,
) -> dict[str, str]:
    """Add only trace/correlation metadata; never copy request payload or secrets."""

    headers = dict(existing or {})
    lowered = {key.lower() for key in headers}
    try:
        from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX

        request_id = REQUEST_ID_CTX.get()
    except Exception:  # noqa: BLE001 - optional middleware context
        request_id = ""
    request_id = _safe_request_id(request_id) or f"svc-{uuid.uuid4()}"
    if "x-request-id" not in lowered:
        headers["x-request-id"] = request_id
        lowered.add("x-request-id")

    carrier: dict[str, str] = {}
    try:
        from opentelemetry.propagate import inject

        inject(carrier)
    except Exception:  # noqa: BLE001 - tracing is optional in stripped images
        carrier = {}
    for name in ("traceparent", "tracestate"):
        value = carrier.get(name)
        if value and name not in lowered:
            headers[name] = value
            lowered.add(name)

    values = {
        "run_id": run_id,
        "turn_id": turn_id,
        "execution_id": execution_id,
    }
    for field, header_name in _CORRELATION_HEADERS.items():
        value = _safe_correlation_id(values[field])
        if value and header_name not in lowered:
            headers[header_name] = value
            lowered.add(header_name)
    return headers


__all__ = ["internal_http_headers"]
