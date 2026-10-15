from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import (
    list_query_feedback,
    list_query_history,
    upsert_query_feedback,
)
from knowledge_service.api.schemas.knowledge import QueryFeedbackUpsertSchema
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.query_observability import (
    QueryObservationConflictError,
    decode_query_cursor,
    encode_query_cursor,
    query_fingerprint,
)

USER = UserContext(user_id="user-a", tenant_id="tenant-a")


def test_query_fingerprint_is_unicode_case_and_whitespace_stable() -> None:
    assert query_fingerprint("  ＡＢＣ   Policy ") == query_fingerprint("abc policy")


def test_query_cursor_round_trips_and_rejects_invalid_input() -> None:
    created_at = datetime(2026, 8, 29, 12, tzinfo=timezone.utc)
    cursor = encode_query_cursor(created_at, "row-a")
    assert decode_query_cursor(cursor) == (created_at, "row-a")
    with pytest.raises(ValueError, match="invalid query pagination cursor"):
        decode_query_cursor("not-json")


@pytest.mark.asyncio
async def test_query_history_route_forwards_filters() -> None:
    captured: dict[str, Any] = {}

    class Service:
        async def list_query_history(self, user, dataset_id, **kwargs):
            captured.update({"user": user, "dataset_id": dataset_id, **kwargs})
            return {"queries": [], "next_cursor": None, "has_more": False}

    response = await list_query_history(
        "dataset-a",
        limit=25,
        zero_results=True,
        mode="hybrid",
        cursor=None,
        svc=Service(),  # type: ignore[arg-type]
        user=USER,
    )
    assert response["queries"] == []
    assert captured == {
        "user": USER,
        "dataset_id": "dataset-a",
        "limit": 25,
        "zero_results": True,
        "mode": "hybrid",
        "cursor": None,
    }


@pytest.mark.asyncio
async def test_feedback_route_maps_trace_conflict_to_409() -> None:
    class Service:
        async def upsert_query_feedback(self, *_args, **_kwargs):
            raise QueryObservationConflictError("fingerprint mismatch")

    payload = QueryFeedbackUpsertSchema(
        trace_id="d04d53c8-acde-49d0-b3eb-49890dbd5673",
        query_fingerprint="a" * 64,
        target_type="qa_answer",
        rating="negative",
        reason_code="incorrect",
    )
    with pytest.raises(HTTPException) as exc_info:
        await upsert_query_feedback(
            "dataset-a",
            payload=payload,
            svc=Service(),  # type: ignore[arg-type]
            user=USER,
        )
    assert exc_info.value.status_code == 409


@pytest.mark.asyncio
async def test_feedback_read_route_maps_permission_to_403() -> None:
    class Service:
        async def list_query_feedback(self, *_args, **_kwargs):
            raise PermissionDeniedError("editor required")

    with pytest.raises(HTTPException) as exc_info:
        await list_query_feedback(
            "dataset-a",
            limit=50,
            rating="negative",
            reason_code=None,
            target_type=None,
            trace_id=None,
            cursor=None,
            svc=Service(),  # type: ignore[arg-type]
            user=USER,
        )
    assert exc_info.value.status_code == 403


class TenantDatabase:
    def __init__(self) -> None:
        self.list_calls: list[dict[str, Any]] = []

    async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
        return {
            "dataset_id": "dataset-b",
            "tenant_id": "tenant-b",
            "visibility": "private",
            "created_by": "user-b",
        }

    async def get_dataset_permission(self, *_args) -> None:
        return None

    async def list_dataset_queries(self, **kwargs: Any) -> list[dict[str, Any]]:
        self.list_calls.append(kwargs)
        return []


@pytest.mark.asyncio
async def test_cross_tenant_query_read_fails_before_persistence() -> None:
    database = TenantDatabase()
    service = DatasetService(SimpleNamespace(), database)  # type: ignore[arg-type]
    with pytest.raises(PermissionDeniedError):
        await service.list_query_history(USER, "dataset-b")
    assert database.list_calls == []
