"""First-run onboarding state endpoint tests.

Follows the pattern of ``test_assistant_local_nodes_gateway.py``: a bare
``FastAPI()`` app with ``include_router`` plus dependency overrides.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import jwt
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context
from src.api.v1 import setup as setup_api
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)

KNOWN_PROVIDERS = {
    "openai",
    "anthropic",
    "deepseek",
    "dashscope",
    "google",
    "google-vertex",
}
SETUP_VIEW_PERMISSION = "console:services:view"
TEST_JWT_SECRET = "test-secret-key-for-setup-state-contract"


class _StubModelMeta:
    """Minimal stand-in for the GatewayModelMeta facade."""

    def __init__(self, *, enabled: list[str] | None = None, configured: list[str] | None = None):
        self._enabled = list(enabled or [])
        self._configured = set(configured or [])

    async def list_enabled_providers(self, _tenant_id: str) -> list[str]:
        return self._enabled

    async def is_provider_configured(self, _tenant_id: str, provider_id: str) -> bool:
        return provider_id in self._configured


def _client(
    model_meta: _StubModelMeta | None = None,
    *,
    settings: Settings | None = None,
    roles: list[str] | None = None,
    permissions: list[str] | None = None,
) -> TestClient:
    app = FastAPI()
    app.include_router(setup_api.router)
    if model_meta is not None:
        app.state.model_meta = model_meta
    app.dependency_overrides[get_auth_context] = lambda: AuthContext(
        tenant_id="tenant-gateway",
        user_id="user-gateway",
        is_authenticated=True,
        roles=roles if roles is not None else ["user"],
        permissions=permissions if permissions is not None else [SETUP_VIEW_PERMISSION],
    )
    app.state.settings = settings or Settings()
    return TestClient(app)


def _real_auth_client(
    model_meta: _StubModelMeta,
    *,
    settings: Settings,
    database=None,
) -> TestClient:
    app = FastAPI()
    app.include_router(setup_api.router)
    app.state.model_meta = model_meta
    app.state.settings = settings
    if database is not None:
        app.state.database = database
    return TestClient(app)


def test_setup_state_reports_unconfigured(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DEFAULT_MODEL", raising=False)
    client = _client(_StubModelMeta())

    response = client.get("/setup/state")

    assert response.status_code == 200
    body = response.json()
    assert body["configured"] is False
    assert sorted(body["missing"]) == sorted(KNOWN_PROVIDERS)
    assert body["mode"] == "ui"
    # Effective deployment default (code default when the env var is unset).
    assert body["default_model"] == "qwen3.8-flash"


def test_setup_state_reports_configured_when_provider_has_credential() -> None:
    client = _client(
        _StubModelMeta(enabled=["dashscope", "openai"], configured=["dashscope"])
    )

    response = client.get("/setup/state")

    assert response.status_code == 200
    body = response.json()
    assert body["configured"] is True
    assert "dashscope" not in body["missing"]
    assert sorted(body["missing"]) == sorted(KNOWN_PROVIDERS - {"dashscope"})


@pytest.mark.parametrize("provider_id", ["dashscope-cn", "dashscope-intl", "tenant-custom"])
def test_setup_state_accepts_database_provider_ids(provider_id: str) -> None:
    client = _client(_StubModelMeta(enabled=[provider_id], configured=[provider_id]))

    response = client.get("/setup/state")

    assert response.status_code == 200
    assert response.json()["configured"] is True


def test_setup_state_is_graceful_without_model_meta() -> None:
    client = _client()  # app.state.model_meta never set

    response = client.get("/setup/state")

    assert response.status_code == 200
    body = response.json()
    assert body["configured"] is False
    assert sorted(body["missing"]) == sorted(KNOWN_PROVIDERS)


def test_setup_state_exposes_environment_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("MODEL_SETUP_MODE", "environment")
    client = _client(
        _StubModelMeta(enabled=["dashscope"], configured=["dashscope"]),
        settings=Settings(_env_file=None),
    )

    response = client.get("/setup/state")

    assert response.status_code == 200
    assert response.json()["mode"] == "environment"


def test_setup_state_returns_default_model_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEFAULT_MODEL", "default-model-from-env")
    client = _client(_StubModelMeta())

    response = client.get("/setup/state")

    assert response.status_code == 200
    assert response.json()["default_model"] == "default-model-from-env"


def test_setup_state_accepts_authorized_api_key_auth() -> None:
    """A tenant API key carrying Services-view permission can use the endpoint."""
    database = SimpleNamespace(
        enabled=True,
        get_api_key=AsyncMock(
            return_value={
                "tenant_id": "tenant-gateway",
                "user_id": "api-user",
                "roles": ["user"],
                "permissions": [SETUP_VIEW_PERMISSION],
                "tier": "normal",
            }
        ),
        get_user_permissions=AsyncMock(return_value=[]),
    )
    settings = Settings(
        authentication=AuthenticationSettings(
            jwt=AuthJWTSettings(enabled=False),
            api_key=AuthAPIKeySettings(enabled=True, keys=[]),
        )
    )
    client = _real_auth_client(_StubModelMeta(), settings=settings, database=database)

    response = client.get("/setup/state", headers={"X-API-Key": "test-key"})

    assert response.status_code == 200
    assert response.json()["configured"] is False


def test_setup_state_accepts_authorized_jwt_auth() -> None:
    settings = Settings(
        authentication=AuthenticationSettings(
            jwt=AuthJWTSettings(enabled=True, secret=TEST_JWT_SECRET, algorithms=["HS256"]),
            api_key=AuthAPIKeySettings(enabled=False),
        )
    )
    token = jwt.encode(
        {
            "sub": "jwt-user",
            "tenant_id": "tenant-gateway",
            "roles": ["user"],
            "permissions": [SETUP_VIEW_PERMISSION],
            "exp": datetime.now(timezone.utc) + timedelta(hours=1),
        },
        TEST_JWT_SECRET,
        algorithm="HS256",
    )
    client = _real_auth_client(_StubModelMeta(), settings=settings)

    response = client.get("/setup/state", headers={"Authorization": f"Bearer {token}"})

    assert response.status_code == 200
    assert response.json()["configured"] is False


def test_setup_state_rejects_authenticated_user_without_services_permission() -> None:
    client = _client(
        _StubModelMeta(enabled=["dashscope"], configured=["dashscope"]),
        roles=["user"],
        permissions=[],
    )

    response = client.get("/setup/state")

    assert response.status_code == 403
    assert response.json()["detail"]["required_permission"] == SETUP_VIEW_PERMISSION
    assert "configured" not in response.json()


def test_setup_state_accepts_role_granted_by_settings_rbac() -> None:
    client = _client(_StubModelMeta(), roles=["manager"], permissions=[])

    response = client.get("/setup/state")

    assert response.status_code == 200


def test_setup_state_rejects_anonymous_guests() -> None:
    app = FastAPI()
    app.include_router(setup_api.router)
    app.state.settings = Settings()
    app.dependency_overrides[get_auth_context] = lambda: AuthContext(
        tenant_id="public",
        user_id="anon:guest",
        is_authenticated=False,
        roles=["guest"],
        permissions=[],
    )
    client = TestClient(app)

    response = client.get("/setup/state")

    assert response.status_code == 401
