"""D4 (frontend handoff): list pagination contract at the route layer.

The body stays a bare array (backwards compatible with the shipped frontend
client); the page total rides in ``X-Total-Count``. These tests pin that
limit/offset reach the service layer verbatim and that the count call shares
the same scope as the page it accompanies.
"""

from __future__ import annotations

from typing import Any

import pytest
from knowledge_service.api.routes.knowledge import list_documents, list_segments
from knowledge_service.core.auth.user_resolver import UserContext

USER = UserContext(user_id="user-a", tenant_id="tenant-a")


class _FakeResponse:
    def __init__(self) -> None:
        self.headers: dict[str, str] = {}


class _DocumentService:
    def __init__(self, rows: list[dict[str, Any]], total: int) -> None:
        self.rows = rows
        self.total = total
        self.list_calls: list[dict[str, Any]] = []
    async def list_documents_page(
        self, user: UserContext, dataset_id: str, **kwargs: Any
    ) -> dict[str, Any]:
        self.list_calls.append(
            {"user": user, "dataset_id": dataset_id, **kwargs}
        )
        return {"items": list(self.rows), "total": self.total, **kwargs}

    async def list_segments_page(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str | None = None,
        q: str | None = None,
        **kwargs: Any,
    ) -> dict[str, Any]:
        self.list_calls.append(
            {
                "user": user,
                "dataset_id": dataset_id,
                "document_id": document_id,
                "q": q,
                **kwargs,
            }
        )
        return {"items": list(self.rows), "total": self.total, **kwargs}


@pytest.mark.asyncio
async def test_document_list_forwards_pagination_and_sets_total_header() -> None:
    svc = _DocumentService(rows=[{"document_id": "d-1"}], total=250)
    response = _FakeResponse()

    result = await list_documents(
        "dataset-a", limit=25, offset=50, response=response, svc=svc, user=USER  # type: ignore[arg-type]
    )

    assert result == [{"document_id": "d-1"}]
    assert response.headers["X-Total-Count"] == "250"
    assert svc.list_calls == [
        {"user": USER, "dataset_id": "dataset-a", "limit": 25, "offset": 50}
    ]


@pytest.mark.asyncio
async def test_document_list_defaults_to_the_documented_page_cap() -> None:
    svc = _DocumentService(rows=[], total=0)
    response = _FakeResponse()

    await list_documents(
        "dataset-a", response=response, svc=svc, user=USER,  # type: ignore[arg-type]
        limit=200, offset=0,
    )

    # Default page == pre-pagination behaviour: the 200-row cap, offset 0.
    assert svc.list_calls[0]["limit"] == 200
    assert svc.list_calls[0]["offset"] == 0
    assert response.headers["X-Total-Count"] == "0"


@pytest.mark.asyncio
async def test_segment_list_forwards_filters_pagination_and_total() -> None:
    svc = _DocumentService(rows=[{"segment_id": "s-1"}], total=501)
    response = _FakeResponse()

    result = await list_segments(
        "dataset-a",
        document_id="document-a",
        q="合规",
        limit=100,
        offset=200,
        response=response,
        svc=svc,  # type: ignore[arg-type]
        user=USER,
    )

    assert result == [{"segment_id": "s-1"}]
    assert response.headers["X-Total-Count"] == "501"
    assert svc.list_calls == [
        {
            "user": USER,
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "q": "合规",
            "limit": 100,
            "offset": 200,
        }
    ]
