"""PRD T8.1: knowledge:read / knowledge:write are enforced at the gateway proxy.

Done-when coverage: a read-scope API key cannot write through /knowledge or
/kb-tools, while retrieval (POST ``…/retrieve``) stays available to a read key.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from starlette.responses import Response

from src.api.v1 import kb_tools, knowledge
from src.api.v1._proxy_utils import enforce_knowledge_scope
from src.core.auth.user_resolver import UserContext


def _request(method: str, *, scopes: object = None) -> SimpleNamespace:
    key_info: dict = {}
    if scopes is not None:
        key_info["scopes"] = scopes
    return SimpleNamespace(method=method, state=SimpleNamespace(api_key_info=key_info))


def _request_no_key() -> SimpleNamespace:
    # JWT / static-key / anonymous callers never set api_key_info.
    return SimpleNamespace(method="POST", state=SimpleNamespace())


def _user() -> UserContext:
    return UserContext(user_id="user-1", tenant_id="tenant-1", is_authenticated=True)


@pytest.mark.parametrize("module", [knowledge, kb_tools])
@pytest.mark.asyncio
async def test_read_scope_key_get_forwards(monkeypatch, module) -> None:
    upstream = AsyncMock(return_value=Response(status_code=204))
    monkeypatch.setattr(module, "proxy_to_kb_service", upstream)
    request = _request("GET", scopes=["knowledge:read"])

    response = await getattr(module, f"proxy_{'kb_tools' if module is kb_tools else 'knowledge'}")(
        path="datasets",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 204
    upstream.assert_awaited_once()


@pytest.mark.parametrize(
    ("module", "method", "path"),
    [
        (knowledge, "POST", "datasets"),
        (knowledge, "PUT", "datasets/ds-1"),
        (knowledge, "PATCH", "datasets/ds-1/documents/doc-1/segments/seg-1"),
        (knowledge, "DELETE", "datasets/ds-1"),
        (kb_tools, "POST", "datasets"),
        (kb_tools, "DELETE", "datasets/ds-1"),
    ],
)
@pytest.mark.asyncio
async def test_read_scope_key_cannot_write(monkeypatch, module, method, path) -> None:
    upstream = AsyncMock()
    monkeypatch.setattr(module, "proxy_to_kb_service", upstream)
    request = _request(method, scopes=["knowledge:read"])

    with pytest.raises(HTTPException) as exc_info:
        await getattr(module, f"proxy_{'kb_tools' if module is kb_tools else 'knowledge'}")(
            path=path,
            request=request,
            user=_user(),
            rate_limiter=None,
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == "knowledge:write"
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_read_scope_key_may_retrieve_via_post(monkeypatch) -> None:
    upstream = AsyncMock(return_value=Response(status_code=200))
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request("POST", scopes=["knowledge:read"])

    response = await knowledge.proxy_knowledge(
        path="ds-1/retrieve",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 200
    upstream.assert_awaited_once()


@pytest.mark.asyncio
async def test_read_scope_key_may_retrieve_batch_via_post(monkeypatch) -> None:
    upstream = AsyncMock(return_value=Response(status_code=200))
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request("POST", scopes=["knowledge:read"])

    response = await knowledge.proxy_knowledge(
        path="ds-1/retrieve_batch",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 200
    upstream.assert_awaited_once()


@pytest.mark.parametrize("method", ["PUT", "PATCH", "DELETE"])
@pytest.mark.asyncio
async def test_read_scope_key_cannot_write_to_retrieval_path(monkeypatch, method) -> None:
    """Only POST is the read-only retrieval verb; other methods stay write-gated."""
    upstream = AsyncMock()
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request(method, scopes=["knowledge:read"])

    with pytest.raises(HTTPException) as exc_info:
        await knowledge.proxy_knowledge(
            path="ds-1/retrieve",
            request=request,
            user=_user(),
            rate_limiter=None,
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == "knowledge:write"
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_write_only_key_cannot_read(monkeypatch) -> None:
    upstream = AsyncMock()
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request("GET", scopes=["knowledge:write"])

    with pytest.raises(HTTPException) as exc_info:
        await knowledge.proxy_knowledge(
            path="datasets",
            request=request,
            user=_user(),
            rate_limiter=None,
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == "knowledge:read"
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_write_scope_key_cannot_retrieve(monkeypatch) -> None:
    upstream = AsyncMock()
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request("POST", scopes=["knowledge:write"])

    with pytest.raises(HTTPException) as exc_info:
        await knowledge.proxy_knowledge(
            path="ds-1/retrieve",
            request=request,
            user=_user(),
            rate_limiter=None,
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == "knowledge:read"
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_full_scope_key_can_write(monkeypatch) -> None:
    upstream = AsyncMock(return_value=Response(status_code=201))
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request("POST", scopes=["knowledge:read", "knowledge:write"])

    response = await knowledge.proxy_knowledge(
        path="datasets/ds-1/documents/upload",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 201
    upstream.assert_awaited_once()


@pytest.mark.parametrize(
    "key_info",
    [
        {},
        {"scopes": []},
        {"scopes": [""]},
        {"permissions": []},
    ],
)
@pytest.mark.parametrize(
    ("method", "path", "required_scope"),
    [
        ("GET", "datasets", "knowledge:read"),
        ("POST", "datasets", "knowledge:write"),
    ],
)
@pytest.mark.asyncio
async def test_empty_scope_db_key_fails_closed(
    monkeypatch,
    key_info,
    method,
    path,
    required_scope,
) -> None:
    upstream = AsyncMock()
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = SimpleNamespace(
        method=method,
        state=SimpleNamespace(api_key_info=key_info),
    )

    with pytest.raises(HTTPException) as exc_info:
        await knowledge.proxy_knowledge(
            path=path,
            request=request,
            user=_user(),
            rate_limiter=None,
        )

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == required_scope
    assert exc_info.value.detail["key_scopes"] == []
    assert "reissue or update" in exc_info.value.detail["message"]
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_non_key_caller_passes_through(monkeypatch) -> None:
    """JWT/session callers are outside the API-key scope model."""
    upstream = AsyncMock(return_value=Response(status_code=204))
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = _request_no_key()

    response = await knowledge.proxy_knowledge(
        path="datasets",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 204
    upstream.assert_awaited_once()


@pytest.mark.asyncio
async def test_explicitly_absent_api_key_info_passes_through(monkeypatch) -> None:
    """JWT/session resolution may leave an explicit ``None`` marker."""
    upstream = AsyncMock(return_value=Response(status_code=204))
    monkeypatch.setattr(knowledge, "proxy_to_kb_service", upstream)
    request = SimpleNamespace(
        method="POST",
        state=SimpleNamespace(api_key_info=None),
    )

    response = await knowledge.proxy_knowledge(
        path="datasets",
        request=request,
        user=_user(),
        rate_limiter=None,
    )

    assert response.status_code == 204
    upstream.assert_awaited_once()


def test_permission_column_is_read_as_scopes() -> None:
    # The gateway schema has exposed the key scope list under both names.
    request = SimpleNamespace(
        method="POST",
        state=SimpleNamespace(api_key_info={"permissions": "knowledge:read"}),
    )

    with pytest.raises(HTTPException) as exc_info:
        enforce_knowledge_scope(request, path="datasets")

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail["required_scope"] == "knowledge:write"


def test_missing_state_is_tolerated() -> None:
    # Requests built without state (unit-test doubles) must not crash.
    enforce_knowledge_scope(SimpleNamespace(), path="datasets")
