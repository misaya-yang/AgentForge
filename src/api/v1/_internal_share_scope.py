"""Private source identities and current access checks for internal share links."""

from __future__ import annotations

import json
from typing import Any

from fastapi import HTTPException, Request

from ...core.auth.user_resolver import UserContext
from ...services.assistant_entry.source_access import (
    SourceVersionRef,
    source_scope_allowed,
)
from ..deps import get_user_context


def freeze_source_scope(
    dataset_ids: frozenset[str],
    document_ids: frozenset[tuple[str, str]],
    source_versions: frozenset[SourceVersionRef],
) -> dict[str, Any]:
    """Build a private JSON value; every referenced document needs an immutable version."""
    if (
        any(not isinstance(dataset_id, str) or not dataset_id for dataset_id in dataset_ids)
        or any(
            not isinstance(dataset_id, str) or not dataset_id
            or not isinstance(document_id, str) or not document_id
            for dataset_id, document_id in document_ids
        )
        or any(
            not isinstance(dataset_id, str) or not dataset_id
            or not isinstance(document_id, str) or not document_id
            or type(version) is not int or version <= 0
            or not isinstance(source_hash, str)
            or len(source_hash) != 64
            or any(character not in "0123456789abcdef" for character in source_hash)
            for dataset_id, document_id, version, source_hash in source_versions
        )
        or document_ids != frozenset((dataset_id, document_id) for dataset_id, document_id, _, _ in source_versions)
        or not {dataset_id for dataset_id, _ in document_ids} <= dataset_ids
        or (dataset_ids and {dataset_id for dataset_id, _, _, _ in source_versions} != set(dataset_ids))
    ):
        raise HTTPException(409, "Share sources lack a verifiable immutable version")
    return {
        "version": 1,
        "dataset_ids": sorted(dataset_ids),
        "document_ids": [list(item) for item in sorted(document_ids)],
        "source_versions": [list(item) for item in sorted(source_versions)],
    }


def _parse_source_scope(value: Any) -> tuple[frozenset[str], frozenset[tuple[str, str]], frozenset[SourceVersionRef]]:
    try:
        scope = json.loads(value) if isinstance(value, str) else value
        if not isinstance(scope, dict) or scope.get("version") != 1:
            raise ValueError("invalid source scope")
        datasets = scope["dataset_ids"]
        documents = scope["document_ids"]
        versions = scope["source_versions"]
        if not isinstance(datasets, list) or not isinstance(documents, list) or not isinstance(versions, list):
            raise ValueError("invalid source scope")
        if any(not isinstance(item, str) for item in datasets):
            raise ValueError("invalid dataset")
        if any(not isinstance(item, list) or len(item) != 2 or any(not isinstance(part, str) for part in item) for item in documents):
            raise ValueError("invalid document")
        if any(
            not isinstance(item, list) or len(item) != 4
            or not isinstance(item[0], str) or not isinstance(item[1], str)
            or type(item[2]) is not int or not isinstance(item[3], str)
            for item in versions
        ):
            raise ValueError("invalid version")
        dataset_ids = frozenset(datasets)
        document_ids = frozenset((item[0], item[1]) for item in documents)
        source_versions = frozenset((item[0], item[1], item[2], item[3]) for item in versions)
        if len(dataset_ids) != len(datasets) or len(document_ids) != len(documents) or len(source_versions) != len(versions):
            raise ValueError("duplicate source")
        freeze_source_scope(dataset_ids, document_ids, source_versions)
        return dataset_ids, document_ids, source_versions
    except (KeyError, TypeError, ValueError, HTTPException) as exc:
        raise HTTPException(410, "Share source identities cannot be verified") from exc


async def require_active_internal_user(request: Request, user: UserContext, tenant_id: str) -> None:
    """A valid JWT alone is insufficient after an account is disabled or moved."""
    if not user.is_authenticated or not user.user_id or not user.tenant_id:
        raise HTTPException(401, "Sign in to open this internal share")
    if user.tenant_id != tenant_id:
        raise HTTPException(404, "Share not found")
    db = getattr(request.app.state, "database", None)
    if db is None or not callable(getattr(db, "get_user", None)):
        raise HTTPException(503, "Account verification is unavailable")
    try:
        account = await db.get_user(user.user_id)
    except Exception as exc:
        raise HTTPException(503, "Account verification is unavailable") from exc
    if not account or account.get("status") != "active" or str(account.get("tenant_id") or "") != user.tenant_id:
        raise HTTPException(403, "Account is not active in this tenant")


async def require_internal_share_access(request: Request, row: Any) -> UserContext:
    """Resolve a live account and recheck the frozen sources on every read."""
    settings = getattr(request.app.state, "settings", None)
    if settings is None:
        raise HTTPException(503, "Authentication is unavailable")
    user = await get_user_context(request, settings=settings)
    await require_active_internal_user(request, user, row["tenant_id"])
    datasets, documents, versions = _parse_source_scope(row.get("source_scope"))
    if not await source_scope_allowed(request, user, datasets, documents, versioned_refs=versions):
        raise HTTPException(403, "Share source access has been revoked")
    return user
