"""Persistent assistant memory preference and owner-scoped management."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.v1._assistant_routes import memory as routes
from src.core.auth.user_resolver import UserContext
from src.services.assistant_entry.memory_controls import (
    MEMORY_CONTROL_KEY,
    effective_assistant_memory_mode,
)


def _request(database):
    return SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(database=database)))


def _user():
    return UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)


@pytest.mark.asyncio
async def test_disabled_preference_forces_off_even_when_old_client_requests_auto() -> None:
    database = SimpleNamespace(fetchrow=AsyncMock(return_value={"value": False}))
    mode = await effective_assistant_memory_mode(_request(database), "tenant-a", "user-a", "auto")
    assert mode == "off"
    database.fetchrow.assert_awaited_once_with(
        "SELECT value FROM user_memory WHERE tenant_id = $1 AND user_id = $2 AND key = $3",
        "tenant-a", "user-a", MEMORY_CONTROL_KEY,
    )


@pytest.mark.asyncio
async def test_memory_api_reads_and_updates_only_current_actor() -> None:
    state = {MEMORY_CONTROL_KEY: False, "likes": "tea"}

    async def fetchrow(_sql, tenant_id, user_id, key):
        assert (tenant_id, user_id) == ("tenant-a", "user-a")
        return {"value": state[key]} if key in state else None

    async def execute(sql, *args):
        assert args[0:2] == ("tenant-a", "user-a")
        if "INSERT INTO user_memory" in sql:
            import json

            state[args[2]] = json.loads(args[3])
        elif "DELETE FROM user_memory" in sql:
            state.pop(args[2], None)
        return "OK"

    database = SimpleNamespace(
        fetchrow=fetchrow,
        execute=execute,
        fetch=AsyncMock(return_value=[{
            "key": "likes", "value": "tea", "namespace": "default",
            "source": "assistant", "created_at": None, "updated_at": None,
            "expires_at": None,
        }]),
    )
    request = _request(database)
    user = _user()
    listed = await routes.get_memory(request, user)
    assert listed["enabled"] is False
    assert listed["items"][0]["key"] == "likes"
    assert MEMORY_CONTROL_KEY not in str(listed)

    preference = await routes.update_memory_preference(
        routes.MemoryPreferenceUpdate(enabled=True), request, user,
    )
    assert preference == {"enabled": True}
    edited = await routes.put_memory_item(
        routes.MemoryItemUpdate(key="likes", value="coffee"), request, user,
    )
    assert edited == {"key": "likes", "value": "coffee"}
    assert await routes.delete_memory_item(request, "likes", user) == {
        "key": "likes", "status": "deleted",
    }


@pytest.mark.asyncio
async def test_reserved_memory_control_cannot_be_edited_as_content() -> None:
    with pytest.raises(HTTPException) as exc:
        await routes.put_memory_item(
            routes.MemoryItemUpdate(key=MEMORY_CONTROL_KEY, value="true"),
            _request(SimpleNamespace()), _user(),
        )
    assert exc.value.status_code == 400
