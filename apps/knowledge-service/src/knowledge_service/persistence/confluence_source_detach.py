"""Source-reference removal inside the existing exclusive dataset lifecycle lease.

The caller keeps that lease across reference classification and the normal
vector/assets/document deletion pipeline. No pool acquisition or network occurs
here. Reference-creating repository entrances must hold the matching shared lease.
"""

from __future__ import annotations

import uuid
from typing import Any

from ..core.exceptions import ValidationFailedError


def _owner(row):
    return str(row.get("owner_id") or row.get("created_by") or "")


def _fail():
    raise ValidationFailedError("Confluence source ownership changed; removal refused")


async def prepare_source_removal(
    *,
    connection: Any,
    dataset_id: str,
    tenant_id: str,
    document: dict | None,
    binding: dict,
    page: dict,
) -> str:
    """Detach a shared/exact empty reference, or authorize last-reference delete.

    Returns detached, absent, or last_reference. No document is deleted here.
    Last-reference classification is valid only while the caller holds its lease.
    """
    binding_id = str(binding.get("binding_id") or "")
    record_id = str(page.get("id") or "")
    page_id = str(page.get("page_id") or "")
    if not all((binding_id, record_id, page_id, tenant_id, dataset_id)):
        _fail()
    if page.get("binding_id") != binding_id:
        _fail()
    current = await connection.fetchrow(
        "SELECT * FROM confluence_space_bindings WHERE binding_id = $1",
        binding_id,
    )
    if not current:
        _fail()
    current = dict(current)
    for key in ("binding_id", "dataset_id", "tenant_id", "connection_id"):
        if not binding.get(key) or current.get(key) != binding.get(key):
            _fail()
    if (
        current["dataset_id"] != dataset_id
        or current["tenant_id"] != tenant_id
        or not _owner(binding)
        or _owner(current) != _owner(binding)
    ):
        _fail()
    row = await connection.fetchrow("SELECT * FROM confluence_pages WHERE id = $1", record_id)
    if row is None:
        return "absent"
    row = dict(row)
    for key in ("id", "binding_id", "page_id", "document_id"):
        if row.get(key) != page.get(key):
            _fail()
    for key in ("version", "content_hash", "updated_at"):
        if key in page and row.get(key) != page[key]:
            _fail()
    document_id = page.get("document_id")
    if not document_id:
        if document is not None:
            _fail()
        await delete_source_record(connection=connection, page=page)
        return "detached"
    if (
        not document
        or document.get("document_id") != document_id
        or document.get("dataset_id") != dataset_id
        or document.get("source_type") != "confluence"
    ):
        _fail()
    # Count every reference, including inconsistent/foreign bindings. We must
    # never delete a document just because another reference failed a join/filter.
    shared = await connection.fetchval(
        "SELECT EXISTS (SELECT 1 FROM confluence_pages WHERE document_id = $1 AND id <> $2)",
        document_id,
        record_id,
    )
    if shared:
        await delete_source_record(connection=connection, page=page)
        return "detached"
    canonical_id = str(
        uuid.uuid5(
            uuid.NAMESPACE_URL,
            f"ai-platform:confluence:{dataset_id}:{current['connection_id']}:{page_id}",
        )
    )
    legacy_identity = (
        document.get("confluence_binding_id") == binding_id
        and str(document.get("confluence_page_id") or "") == page_id
    )
    if document_id != canonical_id and not legacy_identity:
        _fail()
    metadata = document.get("metadata") or {}
    if not isinstance(metadata, dict):
        _fail()
    if metadata.get("confluence_page_id") not in (None, page_id):
        _fail()
    return "last_reference"


async def delete_source_record(*, connection: Any, page: dict) -> None:
    """Delete exactly the row classified while the same lifecycle lease is held."""
    result = await connection.execute(
        """DELETE FROM confluence_pages
           WHERE id = $1 AND binding_id = $2 AND page_id = $3
             AND document_id IS NOT DISTINCT FROM $4""",
        page["id"],
        page["binding_id"],
        page["page_id"],
        page.get("document_id"),
    )
    if result != "DELETE 1":
        _fail()
