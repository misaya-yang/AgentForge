"""Actual FastAPI metadata and in-memory generic-stream wire behavior."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType, StreamEventType
from ai_gateway_core.exceptions import ValidationFailedError
from fastapi import FastAPI

from src.api.deps import get_dispatcher, get_user_context
from src.api.v1.stream import router
from src.core.auth.user_resolver import UserContext
from src.main import create_app
from src.models.request import ContentItem
from src.models.response import StreamChunk

ROOT = Path(__file__).resolve().parents[2]


def test_actual_application_export_and_published_stream_media_agree():
    actual = create_app().openapi()["paths"]["/api/v1/stream"]["post"]
    published = json.loads((ROOT / "sdk/openapi.json").read_text())["paths"]["/api/v1/stream"]["post"]
    assert actual["operationId"] == published["operationId"] == "stream_api_v1_stream_post"
    assert actual["responses"]["200"] == published["responses"]["200"]
    assert actual["responses"]["200"]["content"] == {
        "text/event-stream": {"schema": {"type": "string"}}
    }
    assert actual["requestBody"] == published["requestBody"]
    assert actual["responses"]["422"] == published["responses"]["422"]


def invoke(*, ending="success", preflight_failure=False, body=None):
    calls = []

    async def chunks(request, **kwargs):
        calls.append((request, kwargs))
        yield StreamChunk(
            request_id=request.request_id,
            chunk_index=0,
            content=ContentItem(type=ContentType.TEXT, data="partial text"),
            event_type=StreamEventType.TEXT_DELTA,
        )
        if ending == "error":
            raise ValidationFailedError("fixture_stream_failed")
        yield StreamChunk(
            request_id=request.request_id,
            chunk_index=1,
            content=ContentItem(type=ContentType.TEXT, data=""),
            event_type=StreamEventType.FINAL,
            is_final=True,
        )

    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    dispatcher = SimpleNamespace(stream=chunks)
    app.dependency_overrides[get_dispatcher] = lambda: dispatcher
    app.dependency_overrides[get_user_context] = lambda: UserContext(
        user_id="verified-user", tenant_id="verified-tenant", roles=["user"]
    )
    if preflight_failure:
        app.state.registry = SimpleNamespace(
            get=AsyncMock(side_effect=ValidationFailedError("fixture_preflight_failed"))
        )

    async def request():
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app), base_url="http://fixture.invalid", trust_env=False
        ) as client:
            return await client.post("/api/v1/stream", json=body if body is not None else {
                "service_id": "fixture", "inputs": [{"type": "text", "data": "hello"}],
                "user_id": "untrusted-user", "tenant_id": "untrusted-tenant",
            })

    return asyncio.run(request()), calls


@pytest.mark.parametrize("ending", ["success", "error"])
def test_actual_route_preserves_sse_frames_and_post_header_errors(ending):
    response, calls = invoke(ending=ending)
    assert response.status_code == 200
    assert response.headers["content-type"].startswith("text/event-stream")
    assert len(calls) == 1
    assert calls[0][0].user_id == "verified-user"
    assert calls[0][0].tenant_id == "verified-tenant"
    data = [json.loads(line[6:]) for line in response.text.splitlines() if line.startswith("data: {")]
    assert data[0]["content"]["data"] == "partial text"
    if ending == "error":
        assert "event: error\ndata: fixture_stream_failed\n\n" in response.text
        assert not any(chunk["is_final"] for chunk in data)
    else:
        assert data[-1]["is_final"] is True
        assert "event: error" not in response.text


def test_preflight_error_remains_json_and_does_not_open_stream():
    response, calls = invoke(preflight_failure=True)
    assert response.status_code == 403
    assert response.headers["content-type"].startswith("application/json")
    assert response.json() == {"detail": "fixture_preflight_failed"}
    assert calls == []


def test_request_validation_remains_json_and_does_not_open_stream():
    response, calls = invoke(body={})
    assert response.status_code == 422
    assert response.headers["content-type"].startswith("application/json")
    assert isinstance(response.json()["detail"], list)
    assert calls == []
