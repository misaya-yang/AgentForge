"""Compatibility tests for assistant session listing."""

from datetime import datetime, timedelta
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.exceptions import SessionAlreadyExistsError
from fastapi import HTTPException

from src.api.v1.sessions import (
    SessionCreate,
    _list_assistant_sessions_for_service_id,
    create_session,
)
from src.core.auth.user_resolver import UserContext
from src.models.session import Session


@pytest.mark.asyncio
async def test_list_sessions_builtin_includes_legacy_and_blank():
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    now = datetime.utcnow()

    builtin = Session(
        session_id="s_builtin",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
        updated_at=now - timedelta(minutes=1),
    )
    legacy = Session(
        session_id="s_legacy",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="assistant",
        updated_at=now - timedelta(minutes=2),
    )
    blank = Session(
        session_id="s_blank",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id=None,
        updated_at=now - timedelta(minutes=3),
    )
    other = Session(
        session_id="s_other",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="langgraph",
        updated_at=now,
    )

    async def list_sessions(*, user_id, tenant_id, service_id, limit, status="active"):
        assert service_id is None
        return [other, builtin, legacy, blank]

    session_manager = AsyncMock()
    session_manager.list_sessions.side_effect = list_sessions

    sessions = await _list_assistant_sessions_for_service_id(
        session_manager, user, limit=10, service_id="__builtin_assistant__"
    )

    assert [s.session_id for s in sessions] == ["s_builtin", "s_legacy", "s_blank"]


@pytest.mark.asyncio
async def test_create_session_honors_client_session_id() -> None:
    client_session_id = "11111111-1111-4111-8111-111111111111"
    captured: dict[str, object] = {}

    class _Manager:
        async def create(self, **kwargs: object) -> SimpleNamespace:
            captured.update(kwargs)
            return SimpleNamespace(session_id=kwargs["session_id"])

    result = await create_session(
        body=SessionCreate(
            session_id=client_session_id,
            service_id="__builtin_assistant__",
            metadata={"title": "hello"},
        ),
        session_manager=_Manager(),  # type: ignore[arg-type]
        user=UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True),
    )

    assert result["session_id"] == client_session_id
    assert captured["session_id"] == client_session_id
    assert captured["user_id"] == "user_1"
    assert captured["service_id"] == "__builtin_assistant__"
    assert captured["fail_if_exists"] is True


@pytest.mark.asyncio
async def test_create_session_rejects_existing_client_session_id() -> None:
    class _Manager:
        async def create(self, **_kwargs: object) -> SimpleNamespace:
            raise SessionAlreadyExistsError("session already exists")

    with pytest.raises(HTTPException) as exc_info:
        await create_session(
            body=SessionCreate(
                session_id="11111111-1111-4111-8111-111111111111",
            ),
            session_manager=_Manager(),  # type: ignore[arg-type]
            user=UserContext(
                user_id="user_1", tenant_id="tenant_1", is_authenticated=True
            ),
        )

    assert exc_info.value.status_code == 409
