"""Channel-owned turns can be controlled without becoming builtin V2 turns."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.persistence.repositories.agent_repository import AgentNotFoundError
from fastapi import HTTPException

from src.api.v2 import agent as routes
from src.core.auth.user_resolver import UserContext
from tests.api.test_agent_v2 import _request


def fixture(**changes):
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    session = SimpleNamespace(
        user_id=user.user_id, tenant_id=user.tenant_id, agent_id="agent-a",
        agent_version_id=None, channel="preview", publication_id=None,
        agent_draft_revision=1,
    )
    for key, value in changes.items():
        setattr(session, key, value)
    repository = SimpleNamespace(get_agent=AsyncMock(return_value={"status": "draft"}))
    request = _request(SimpleNamespace(
        session_manager=SimpleNamespace(get=AsyncMock(return_value=session)),
        agent_repository=repository,
    ))
    return request, user, session, repository


async def test_draft_control_rechecks_access_without_resolving_newer_draft():
    request, user, session, repository = fixture()
    assert await routes._pinned_version_session(request, user, "session-a", allow_channel_pin=True) is session
    assert await routes._pinned_snapshot(request, user, session) == {}
    repository.get_agent.assert_awaited_once_with(
        tenant_id="tenant-a", agent_id="agent-a", user_id="user-a", is_tenant_admin=False,
    )
    with pytest.raises(HTTPException, match="409"):
        await routes._pinned_version_session(request, user, "session-a")


async def test_archived_agent_cannot_approve_original_draft_action():
    request, user, session, repository = fixture()
    repository.get_agent.return_value = {"status": "archived"}
    with pytest.raises(HTTPException) as caught:
        await routes._pinned_snapshot(request, user, session)
    assert caught.value.detail["code"] == "AGENT_RUNTIME_AGENT_UNAVAILABLE"


async def test_revoked_draft_membership_cannot_approve_original_action():
    request, user, session, repository = fixture()
    repository.get_agent.side_effect = AgentNotFoundError("missing")
    with pytest.raises(HTTPException):
        await routes._pinned_snapshot(request, user, session)


async def test_published_controls_resolve_original_version_after_pointer_rollback(monkeypatch):
    snapshot = {"fingerprints": {"spec": "a" * 64}, "publication": {"id": "publication-a"}}
    request, user, session, repository = fixture(
        channel="hosted", publication_id="publication-a", agent_version_id="version-b",
        agent_draft_revision=None, agent_spec_hash="a" * 64,
        runtime_fingerprint=runtime_sha256(snapshot),
    )
    resolution = {"current_version": "version-a"}
    repository.resolve_publication_runtime = AsyncMock(return_value=resolution)
    build = AsyncMock(return_value=snapshot)
    monkeypatch.setattr(routes, "_build_snapshot", build)
    assert await routes._pinned_version_session(request, user, "session-a", allow_channel_pin=True) is session
    assert await routes._pinned_snapshot(request, user, session) == snapshot
    repository.resolve_publication_runtime.assert_awaited_once_with(
        tenant_id="tenant-a", publication_id="publication-a", user_id="user-a",
        is_tenant_admin=False, pinned_version_id="version-b",
    )
    build.assert_awaited_once_with(request, resolution, user, channel="hosted")


@pytest.mark.parametrize("changes", [
    {"agent_version_id": "also-a-version"},
    {"agent_draft_revision": True},
    {"agent_draft_revision": 0},
    {"publication_id": "also-a-publication"},
])
async def test_ambiguous_draft_pin_still_fails_closed(changes):
    request, user, _, repository = fixture(**changes)
    with pytest.raises(HTTPException):
        await routes._pinned_version_session(request, user, "session-a", allow_channel_pin=True)
    repository.get_agent.assert_not_awaited()
