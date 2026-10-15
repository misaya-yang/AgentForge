from __future__ import annotations

from decimal import Decimal
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from src.services.llm.model_catalog_sync import ModelCatalogSyncService
from src.services.llm.model_service import ModelService


class FakeProviderService:
    def __init__(self, provider: dict[str, Any], api_key: str | None = "secret"):
        self.provider = provider
        self.api_key = api_key

    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        if self.provider["provider_id"] == provider_id:
            return self.provider
        return None

    async def get_runtime_provider_config(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        assert provider_id == self.provider["provider_id"]
        return {**self.provider, "api_key": self.api_key}


class FakeModelService:
    def __init__(self):
        self.calls: list[dict[str, Any]] = []

    async def upsert_model_from_catalog(self, **kwargs: Any) -> tuple[str, dict[str, Any], bool]:
        self.calls.append(kwargs)
        return (
            "created",
            {
                "model_id": kwargs["model_id"],
                "provider_id": kwargs["provider_id"],
                "display_name": kwargs["display_name"],
                "is_enabled": True,
            },
            True,
        )


class FakeHTTPResponse:
    def __init__(self, status_code: int, payload: dict[str, Any]):
        self.status_code = status_code
        self._payload = payload

    def json(self) -> dict[str, Any]:
        return self._payload


class FakeHTTPClient:
    def __init__(self, response: FakeHTTPResponse):
        self.response = response
        self.calls: list[dict[str, Any]] = []

    async def get(self, url: str, *, params: dict[str, str], timeout: float) -> FakeHTTPResponse:
        self.calls.append({"url": url, "params": params, "timeout": timeout})
        return self.response


@pytest.mark.asyncio
async def test_dashscope_catalog_sync_creates_trusted_models() -> None:
    provider_service = FakeProviderService(
        {
            "provider_id": "dashscope-cn",
            "api_type": "openai",
            "base_url": "https://dashscope.aliyuncs.com/compatible-mode",
        }
    )
    model_service = FakeModelService()
    sync_service = ModelCatalogSyncService(provider_service, model_service)  # type: ignore[arg-type]

    result = await sync_service.sync_provider_models(
        tenant_id="tenant-a",
        provider_id="dashscope-cn",
    )

    created_ids = {item["model_id"] for item in result["created_models"]}
    assert "qwen3.8-flash" in created_ids
    assert "qwen3.7-plus" in created_ids
    assert "qwen3.7-max" in created_ids
    assert "qwen3.6-plus" in created_ids
    assert result["skipped_models"] == []
    assert all(call["provider_id"] == "dashscope-cn" for call in model_service.calls)


@pytest.mark.asyncio
async def test_google_discovery_skips_unknown_models_until_catalog_trusts_them() -> None:
    provider_service = FakeProviderService(
        {
            "provider_id": "google",
            "api_type": "google",
            "base_url": "https://generativelanguage.googleapis.com",
        }
    )
    model_service = FakeModelService()
    http_client = FakeHTTPClient(
        FakeHTTPResponse(
            200,
            {
                "models": [
                    {
                        "name": "models/gemini-3.5-flash",
                        "displayName": "Gemini3.5-flash",
                        "inputTokenLimit": 1000000,
                        "outputTokenLimit": 8192,
                        "supportedGenerationMethods": ["generateContent"],
                    },
                    {
                        "name": "models/gemini-unreviewed-preview",
                        "displayName": "Gemini Unreviewed",
                        "supportedGenerationMethods": ["generateContent"],
                    },
                ]
            },
        )
    )
    sync_service = ModelCatalogSyncService(
        provider_service,  # type: ignore[arg-type]
        model_service,  # type: ignore[arg-type]
        http_client=http_client,  # type: ignore[arg-type]
    )

    result = await sync_service.sync_provider_models(
        tenant_id="tenant-a",
        provider_id="google",
    )

    assert any(item["model_id"] == "gemini-3.5-flash" for item in result["created_models"])
    assert {
        "model_id": "gemini-unreviewed-preview",
        "reason": "discovered_model_not_in_trusted_catalog",
    } in result["skipped_models"]
    assert http_client.calls[0]["params"] == {"key": "secret"}


@pytest.mark.asyncio
async def test_sync_without_discovery_uses_trusted_catalog_only() -> None:
    provider_service = FakeProviderService(
        {
            "provider_id": "google",
            "api_type": "google",
            "base_url": "https://generativelanguage.googleapis.com",
        }
    )
    model_service = FakeModelService()
    http_client = FakeHTTPClient(FakeHTTPResponse(500, {"models": []}))
    sync_service = ModelCatalogSyncService(
        provider_service,  # type: ignore[arg-type]
        model_service,  # type: ignore[arg-type]
        http_client=http_client,  # type: ignore[arg-type]
    )

    result = await sync_service.sync_provider_models(
        tenant_id="tenant-a",
        provider_id="google",
        discover=False,
    )

    assert any(item["model_id"] == "gemini-3.5-flash" for item in result["created_models"])
    assert result["skipped_models"] == []
    assert result["discovery_warnings"] == []
    assert http_client.calls == []


@pytest.mark.asyncio
async def test_model_catalog_upsert_preserves_existing_disabled_state() -> None:
    db = MagicMock()
    existing = {
        "model_id": "gemini-3.5-flash",
        "tenant_id": "tenant-a",
        "provider_id": "google",
        "display_name": "Old Gemini",
        "context_window": 1000,
        "max_output_tokens": 100,
        "supports_vision": False,
        "supports_tools": True,
        "input_price_per_1k": Decimal("0"),
        "output_price_per_1k": Decimal("0"),
        "access_level": "public",
        "is_enabled": False,
        "sort_order": 1,
        "created_at": "2026-05-25T00:00:00Z",
        "updated_at": "2026-05-25T00:00:00Z",
    }
    updated = {
        **existing,
        "display_name": "Gemini3.5-flash",
        "context_window": 1000000,
        "is_enabled": False,
    }
    # fetchrow call order: get_provider_model, capability pre-check read in
    # update_model (supports_* flags are revision-relevant), then the UPDATE.
    db.fetchrow = AsyncMock(side_effect=[existing, existing, updated])
    db.fetch = AsyncMock(return_value=[])
    db.execute = AsyncMock()
    service = ModelService(database=db)

    with patch("src.services.llm.model_service.get_pricing_service") as mock_get_pricing:
        pricing = MagicMock()
        pricing.update_pricing = AsyncMock()
        mock_get_pricing.return_value = pricing

        status, result, capability_changed = await service.upsert_model_from_catalog(
            tenant_id="tenant-a",
            provider_id="google",
            model_id="gemini-3.5-flash",
            display_name="Gemini3.5-flash",
            context_window=1000000,
        )

    assert status == "updated"
    assert result["is_enabled"] is False
    update_sql = db.fetchrow.call_args_list[2].args[0].split("RETURNING")[0]
    assert "is_enabled =" not in update_sql
