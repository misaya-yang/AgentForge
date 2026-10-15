"""The generic session-delete route must release the Runtime thread first.

``assistant_runtime_threads`` references ``sessions`` ON DELETE RESTRICT, so
dropping the session row before tombstoning the thread raises a raw
ForeignKeyViolationError and the client sees a 500.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException, Request

from src.api.v1 import sessions as sessions_api
from src.core.auth.user_resolver import UserContext
from src.services.agent_runtime import AgentRuntimeControlError


def _build_request() -> Request:
    app = FastAPI()
    scope = {
        "type": "http",
        "method": "DELETE",
        "path": "/api/v1/sessions/session-1",
        "headers": [],
        "app": app,
    }
    return Request(scope)


def _owned_session(user: UserContext) -> SimpleNamespace:
    return SimpleNamespace(user_id=user.user_id, tenant_id=user.tenant_id)


@pytest.mark.asyncio
async def test_delete_session_tombstones_runtime_thread_before_deleting_row() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = _owned_session(user)
    request = _build_request()
    control = AsyncMock()
    control.cleanup_session.return_value = True
    request.app.state.agent_runtime_control = control

    response = await sessions_api.delete_session(
        "session-1", request, session_manager=session_manager, user=user
    )

    assert response == {"session_id": "session-1", "status": "deleted"}
    control.cleanup_session.assert_awaited_once_with(
        tenant_id="tenant_1",
        user_id="user_1",
        session_id="session-1",
    )
    session_manager.delete.assert_awaited_once_with("session-1")


@pytest.mark.asyncio
async def test_delete_session_preserves_row_when_runtime_cleanup_fails() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = _owned_session(user)
    request = _build_request()
    control = AsyncMock()
    control.cleanup_session.side_effect = AgentRuntimeControlError(
        "AI_PLATFORM_AGENT_RUNTIME_UNAVAILABLE", status_code=503
    )
    request.app.state.agent_runtime_control = control

    with pytest.raises(HTTPException) as error:
        await sessions_api.delete_session(
            "session-1", request, session_manager=session_manager, user=user
        )

    assert error.value.status_code == 503
    session_manager.delete.assert_not_awaited()


@pytest.mark.asyncio
async def test_delete_session_still_works_without_a_runtime_control() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = _owned_session(user)
    request = _build_request()

    response = await sessions_api.delete_session(
        "session-1", request, session_manager=session_manager, user=user
    )

    assert response == {"session_id": "session-1", "status": "deleted"}
    session_manager.delete.assert_awaited_once_with("session-1")
