"""Unit tests for the PRD T1.1 display_status derivation.

The API must never leak internal lifecycle states: every document payload
carries a derived ``display_status`` from the fixed Dify-parity vocabulary
(queuing/indexing/paused/error/available/disabled/archived). Unknown or
in-flight internal states fail closed into 'indexing'.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.document_service import (
    DOCUMENT_DISPLAY_STATUS_VOCABULARY,
    DocumentService,
    _with_display_status,
    derive_document_display_status,
)


@pytest.mark.parametrize(
    ("internal_status", "expected"),
    [
        # Canonical machine vocabulary.
        ("waiting", "queuing"),
        ("parsing", "indexing"),
        ("splitting", "indexing"),
        ("indexing", "indexing"),
        ("completed", "available"),
        ("error", "error"),
        ("paused", "paused"),
        # Legacy Confluence sync state must not surface verbatim.
        ("syncing", "indexing"),
        # Upload-phase states belong to the catch-all indexing bucket.
        ("uploading", "indexing"),
        ("uploading_images", "indexing"),
        # Pre-migration legacy values that may still be observed.
        ("failed", "error"),
    ],
)
def test_internal_states_map_to_display_vocabulary(
    internal_status: str, expected: str
) -> None:
    document = {"status": internal_status, "enabled": True, "archived": False}
    assert derive_document_display_status(document) == expected


def test_archived_wins_over_every_other_state() -> None:
    for status in ("error", "completed", "waiting", "indexing", "failed", ""):
        assert (
            derive_document_display_status(
                {"status": status, "enabled": True, "archived": True}
            )
            == "archived"
        )


def test_completed_splits_on_enabled_flag() -> None:
    assert (
        derive_document_display_status(
            {"status": "completed", "enabled": True, "archived": False}
        )
        == "available"
    )
    assert (
        derive_document_display_status(
            {"status": "completed", "enabled": False, "archived": False}
        )
        == "disabled"
    )
    # A missing enabled flag defaults to serving.
    assert (
        derive_document_display_status({"status": "completed", "archived": False})
        == "available"
    )


@pytest.mark.parametrize(
    "bad_status",
    ["", None, "uploaded", "queued", "processing", "detecting", "segmenting",
     "embedding", "embedding_images", "associating_images", "something-new"],
)
def test_unknown_and_legacy_states_fail_closed_to_indexing(bad_status: object) -> None:
    document = {"status": bad_status, "enabled": True, "archived": False}
    assert derive_document_display_status(document) == "indexing"


def test_non_dict_input_fails_closed() -> None:
    assert derive_document_display_status(None) == "indexing"
    assert derive_document_display_status("completed") == "indexing"
    assert derive_document_display_status([]) == "indexing"


def test_status_text_is_normalized_before_matching() -> None:
    assert (
        derive_document_display_status(
            {"status": "  COMPLETED ", "enabled": True, "archived": False}
        )
        == "available"
    )
    assert derive_document_display_status({"status": " Waiting "}) == "queuing"


def test_every_derivation_stays_inside_the_display_vocabulary() -> None:
    statuses = [
        "waiting", "parsing", "splitting", "indexing", "completed", "error",
        "paused", "syncing", "uploading", "uploading_images", "failed", "",
        None, "mystery",
    ]
    for status in statuses:
        for archived in (True, False):
            for enabled in (True, False):
                derived = derive_document_display_status(
                    {"status": status, "enabled": enabled, "archived": archived}
                )
                assert derived in DOCUMENT_DISPLAY_STATUS_VOCABULARY


def test_with_display_status_stamps_the_payload_in_place() -> None:
    document = {"document_id": "doc-a", "status": "waiting", "enabled": True}
    stamped = _with_display_status(document)
    assert stamped is document
    assert stamped["display_status"] == "queuing"
    # Internal status stays present for compatibility until T5 rewires the
    # frontend; nothing else in the payload is disturbed.
    assert stamped["status"] == "waiting"
    assert stamped["document_id"] == "doc-a"


def test_with_display_status_passes_non_dicts_through() -> None:
    assert _with_display_status(None) is None  # type: ignore[arg-type]
    assert _with_display_status("x") == "x"  # type: ignore[arg-type]


@pytest.mark.parametrize("archived", [False, True])
def test_failed_document_projection_hides_internal_error(archived: bool) -> None:
    document = {
        "document_id": "doc-safe-diagnostic",
        "status": "error",
        "archived": archived,
        "error": "provider credential private-secret and /internal/storage/path",
    }
    stamped = _with_display_status(document)
    assert stamped["display_status"] == ("archived" if archived else "error")
    assert stamped["document_id"] == "doc-safe-diagnostic"
    assert "private-secret" not in stamped["error"]
    assert "/internal/storage/path" not in stamped["error"]
    assert "retry manually" in stamped["error"]


def test_specialized_rebuild_refusal_has_safe_actionable_code() -> None:
    stamped = _with_display_status({
        "document_id": "doc-a",
        "status": "error",
        "error": "specialized_rebuild_unavailable",
    })
    assert stamped["failure_code"] == "specialized_rebuild_unavailable"
    assert "may have partial results" in stamped["error"]


@pytest.mark.asyncio
async def test_document_body_viewer_requires_active_row_editor_can_manage_disabled() -> None:
    document = {
        "document_id": "doc-a", "dataset_id": "dataset-a",
        "content": "private document body", "status": "completed",
        "enabled": False, "archived": False,
    }
    database = SimpleNamespace(
        get_document=AsyncMock(return_value=document),
        filter_active_document_ids=AsyncMock(return_value=set()),
    )
    dataset = {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "content_revision": 1}
    knowledge = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=dataset),
        _effective_dataset_permission=AsyncMock(return_value="viewer"),
    )
    service = DocumentService(settings=None, database=database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]
    user = UserContext(user_id="user-a", tenant_id="tenant-a")

    with pytest.raises(ValidationFailedError, match="document not found"):
        await service.get_document(user, "dataset-a", "doc-a")
    database.filter_active_document_ids.return_value = {"doc-a"}
    assert (await service.get_document(user, "dataset-a", "doc-a"))["content"] == "private document body"
    knowledge._effective_dataset_permission.return_value = "editor"
    database.filter_active_document_ids.return_value = set()
    assert (await service.get_document(user, "dataset-a", "doc-a"))["content"] == "private document body"


@pytest.mark.asyncio
async def test_viewer_pages_request_active_rows_with_matching_totals() -> None:
    database = SimpleNamespace(
        list_documents=AsyncMock(return_value=[]),
        count_documents=AsyncMock(return_value=0),
        list_segments=AsyncMock(return_value=[]),
        count_segments=AsyncMock(return_value=0),
    )
    dataset = {"dataset_id": "dataset-a", "tenant_id": "tenant-a", "content_revision": 1}
    knowledge = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value=dataset),
        _effective_dataset_permission=AsyncMock(return_value="viewer"),
    )
    service = DocumentService(settings=None, database=database)  # type: ignore[arg-type]
    service._ks = knowledge  # type: ignore[assignment]
    user = UserContext(user_id="user-a", tenant_id="tenant-a")
    await service.list_documents_page(user, "dataset-a")
    await service.list_segments_page(user, "dataset-a")
    for method in (database.list_documents, database.count_documents,
                   database.list_segments, database.count_segments):
        assert method.await_args.kwargs["active_only"] is True
        assert method.await_args.kwargs["tenant_id"] == "tenant-a"

    knowledge._effective_dataset_permission.return_value = "editor"
    await service.list_documents_page(user, "dataset-a")
    await service.list_segments_page(user, "dataset-a")
    for method in (database.list_documents, database.count_documents,
                   database.list_segments, database.count_segments):
        assert method.await_args.kwargs["active_only"] is False
