"""Authoritative published citation identity shared by Knowledge HTTP adapters."""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .document_service import _require_unchanged_dataset_content

if TYPE_CHECKING:
    from ...core.auth.user_resolver import UserContext
    from .knowledge_service import KnowledgeService


async def _published_source_identity_map(
    svc: KnowledgeService,
    user: UserContext,
    dataset_id: str,
    document_ids: list[str],
    expected_generation: Any,
) -> dict[str, tuple[int, str]]:
    if not document_ids:
        return {}
    lookup = getattr(getattr(svc, "db", None), "published_source_identities", None)
    if not callable(lookup):
        return {}
    if expected_generation is None:
        return {}
    await _require_unchanged_dataset_content(
        svc, user, dataset_id, expected_generation,
    )
    try:
        identities = await lookup(dataset_id, user.tenant_id, document_ids)
    except Exception:
        # Database drivers use several ordinary exception classes. Normalize
        # only this IO boundary; cancellation and outer ACL/generation checks
        # retain their original semantics and no driver detail is exposed.
        raise RuntimeError("published source identity authority unavailable") from None
    if not isinstance(identities, dict) or not set(identities) <= set(document_ids):
        raise RuntimeError("published source identity lookup is invalid")
    await _require_unchanged_dataset_content(
        svc, user, dataset_id, expected_generation,
    )
    if any(
        not isinstance(value, (tuple, list)) or len(value) != 2
        or type(value[0]) is not int or value[0] <= 0
        or not isinstance(value[1], str) or len(value[1]) != 64
        or any(char not in "0123456789abcdef" for char in value[1])
        for value in identities.values()
    ):
        raise RuntimeError("published source identity lookup is invalid")
    return identities


def _source_identity(
    metadata: dict[str, Any] | None,
    document_id: str,
    identities: dict[str, tuple[int, str]],
) -> tuple[int, str] | None:
    payload = metadata if isinstance(metadata, dict) else {}
    nested = payload.get("metadata")
    nested = nested if isinstance(nested, dict) else {}
    if (
        payload.get("source_type") == "manual"
        or payload.get("manual_source_override") is True
        or nested.get("manual_source_override") is True
    ):
        return None
    return identities.get(document_id)


def _source_metadata(
    metadata: dict[str, Any] | None,
    document_id: str,
    identities: dict[str, tuple[int, str]],
) -> dict[str, Any]:
    result = dict(metadata) if isinstance(metadata, dict) else {}
    result.pop("source_version", None)
    result.pop("source_hash", None)
    identity = _source_identity(result, document_id, identities)
    if identity:
        result["source_version"], result["source_hash"] = identity
    return result


