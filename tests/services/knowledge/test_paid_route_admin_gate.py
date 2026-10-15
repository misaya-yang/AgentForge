from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes import knowledge as routes
from knowledge_service.api.schemas.knowledge import (
    QAQuerySchema,
    SegmentBatchEnableDisableSchema,
)
from knowledge_service.core.auth.user_resolver import UserContext


class _Poison:
    def __getattribute__(self, name: str):
        raise AssertionError(f"paid route resolved {name} before admin authorization")


@pytest.fixture
def editor() -> UserContext:
    return UserContext(
        user_id="editor-a",
        tenant_id="tenant-a",
        roles=["user"],
        is_authenticated=True,
    )


@pytest.fixture
def admin() -> UserContext:
    return UserContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        is_authenticated=True,
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "route_name",
    ["retrieve_evaluate", "qa_query", "qa_query_stream", "qa_batch_test"],
)
async def test_paid_routes_reject_editor_before_payload_config_or_service_resolution(
    route_name: str,
    editor: UserContext,
) -> None:
    poison = _Poison()

    with pytest.raises(HTTPException) as exc_info:
        if route_name == "retrieve_evaluate":
            await routes.retrieve_evaluate(
                "dataset-a",
                poison,  # type: ignore[arg-type]
                poison,  # type: ignore[arg-type]
                editor,
            )
        else:
            await getattr(routes, route_name)(
                poison,  # request
                "dataset-a",
                poison,  # payload
                poison,  # service
                editor,
                poison,  # settings
            )

    assert exc_info.value.status_code == 403


def test_paid_http_routes_declare_pre_handler_admin_dependency() -> None:
    expected_paths = {
        "/knowledge/{dataset_id}/retrieve_evaluate",
        "/knowledge/{dataset_id}/qa",
        "/knowledge/{dataset_id}/qa/stream",
        "/knowledge/{dataset_id}/qa/batch",
    }
    protected_paths = {
        route.path
        for route in routes.router.routes
        if route.path in expected_paths
        and any(
            dependency.dependency is routes.require_admin_user
            for dependency in route.dependencies
        )
    }

    assert protected_paths == expected_paths


@pytest.mark.asyncio
async def test_admin_can_reach_qa_service_fixture_without_network(
    monkeypatch: pytest.MonkeyPatch,
    admin: UserContext,
) -> None:
    from knowledge_service.services.knowledge import qa_service as qa_module

    result = SimpleNamespace(
        query="question",
        answer="answer",
        context_segments=[],
        retrieval_metadata={},
        retrieval_time_ms=1.0,
        llm_time_ms=2.0,
        total_time_ms=3.0,
        model="server-model",
        tokens_used=4,
        trace_id="00000000-0000-4000-8000-000000000001",
        query_fingerprint="a" * 64,
    )

    class _QAService:
        def __init__(self, svc, llm_config) -> None:
            self.svc = svc
            self.llm_config = llm_config

        async def query(self, **_kwargs):
            return result

        async def close(self) -> None:
            return None

    monkeypatch.setattr(qa_module, "QAService", _QAService)
    llm_config = object()
    build_config = Mock(return_value=llm_config)
    monkeypatch.setattr(routes, "_build_server_qa_llm_config", build_config)
    svc = SimpleNamespace(
        require_dataset_access=AsyncMock(return_value={"dataset_id": "dataset-a"})
    )
    settings = SimpleNamespace()

    response = await routes.qa_query(
        None,  # type: ignore[arg-type]
        "dataset-a",
        QAQuerySchema(query="question"),
        svc,  # type: ignore[arg-type]
        admin,
        settings,  # type: ignore[arg-type]
    )

    assert response["answer"] == "answer"
    assert response["trace_id"] == result.trace_id
    assert response["query_fingerprint"] == result.query_fingerprint
    svc.require_dataset_access.assert_awaited_once_with(
        admin,
        "dataset-a",
        required="editor",
    )
    build_config.assert_called_once()


@pytest.mark.asyncio
async def test_segment_batch_route_uses_typed_service_contract(
    editor: UserContext,
) -> None:
    svc = SimpleNamespace(
        set_segments_enabled_batch=AsyncMock(
            return_value={"success": True, "updated": 1, "total": 1}
        )
    )
    payload = SegmentBatchEnableDisableSchema(
        segment_ids=["  segment-a  "],
        enabled=False,
    )

    response = await routes.batch_enable_segments(
        "dataset-a",
        payload,
        svc,  # type: ignore[arg-type]
        editor,
    )

    assert response == {"success": True, "updated": 1, "total": 1}
    svc.set_segments_enabled_batch.assert_awaited_once_with(
        editor,
        "dataset-a",
        ["segment-a"],
        False,
    )
