from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

import pytest

from src.services.llm import model_catalog_sync
from src.services.llm.startup_seeder import (
    seed_startup_providers,
    sync_startup_model_catalog,
)

_PROVIDER_ENV_KEYS = (
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "DEEPSEEK_API_KEY",
    "DEEPSEEK_BASE_URL",
    "DASHSCOPE_API_KEY",
    "DASHSCOPE_CHAT_API_KEY",
    "DASHSCOPE_BASE_URL",
    "DASHSCOPE_CHAT_BASE_URL",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_API_BACKEND",
    "GOOGLE_CHAT_BACKEND",
    "GOOGLE_VERTEX_MODELS",
    "VERTEX_API_KEY",
    "VERTEX_CHAT_API_KEY",
)


@pytest.fixture(autouse=True)
def isolated_provider_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    import ai_gateway_core.config as endpoint_config

    for key in _PROVIDER_ENV_KEYS:
        monkeypatch.delenv(key, raising=False)
    monkeypatch.setattr(
        endpoint_config,
        "resolve_dashscope",
        lambda _domain: ("", "https://dashscope.aliyuncs.com/compatible-mode"),
    )
    monkeypatch.setattr(
        endpoint_config,
        "resolve_google",
        lambda _domain: ("", "https://generativelanguage.googleapis.com", "ai_studio"),
    )


class FakeProviderService:
    def __init__(
        self,
        *,
        existing: dict[str, dict[str, Any]] | None = None,
        db_providers: list[dict[str, Any]] | None = None,
        fail_get: set[str] | None = None,
        fail_create: set[str] | None = None,
        fail_list: bool = False,
        runtime_keys: dict[str, str] | None = None,
    ) -> None:
        self.existing = existing or {}
        self.db_providers = db_providers or []
        self.fail_get = fail_get or set()
        self.fail_create = fail_create or set()
        self.fail_list = fail_list
        self.runtime_keys = runtime_keys or {}
        self.get_calls: list[tuple[str, str]] = []
        self.runtime_calls: list[tuple[str, str]] = []
        self.create_calls: list[dict[str, Any]] = []
        self.update_calls: list[dict[str, Any]] = []
        self.list_calls: list[tuple[str, bool]] = []

    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any] | None:
        self.get_calls.append((tenant_id, provider_id))
        if provider_id in self.fail_get:
            raise RuntimeError(f"get failed: {provider_id}")
        return self.existing.get(provider_id)

    async def create_provider(self, **kwargs: Any) -> dict[str, Any]:
        self.create_calls.append(kwargs)
        if kwargs["provider_id"] in self.fail_create:
            raise RuntimeError(f"create failed: {kwargs['provider_id']}")
        return kwargs

    async def update_provider(self, **kwargs: Any) -> dict[str, Any]:
        self.update_calls.append(kwargs)
        return kwargs

    async def get_runtime_provider_config(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        self.runtime_calls.append((tenant_id, provider_id))
        return {**self.existing.get(provider_id, {}), "api_key": self.runtime_keys.get(provider_id)}

    async def list_providers(
        self,
        tenant_id: str,
        include_disabled: bool = False,
    ) -> list[dict[str, Any]]:
        self.list_calls.append((tenant_id, include_disabled))
        if self.fail_list:
            raise RuntimeError("list failed")
        return self.db_providers


@pytest.mark.asyncio
async def test_seed_creates_all_default_rows_without_credentials() -> None:
    service = FakeProviderService()

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key="",
        log=MagicMock(),
    )

    assert [call["provider_id"] for call in service.create_calls] == [
        "openai",
        "anthropic",
        "deepseek",
        "dashscope",
        "google",
        "google-vertex",
    ]
    assert all(call["api_key"] is None for call in service.create_calls)
    assert service.list_calls == [("default", False)]
    assert result.configured_providers == ()
    assert result.runtime_configured_providers == frozenset()


@pytest.mark.asyncio
async def test_existing_rows_update_only_when_environment_key_is_present(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "openai-secret")
    monkeypatch.setenv("OPENAI_BASE_URL", "https://openai.example")
    service = FakeProviderService(
        existing={provider_id: {"provider_id": provider_id} for provider_id in (
            "openai",
            "anthropic",
            "deepseek",
            "dashscope",
            "google",
            "google-vertex",
        )}
    )
    log = MagicMock()

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key="",
        log=log,
    )

    assert service.create_calls == []
    assert service.update_calls == [
        {
            "tenant_id": "default",
            "provider_id": "openai",
            "api_key": "openai-secret",
            "api_type": "openai",
            "base_url": "https://openai.example",
        }
    ]
    assert result.configured_providers == ("openai",)
    assert result.runtime_configured_providers == frozenset({"openai"})
    assert "openai-secret" not in repr(log.method_calls)


