from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

import pytest
from fastapi import HTTPException
from pydantic import ValidationError

from src.api.v1._assistant_routes import attachment_refs
from src.api.v2.agent import ThreadCreateRequest
from src.core.auth.user_resolver import UserContext
from src.services.agent_runtime.control import thread_lifecycle
from src.services.agent_runtime.control.types import AgentRuntimeControlError


def test_v2_thread_provider_is_additive_and_cannot_override_version():
    assert ThreadCreateRequest(model_id="unique").provider_id is None
    assert ThreadCreateRequest(model_id="same", provider_id="second").provider_id == "second"
    for value in ["", "x" * 256, 42]:
        with pytest.raises(ValidationError):
            ThreadCreateRequest(provider_id=value)
    with pytest.raises(ValidationError):
        ThreadCreateRequest(agent_id=uuid4(), agent_version_id=uuid4(), provider_id="other")


@pytest.mark.parametrize("provider, revision", [("second", 9), (None, 3)])
async def test_thread_bootstrap_reads_the_selected_provider_capability_revision(provider, revision):
    seen = []
    async def get_model(tenant, model, **kwargs):
        seen.append((tenant, model, kwargs))
        return {"is_enabled": True, "capability_revision": revision}
    catalog = AsyncMock()
    bind = AsyncMock(return_value={"runtime_thread_id": "existing"})
    plane = SimpleNamespace(
        model_service=SimpleNamespace(get_model=get_model),
        _readonly_capability_payload=lambda _, **kwargs: {"capability_revision": kwargs["capability_revision"]},
        _fetch_capability_catalog=catalog,
        _existing_thread=AsyncMock(return_value={"runtime_thread_id": "existing"}),
        _dynamic_tool_fingerprint=lambda value: str(value["capability_revision"]),
        _bind_dynamic_tool_fingerprint=bind,
    )
    result = await thread_lifecycle.ensure_thread(
        plane, tenant_id="tenant", user_id="user", session_id="session",
        model_id="same", provider_id=provider,
    )
    assert result == {"runtime_thread_id": "existing"}
    assert seen == [("tenant", "same", {"provider_id": provider} if provider else {})]
    assert catalog.await_args.kwargs["capability_revision"] == revision
    assert bind.await_args.kwargs["fingerprint"] == str(revision)


async def test_disabled_selected_model_stops_before_catalog_or_thread():
    plane = SimpleNamespace(model_service=SimpleNamespace(get_model=AsyncMock(return_value={"is_enabled": False})))
    with pytest.raises(AgentRuntimeControlError):
        await thread_lifecycle.ensure_thread(plane, tenant_id="tenant", user_id="user", session_id="session", model_id="same", provider_id="disabled")


@pytest.mark.parametrize("provider, vision", [("vision-provider", True), ("text-provider", False)])
@pytest.mark.parametrize("is_upload", [False, True])
async def test_image_attachment_checks_the_selected_provider(monkeypatch, provider, vision, is_upload):
    owner = UserContext(user_id="user", tenant_id="tenant", tier="normal", roles=[], is_authenticated=True)
    get_model = AsyncMock(return_value={"is_enabled": True, "supports_vision": vision})
    monkeypatch.setattr(attachment_refs, "assistant_model_service", lambda _: SimpleNamespace(get_model=get_model))
    storage = SimpleNamespace(get_artifact=AsyncMock(return_value=SimpleNamespace(session_id="session", size_bytes=10, mime_type="image/png")), create_artifact=AsyncMock())
    monkeypatch.setattr(attachment_refs, "_user_upload_bytes", AsyncMock(return_value=b"image"))
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: storage)
    reference = "/uploads/tenant/user/abcdef01_20260923_150000.png" if is_upload else "art_0123456789abcdef"
    args = dict(session_id="session", model_id="same", provider_id=provider, refs=[reference])
    if vision:
        bound = await attachment_refs.bind_assistant_attachment_refs(None, owner, **args)
        assert len(bound) == 1 and bound[0].startswith("art_")
        if not is_upload:
            assert bound == args["refs"]
    else:
        with pytest.raises(HTTPException) as error:
            await attachment_refs.bind_assistant_attachment_refs(None, owner, **args)
        assert error.value.detail == {"code": "VISION_MODEL_REQUIRED"}
    get_model.assert_awaited_once_with("tenant", "same", provider_id=provider)


async def test_legacy_vision_lookup_does_not_invent_a_provider(monkeypatch):
    owner = UserContext(user_id="user", tenant_id="tenant", tier="normal", roles=[], is_authenticated=True)
    get_model = AsyncMock(return_value={"is_enabled": True, "supports_vision": True})
    monkeypatch.setattr(attachment_refs, "assistant_model_service", lambda _: SimpleNamespace(get_model=get_model))
    await attachment_refs._require_vision_model(None, owner, "unique")
    get_model.assert_awaited_once_with("tenant", "unique")
