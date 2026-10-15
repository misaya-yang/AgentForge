from __future__ import annotations

import json
from typing import Any

from fastapi import Request
from fastapi.encoders import jsonable_encoder

from ...api.deps import AuthContext
from ...api.deps import _get_client_ip as get_client_ip
from .redaction import redact_sensitive_data


def _request_id(request: Request) -> str:
    return str(getattr(request.state, "request_id", "") or "")


def _trace_id(request: Request) -> str:
    return str(getattr(request.state, "trace_id", "") or _request_id(request))


def _audit_payload(
    *,
    request: Request,
    before: dict[str, Any] | None,
    after: dict[str, Any] | None,
) -> dict[str, Any]:
    # Pydantic admin payloads can contain Decimal/datetime/UUID values.  The
    # direct pool fallback serializes this object with json.dumps, so normalize
    # only after redaction; otherwise a successful config write can surface as
    # HTTP 500 while the database mutation has already committed.
    return jsonable_encoder(
        {
            "trace_id": _trace_id(request),
            "request_id": _request_id(request),
            "before": redact_sensitive_data(before or {}),
            "after": redact_sensitive_data(after or {}),
        }
    )


async def record_config_change(
    *,
    request: Request,
    auth: AuthContext,
    resource_type: str,
    resource_id: str,
    action: str,
    before: dict[str, Any] | None,
    after: dict[str, Any] | None,
) -> None:
    """Persist a redacted admin configuration audit event."""
    database = getattr(request.app.state, "database", None)
    if database is None:
        return

    event = {
        "event_type": "config_changed",
        "user_id": auth.user_id or None,
        "tenant_id": auth.tenant_id or "public",
        "ip_address": get_client_ip(request),
        "user_agent": request.headers.get("user-agent", ""),
        "resource_type": resource_type,
        "resource_id": resource_id,
        "action": action,
        "request_summary": _audit_payload(request=request, before=before, after=after),
        "response_summary": {
            "status": "success",
            "has_api_key": bool(
                isinstance(after, dict)
                and any(str(k).lower() in {"api_key", "_api_key"} for k in after)
            ),
        },
        "status": "success",
        "error_message": None,
        "duration_ms": None,
    }

    if hasattr(database, "record_audit_event"):
        await database.record_audit_event(**event)
        return

    pool = getattr(database, "_pool", None)
    if not pool:
        return

    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, ip_address, user_agent,
                resource_type, resource_id, action,
                request_summary, response_summary, status, error_message, duration_ms
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8,
                $9::jsonb, $10::jsonb, $11, $12, $13
            )
            """,
            event["event_type"],
            event["user_id"],
            event["tenant_id"],
            event["ip_address"],
            event["user_agent"],
            event["resource_type"],
            event["resource_id"],
            event["action"],
            json.dumps(event["request_summary"], ensure_ascii=False),
            json.dumps(event["response_summary"], ensure_ascii=False),
            event["status"],
            event["error_message"],
            event["duration_ms"],
        )
