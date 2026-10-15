from __future__ import annotations

import inspect
from types import SimpleNamespace
from typing import Any
from unittest.mock import MagicMock

import pytest

from src.services.llm import gateway_model_meta, startup_seeder
from src.services.llm.startup_seeder import ProviderSeedResult


@pytest.mark.asyncio
async def test_agent_control_plane_seeds_models_and_preserves_order(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src import main

    for key in ("DASHSCOPE_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"):
        monkeypatch.delenv(key, raising=False)

    events: list[str] = []
    seed_kwargs: dict[str, Any] = {}
    catalog_kwargs: dict[str, Any] = {}
    provider_service = object()

    class FakeModelService:
        async def sync_pricing_from_llm_models(self, **kwargs: Any) -> int:
            events.append("pricing")
            assert kwargs == {"tenant_id": "default", "include_disabled": True}
            return 3

    model_service = FakeModelService()

    async def fake_seed_startup_providers(**kwargs: Any) -> ProviderSeedResult:
        events.append("seed")
        seed_kwargs.update(kwargs)
        return ProviderSeedResult(
            configured_providers=("dashscope", "deepseek"),
            runtime_configured_providers=frozenset({"dashscope"}),
        )

    async def fake_sync_startup_model_catalog(**kwargs: Any) -> None:
        events.append("catalog")
        catalog_kwargs.update(kwargs)

    class FakeGatewayModelMeta:
        def __init__(self, model: object, provider: object, **kwargs: Any) -> None:
            events.append("model_meta")
            assert model is model_service
            assert provider is provider_service
            assert kwargs == {"runtime_configured_providers": {"dashscope"}}

    monkeypatch.setattr(startup_seeder, "seed_startup_providers", fake_seed_startup_providers)
    monkeypatch.setattr(
        startup_seeder,
        "sync_startup_model_catalog",
        fake_sync_startup_model_catalog,
    )
    monkeypatch.setattr(gateway_model_meta, "GatewayModelMeta", FakeGatewayModelMeta)
    log = MagicMock()
    monkeypatch.setattr(main, "logger", log)

    knowledge_service = SimpleNamespace(vlm_service=object())
    session_manager = object()
    app = SimpleNamespace(
        state=SimpleNamespace(
            provider_service=provider_service,
            model_service=model_service,
            knowledge_service=knowledge_service,
            session_manager=session_manager,
        )
    )
    settings = SimpleNamespace(
        knowledge=SimpleNamespace(dashscope=SimpleNamespace(api_key="legacy-dashscope-secret"))
    )

    await main._init_agent_control_plane(app, settings)

    assert events == ["seed", "catalog", "model_meta", "pricing"]
    assert seed_kwargs == {
        "provider_service": provider_service,
        "legacy_dashscope_api_key": "legacy-dashscope-secret",
        "tenant_id": "default",
        "log": log,
    }
    assert catalog_kwargs == {
        "provider_service": provider_service,
        "model_service": model_service,
        "configured_providers": ["dashscope", "deepseek"],
        "tenant_id": "default",
        "log": log,
    }
    assert isinstance(app.state.model_meta, FakeGatewayModelMeta)
    assert main._init_agent_control_plane.__name__ == "_init_agent_control_plane"
    assert list(inspect.signature(main._init_agent_control_plane).parameters) == [
        "app",
        "settings",
    ]
    assert "legacy-dashscope-secret" not in repr(log.method_calls)
