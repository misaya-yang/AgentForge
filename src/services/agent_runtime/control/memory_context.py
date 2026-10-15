"""Bounded cross-session memory loading for Runtime turns.

ARC-02 split of ``control_plane.py``.
"""

from __future__ import annotations

from typing import Any

from ai_gateway_contracts.agent_runtime import canonical_runtime_json

from .types import AgentRuntimeControlError


async def load_memory_context(
    memory_service: Any,
    *,
    tenant_id: str,
    user_id: str,
    mode: str,
) -> dict[str, Any] | None:
    """Load bounded cross-session memory only for an explicit memory mode."""

    if mode not in {"auto", "strict", "user"}:
        return None
    service = memory_service
    if service is None or not hasattr(service, "get_long_term_context"):
        if mode in {"strict", "user"}:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_UNAVAILABLE", status_code=503
            )
        return {"status": "unavailable", "reason": "memory_service_unavailable"}
    try:
        context = await service.get_long_term_context(
            tenant_id=tenant_id,
            user_id=user_id,
            limit=20,
        )
    except Exception as exc:  # noqa: BLE001 - memory policy decides fallback
        if mode in {"strict", "user"}:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_UNAVAILABLE", status_code=503
            ) from exc
        return {"status": "unavailable", "reason": "memory_lookup_failed"}
    if not isinstance(context, dict):
        if mode in {"strict", "user"}:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_INVALID", status_code=503
            )
        return {"status": "unavailable", "reason": "memory_payload_invalid"}
    preferences = context.get("preferences")
    frequent = context.get("frequent_memories")
    if not isinstance(preferences, dict):
        preferences = {}
    if not isinstance(frequent, list):
        frequent = []
    bounded_frequent: list[dict[str, Any]] = []
    for item in frequent[:10]:
        if not isinstance(item, dict):
            continue
        bounded_frequent.append(
            {
                "key": str(item.get("key") or "")[:128],
                "value": item.get("value"),
                "access_count": int(item.get("access_count") or 0),
            }
        )
    bounded = {"preferences": preferences, "frequent_memories": bounded_frequent}
    encoded = canonical_runtime_json(bounded)
    if len(encoded) > 64 * 1024:
        bounded["frequent_memories"] = []
        encoded = canonical_runtime_json(bounded)
        if len(encoded) > 64 * 1024:
            bounded["preferences"] = {}
    return {"status": "available", "context": bounded}


__all__ = ["load_memory_context"]