@pytest.mark.asyncio
@pytest.mark.parametrize("changed", [None, "credential", "endpoint", "api_type"])
async def test_restart_preserves_revision_only_for_unchanged_runtime_configuration(monkeypatch, changed):
    monkeypatch.setenv("OPENAI_API_KEY", "synthetic-startup-credential")
    monkeypatch.setenv("OPENAI_BASE_URL", "https://openai.example")
    row = {"provider_id": "openai", "has_api_key": True, "api_type": "openai",
           "base_url": "https://openai.example", "updated_at": "original-revision"}
    if changed == "endpoint":
        row["base_url"] = "https://previous.example"
    if changed == "api_type":
        row["api_type"] = "anthropic"
    service = FakeProviderService(
        existing={"openai": row},
        runtime_keys={"openai": "previous-synthetic-credential" if changed == "credential" else "synthetic-startup-credential"},
    )
    log = MagicMock()
    for _ in range(2):
        await seed_startup_providers(provider_service=service, legacy_dashscope_api_key="", log=log)
    assert len(service.update_calls) == (0 if changed is None else 2)
    assert row["updated_at"] == "original-revision"
    assert "synthetic-startup-credential" not in repr(log.method_calls)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("resolved_key", "legacy_key", "expected_runtime"),
    [
        ("dashscope-runtime-secret", "legacy-secret", frozenset({"dashscope"})),
        ("", "legacy-secret", frozenset()),
    ],
)
async def test_dashscope_legacy_key_seeds_db_but_not_runtime_provider_set(
    monkeypatch: pytest.MonkeyPatch,
    resolved_key: str,
    legacy_key: str,
    expected_runtime: frozenset[str],
) -> None:
    import ai_gateway_core.config as endpoint_config

    monkeypatch.setattr(
        endpoint_config,
        "resolve_dashscope",
        lambda _domain: (resolved_key, "https://dashscope.example/compatible-mode"),
    )
    service = FakeProviderService()

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key=legacy_key,
        log=MagicMock(),
    )

    dashscope_call = next(
        call for call in service.create_calls if call["provider_id"] == "dashscope"
    )
    assert dashscope_call["api_key"] == (resolved_key or legacy_key)
    assert dashscope_call["base_url"] == "https://dashscope.example/compatible-mode"
    assert result.configured_providers == ("dashscope",)
    assert result.runtime_configured_providers == expected_runtime


@pytest.mark.asyncio
async def test_google_vertex_resolution_seeds_none_base_url(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import ai_gateway_core.config as endpoint_config

    monkeypatch.setattr(
        endpoint_config,
        "resolve_google",
        lambda _domain: ("vertex-secret", "https://vertex.example", "vertex"),
    )
    service = FakeProviderService()

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key="",
        log=MagicMock(),
    )

    google_call = next(call for call in service.create_calls if call["provider_id"] == "google")
    assert google_call["api_key"] == "vertex-secret"
    assert google_call["base_url"] is None
    assert result.configured_providers == ("google",)
    assert result.runtime_configured_providers == frozenset({"google"})


@pytest.mark.asyncio
async def test_db_only_provider_is_configured_but_not_runtime_ready() -> None:
    service = FakeProviderService(
        db_providers=[
            {"provider_id": "deepseek", "has_api_key": True},
            {"provider_id": "custom-provider", "has_api_key": True},
            {"provider_id": "anthropic", "has_api_key": False},
        ]
    )

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key="",
        log=MagicMock(),
    )

    assert result.configured_providers == ("deepseek",)
    assert result.runtime_configured_providers == frozenset()


@pytest.mark.asyncio
async def test_provider_and_list_failures_are_isolated() -> None:
    service = FakeProviderService(
        fail_get={"openai"},
        fail_create={"anthropic"},
        fail_list=True,
    )
    log = MagicMock()

    result = await seed_startup_providers(
        provider_service=service,  # type: ignore[arg-type]
        legacy_dashscope_api_key="",
        log=log,
    )

    assert [provider_id for _, provider_id in service.get_calls] == [
        "openai",
        "anthropic",
        "deepseek",
        "dashscope",
        "google",
        "google-vertex",
    ]
    assert result.configured_providers == ()
    warning_calls = repr(log.warning.call_args_list)
    assert "Failed to sync provider openai" in warning_calls
    assert "Failed to sync provider anthropic" in warning_calls
    assert "Failed to load providers from database" in warning_calls


@pytest.mark.asyncio
async def test_catalog_sync_is_sorted_deduplicated_and_failure_isolated(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[dict[str, Any]] = []
    provider_service = object()
    model_service = object()

    class FakeCatalogSyncService:
        def __init__(self, provider: object, model: object) -> None:
            assert provider is provider_service
            assert model is model_service

        async def sync_provider_models(self, **kwargs: Any) -> dict[str, Any]:
            calls.append(kwargs)
            if kwargs["provider_id"] == "google":
                raise RuntimeError("catalog failed")
            return {"created_models": [{"model_id": "created"}], "updated_models": []}

    monkeypatch.setattr(model_catalog_sync, "ModelCatalogSyncService", FakeCatalogSyncService)
    log = MagicMock()

    await sync_startup_model_catalog(
        provider_service=provider_service,  # type: ignore[arg-type]
        model_service=model_service,  # type: ignore[arg-type]
        configured_providers=["openai", "google", "anthropic", "openai"],
        log=log,
    )

    assert calls == [
        {"tenant_id": "default", "provider_id": "anthropic", "discover": False},
        {"tenant_id": "default", "provider_id": "google", "discover": False},
        {"tenant_id": "default", "provider_id": "openai", "discover": False},
    ]
    assert "Failed to sync startup model catalog for provider %s: %s" in repr(
        log.warning.call_args_list
    )
