"""HTTP-level eval list/detail after shipped capture → ingest_trace roundtrip."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context
from src.api.v1 import eval as eval_routes
from src.config.settings import Settings
from src.core.auth.rbac import RBAC
from src.main import create_app
from tests.services.eval.in_memory_trace_repository import InMemoryTraceRepository
from tests.services.eval.trace_roundtrip_fixtures import seed_family


async def _seed_repo() -> tuple[InMemoryTraceRepository, dict[str, str]]:
    repo = InMemoryTraceRepository()
    trace_ids: dict[str, str] = {}
    for family in ("assistant", "langgraph_proxy", "rag"):
        trace_ids[family] = await seed_family(repo, family, request_suffix="http")
    return repo, trace_ids


@pytest.fixture
def eval_client(monkeypatch: pytest.MonkeyPatch) -> tuple[TestClient, dict[str, str]]:
    repo, trace_ids = asyncio.run(_seed_repo())
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    settings = Settings()
    app = create_app()
    app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    app.state.database = SimpleNamespace(enabled=True)

    async def _fake_auth() -> AuthContext:
        return AuthContext(
            user_id="user-a",
            tenant_id="tenant-a",
            roles=["admin"],
            permissions=["console:eval:view", "console:eval:run"],
            is_authenticated=True,
        )

    app.dependency_overrides[get_auth_context] = _fake_auth
    return TestClient(app), trace_ids


def test_http_eval_summary_list_and_detail_span_trees(
    eval_client: tuple[TestClient, dict[str, str]],
    capsys: pytest.CaptureFixture[str],
) -> None:
    client, trace_ids = eval_client

    summary = client.get("/api/v1/eval/summary")
    assert summary.status_code == 200
    summary_body = summary.json()
    print(f"LIVE_HTTP summary status={summary.status_code} total={summary_body['total_traces']}")
    assert summary_body["total_traces"] == 3

    for family in ("assistant", "langgraph_proxy", "rag"):
        listed = client.get("/api/v1/eval/traces", params={"trace_family": family})
        assert listed.status_code == 200
        listed_body = listed.json()
        print(
            f"LIVE_HTTP list family={family} status={listed.status_code} "
            f"count={listed_body['total']}"
        )
        assert listed_body["total"] == 1
        assert listed_body["traces"][0]["trace_id"] == trace_ids[family]

        detail = client.get(
            f"/api/v1/eval/traces/{trace_ids[family]}",
            params={"trace_family": family},
        )
        assert detail.status_code == 200
        detail_body = detail.json()
        spans = detail_body["spans"]
        lifecycle = next(s for s in spans if s["parent_span_id"] is None)
        children = [s for s in spans if s["parent_span_id"] is not None]
        print(
            f"LIVE_HTTP detail family={family} lifecycle={lifecycle['span_id']} "
            f"children={len(children)} child_parent={children[0]['parent_span_id']}"
        )
        assert children
        assert children[0]["parent_span_id"] == lifecycle["span_id"]

    out = capsys.readouterr().out
    print(out)
    assert "LIVE_HTTP summary status=200" in out
    assert "LIVE_HTTP detail family=rag" in out
