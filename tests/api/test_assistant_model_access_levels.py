from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from pydantic import ValidationError

from src.api import deps
from src.api.schemas.assistant import AssistantChatRequest
from src.api.v1 import assistant as assistant_routes
from src.core.auth.user_resolver import UserContext
from src.services.assistant_entry.model_access import (
    check_model_permission,
    effective_chat_model_id,
    user_can_access_model,
)


@pytest.mark.parametrize(
    "payload",
    [
        {"message": "hello", "kb_dataset_ids": [f"dataset-{i}" for i in range(9)]},
        {"message": "hello", "kb_dataset_ids": ["dataset-a", "dataset-a"]},
        {"message": "hello", "kb_dataset_ids": ["x" * 129]},
        {"message": "hello", "kb_include_images": True},
    ],
)
def test_assistant_request_rejects_unbounded_or_multimodal_kb_scope(payload) -> None:
    with pytest.raises(ValidationError):
        AssistantChatRequest.model_validate(payload)


def test_assistant_request_rejects_unresumable_plan_confirmation() -> None:
    with pytest.raises(ValidationError, match="durable plan approval"):
        AssistantChatRequest.model_validate({"message": "hello", "confirm_plan": True})


def test_assistant_request_requires_local_node_device_and_grant_selection() -> None:
    assert AssistantChatRequest.model_validate(
        {"message": "hello", "os_agent_enabled": True}
    ).local_node_device_id is None

    with pytest.raises(ValidationError, match="selectors require one device"):
        AssistantChatRequest.model_validate(
            {"message": "hello", "local_node_device_id": "device-a"}
        )

    request = AssistantChatRequest.model_validate(
        {
            "message": "analyze authorized files",
            "os_agent_enabled": True,
            "local_node_device_id": "device-a",
            "local_node_grant_ids": ["grant-a"],
        }
    )
    assert request.local_node_device_id == "device-a"
    assert request.local_node_grant_ids == ["grant-a"]


def test_assistant_request_openapi_marks_plan_confirmation_as_false_only() -> None:
    schema = AssistantChatRequest.model_json_schema()["properties"]["confirm_plan"]

    assert schema["const"] is False
    assert schema["default"] is False


def test_streaming_gateway_rejects_plan_confirmation_before_runtime(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def no_rate_limit(*args, **kwargs) -> None:
        del args, kwargs

    app = FastAPI()
    app.include_router(assistant_routes.router)
    app.dependency_overrides[deps.get_user_context] = lambda: UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        is_authenticated=True,
    )
    monkeypatch.setattr(deps, "enforce_rate_limit", no_rate_limit)

    response = TestClient(app).post(
        "/assistant/chat/stream",
        json={"message": "hello", "confirm_plan": True},
    )

    assert response.status_code == 422
    assert "confirm_plan" in response.text


@pytest.mark.asyncio
async def test_gateway_resolves_and_authorizes_omitted_default_model() -> None:
    permission_lookups: list[tuple[str, str]] = []

    async def get_access_level(tenant_id: str, model_id: str) -> str:
        permission_lookups.append((tenant_id, model_id))
        return "public"

    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(settings=SimpleNamespace(default_model="tenant-default-model")))
    )
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        is_authenticated=True,
    )
    model_id = effective_chat_model_id(request, None)
    await check_model_permission(
        user,
        model_id,
        SimpleNamespace(get_access_level=get_access_level),
    )

    assert model_id == "tenant-default-model"
    assert permission_lookups == [("tenant-1", "tenant-default-model")]


@pytest.mark.asyncio
async def test_gateway_denies_omitted_restricted_default_model_before_runtime() -> None:
    async def get_access_level(_tenant_id: str, model_id: str) -> str:
        assert model_id == "restricted-default-model"
        return "premium"

    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(settings=SimpleNamespace(default_model="restricted-default-model")))
    )
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier="normal",
        is_authenticated=True,
    )
    model_id = effective_chat_model_id(request, None)
    with pytest.raises(HTTPException) as exc:
        await check_model_permission(
            user,
            model_id,
            SimpleNamespace(get_access_level=get_access_level),
        )
    assert exc.value.status_code == 403


@pytest.mark.parametrize(
    ("tier", "roles", "access_level", "expected"),
    [
        ("normal", [], "public", True),
        ("normal", [], "premium", False),
        ("premium", [], "premium", True),
        ("normal", [], "admin", False),
        ("admin", [], "admin", True),
        ("normal", ["admin"], "admin", True),
    ],
)
def test_user_model_access_preserves_known_access_level_rules(
    tier: str,
    roles: list[str],
    access_level: str,
    expected: bool,
) -> None:
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier=tier,
        roles=roles,
        is_authenticated=True,
    )

    assert user_can_access_model(user, access_level) is expected


@pytest.mark.parametrize("tier", ["normal", "admin"])
def test_user_model_access_rejects_unknown_access_level_for_every_tier(tier: str) -> None:
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier=tier,
        is_authenticated=True,
    )

    assert user_can_access_model(user, "corrupt") is False


@pytest.mark.asyncio
async def test_model_permission_rejects_dirty_database_access_level() -> None:
    user = UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier="admin",
        is_authenticated=True,
    )
    model_meta = SimpleNamespace()

    async def get_access_level(_tenant_id: str, _model_id: str) -> str:
        return "corrupt"

    model_meta.get_access_level = get_access_level

    with pytest.raises(HTTPException) as exc_info:
        await check_model_permission(user, "model-1", model_meta)

    assert exc_info.value.status_code == 403


@pytest.mark.parametrize("path", ["/assistant/chat", "/assistant/chat/stream"])
def test_v1_chat_edge_rejects_anonymous_callers_before_model_resolution(
    monkeypatch: pytest.MonkeyPatch,
    path: str,
) -> None:
    """An unauthenticated caller must get 401, not a model-catalog answer.

    ``get_user_context`` mints an anonymous guest on tenant ``public`` instead
    of rejecting; before the edge guard the request reached
    ``_check_model_permission`` and came back ``400 Unknown model: <default>``,
    disclosing the configured default model and leaving anonymous turns one
    ``llm_models`` row away from actually running.
    """

    async def no_rate_limit(*args, **kwargs) -> None:  # pragma: no cover - guard fires first
        del args, kwargs

    def unexpected_model_meta(*args, **kwargs):  # pragma: no cover - guard fires first
        raise AssertionError("anonymous request reached model resolution")

    app = FastAPI()
    app.include_router(assistant_routes.router)
    app.state.settings = SimpleNamespace(default_model="tenant-default-model")
    app.state.model_meta = SimpleNamespace(get_access_level=unexpected_model_meta)
    app.dependency_overrides[deps.get_user_context] = lambda: UserContext(
        user_id="anon:203.0.113.7",
        tenant_id="public",
        tier="anonymous",
        is_authenticated=False,
        roles=["guest"],
    )
    monkeypatch.setattr(deps, "enforce_rate_limit", no_rate_limit)

    response = TestClient(app).post(path, json={"message": "hello"})

    assert response.status_code == 401
    assert "tenant-default-model" not in response.text
