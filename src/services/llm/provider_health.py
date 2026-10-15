"""Bounded receipts from explicit provider connection tests, scoped to one process."""

from __future__ import annotations

from collections import OrderedDict
from datetime import datetime, timezone
from typing import Any
from uuid import uuid4

PROBE_TTL_SECONDS = 300
MAX_PROBE_RECEIPTS = 256


def begin_provider_probe(request: Any, tenant_id: str, provider_id: str) -> str | None:
    """Bind the result to this test; config changes and newer tests invalidate it."""
    if request is None:
        return None
    state = request.app.state
    receipts = getattr(state, "provider_probe_receipts", None)
    if receipts is None:
        receipts = OrderedDict()
        state.provider_probe_receipts = receipts
    key = (tenant_id, provider_id)
    probe_id = str(uuid4())
    receipts[key] = {"probe_id": probe_id}
    receipts.move_to_end(key)
    while len(receipts) > MAX_PROBE_RECEIPTS:
        receipts.popitem(last=False)
    return probe_id


def record_provider_probe(
    request: Any,
    tenant_id: str,
    provider_id: str,
    result: dict[str, Any],
    *,
    probe_id: str | None,
    now: datetime | None = None,
) -> None:
    if request is None or probe_id is None:
        return
    receipt = getattr(request.app.state, "provider_probe_receipts", {}).get(
        (tenant_id, provider_id)
    )
    if not isinstance(receipt, dict) or receipt.get("probe_id") != probe_id:
        return
    receipt.update(
        checked_at=(now or datetime.now(timezone.utc)).isoformat(),
        success=result.get("success") is True,
    )


def invalidate_provider_probe(request: Any, tenant_id: str, provider_id: str) -> None:
    if request is not None:
        receipts = getattr(request.app.state, "provider_probe_receipts", {})
        receipts.pop((tenant_id, provider_id), None)


def provider_probe_status(
    request: Any,
    tenant_id: str,
    provider_id: str,
    *,
    configured: bool,
    now: datetime | None = None,
) -> dict[str, Any]:
    if not configured:
        return {"status": "not_configured", "last_check": None, "probe_source": None}
    receipt = getattr(request.app.state, "provider_probe_receipts", {}).get(
        (tenant_id, provider_id)
    )
    if not isinstance(receipt, dict) or "checked_at" not in receipt:
        return {"status": "unverified", "last_check": None, "probe_source": None}
    current = now or datetime.now(timezone.utc)
    checked = datetime.fromisoformat(receipt["checked_at"])
    age = (current - checked).total_seconds()
    status = (
        "stale"
        if not 0 <= age <= PROBE_TTL_SECONDS
        else "healthy"
        if receipt["success"]
        else "unhealthy"
    )
    return {
        "status": status,
        "last_check": receipt["checked_at"],
        "probe_source": "connection_test",
    }
