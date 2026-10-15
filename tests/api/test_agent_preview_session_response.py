from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.schemas.agent_runtime import AgentPreviewSessionRequest
from src.api.v1._agent_runtime_routes import preview
from src.api.v1.agent_runtime import router


async def test_draft_preview_returns_server_resolved_native_capabilities(monkeypatch) -> None:
    class Repository:
        async def resolve_preview_runtime(self, **_kwargs):
            return {"spec": {}}

    snapshot = {
        "agent_id": "agent-one",
        "capabilities": [
            {
                "type": "platform",
                "id": "safe_search",
                "schema_hash": f"sha256:{'0' * 64}",
                "risk": "low",
                "config": {"requires_confirmation": False},
            },
            {
                "type": "mcp",
                "id": "unrelated_mcp",
                "schema_hash": f"sha256:{'1' * 64}",
                "risk": "low",
                "config": {},
            },
        ],
    }

    async def build_snapshot(*_args, **_kwargs):
        return snapshot

    async def bind_session(*_args, **_kwargs):
        return None

    monkeypatch.setattr(preview, "_require_actor", lambda *_args: None)
    monkeypatch.setattr(preview, "reject_client_agent_forgery", lambda *_args: None)
    monkeypatch.setattr(preview, "_repository", lambda *_args: Repository())
    monkeypatch.setattr(preview, "_is_tenant_admin", lambda *_args: False)
    monkeypatch.setattr(preview, "_build_snapshot", build_snapshot)
    monkeypatch.setattr(preview, "_bind_session", bind_session)
    monkeypatch.setattr(preview, "_request_id", lambda *_args: "request-one")

    response = await preview.create_preview_session(
        "agent-one",
        AgentPreviewSessionRequest(draft_revision=2),
        SimpleNamespace(),
        SimpleNamespace(tenant_id="tenant-one", user_id="user-one"),
    )

    assert response.draft_revision == 2
    assert response.channel == "preview"
    assert [capability.name for capability in response.effective_capabilities] == ["safe_search"]


@pytest.mark.parametrize(
    ("changed_field", "changed_value"),
    [
        ("user_id", "other-user"),
        ("tenant_id", "other-tenant"),
        ("agent_id", "other-agent"),
        ("channel", "hosted"),
        ("publication_id", "publication-one"),
    ],
)
async def test_preview_recovery_rejects_other_pins(
    monkeypatch, changed_field: str, changed_value: str
) -> None:
    session = SimpleNamespace(
        user_id="user-one",
        tenant_id="tenant-one",
        agent_id="agent-one",
        channel="preview",
        publication_id=None,
        agent_version_id=None,
        agent_draft_revision=2,
    )
    setattr(session, changed_field, changed_value)

    class SessionManager:
        async def get(self, _session_id):
            return session

    monkeypatch.setattr(preview, "_session_manager", lambda _request: SessionManager())
    request = SimpleNamespace(state=SimpleNamespace())
    user = SimpleNamespace(is_authenticated=True, user_id="user-one", tenant_id="tenant-one")
    with pytest.raises(HTTPException) as caught:
        await preview.get_preview_session("agent-one", "session-one", request, user)
    assert caught.value.status_code == 404


async def test_preview_recovery_returns_authoritative_pin(monkeypatch) -> None:
    session = SimpleNamespace(
        user_id="user-one",
        tenant_id="tenant-one",
        agent_id="agent-one",
        channel="preview",
        publication_id=None,
        agent_version_id="version-one",
        agent_draft_revision=None,
    )

    class SessionManager:
        async def get(self, _session_id):
            return session

    monkeypatch.setattr(preview, "_session_manager", lambda _request: SessionManager())
    request = SimpleNamespace(state=SimpleNamespace())
    user = SimpleNamespace(is_authenticated=True, user_id="user-one", tenant_id="tenant-one")
    result = await preview.get_preview_session("agent-one", "session-one", request, user)
    assert result.agent_version_id == "version-one"
    assert result.draft_revision is None
    assert result.channel == "preview"
    assert any(
        route.path == "/agents/{agent_id}/preview/sessions/{session_id}"
        and "GET" in route.methods
        for route in router.routes
    )
