"""Fail-closed adapter for Agent knowledge authorization.

The Dataset ACL is owned by knowledge-service. Both authoring and runtime pass
their candidate bindings to the same HMAC-authenticated resolver; this module
contains no knowledge-table SQL.
"""

from __future__ import annotations

from typing import Any, Protocol


class AgentKnowledgeResolver(Protocol):
    """Knowledge-service authorization client used by Agent workflows."""

    async def resolve(
        self,
        *,
        tenant_id: str,
        user_id: str,
        bindings: list[dict[str, Any]],
        is_tenant_admin: bool = False,
        roles: list[str] | None = None,
        **kwargs: Any,
    ) -> list[dict[str, Any]]: ...


async def authorized_dataset_ids(
    resolver: AgentKnowledgeResolver | None,
    *,
    tenant_id: str,
    user_id: str,
    dataset_ids: list[str],
    is_tenant_admin: bool,
    roles: list[str] | None = None,
) -> set[str]:
    """Resolve candidate IDs through knowledge-service, denying uncertainty."""

    if not dataset_ids:
        return set()
    if resolver is None:
        return set()

    bindings = [{"dataset_id": str(dataset_id)} for dataset_id in dataset_ids]
    try:
        resolved = await resolver.resolve(
            tenant_id=tenant_id,
            user_id=user_id,
            bindings=bindings,
            is_tenant_admin=is_tenant_admin,
            roles=roles or [],
            channel="authoring",
            authenticated=True,
        )
    except Exception:  # noqa: BLE001 - authorization uncertainty must deny
        return set()
    if not isinstance(resolved, list):
        return set()

    allowed: set[str] = set()
    for binding in resolved:
        if not isinstance(binding, dict):
            return set()
        dataset_id = binding.get("dataset_id")
        if not isinstance(dataset_id, str) or not dataset_id:
            return set()
        allowed.add(dataset_id)
    return allowed


__all__ = ["AgentKnowledgeResolver", "authorized_dataset_ids"]
