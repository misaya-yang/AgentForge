from __future__ import annotations

from typing import Any

import pytest
from fastapi import HTTPException

from src.api.schemas.providers import ProviderFromTemplateCreate
from src.api.v1.providers import create_provider_from_template, list_templates
from src.core.auth.user_resolver import UserContext


def _admin() -> UserContext:
    return UserContext(
        user_id="admin",
        tenant_id="tenant-a",
        tier="enterprise",
        is_authenticated=True,
        roles=["admin"],
        ip="127.0.0.1",
    )


class FakeProviderService:
    def __init__(self, existing: dict[str, Any] | None = None):
        self.existing = existing
        self.created: dict[str, Any] | None = None
        self.updated: dict[str, Any] | None = None

    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        if self.existing and self.existing["provider_id"] == provider_id:
            return self.existing
        return None

    async def create_provider(self, **kwargs: Any) -> dict[str, Any]:
        self.created = kwargs
        return {
            **kwargs,
            "has_api_key": bool(kwargs.get("api_key")),
            "created_at": "2026-05-25T00:00:00Z",
            "updated_at": "2026-05-25T00:00:00Z",
        }

    async def update_provider(self, **kwargs: Any) -> dict[str, Any]:
        self.updated = kwargs
        return {
            **(self.existing or {}),
            **kwargs,
            "has_api_key": bool(kwargs.get("api_key")) or bool((self.existing or {}).get("has_api_key")),
            "created_at": "2026-05-25T00:00:00Z",
            "updated_at": "2026-05-25T00:00:00Z",
        }


@pytest.mark.asyncio
async def test_list_provider_templates_returns_guided_templates() -> None:
    result = await list_templates(user=_admin())

    template_ids = {item["template_id"] for item in result}
    assert "dashscope-cn" in template_ids
    assert "google-ai-studio" in template_ids


@pytest.mark.asyncio
async def test_create_provider_from_template_hides_raw_fields_for_dashscope() -> None:
    service = FakeProviderService()

    result = await create_provider_from_template(
        body=ProviderFromTemplateCreate(
            template_id="dashscope-cn",
            api_key="server-side-secret",
            is_enabled=True,
        ),
        provider_service=service,
        user=_admin(),
    )

    assert result["provider_id"] == "dashscope-cn"
    assert result["display_name"] == "Qwen/DashScope China"
    assert result["api_type"] == "openai"
    assert result["base_url"] == "https://dashscope.aliyuncs.com/compatible-mode"
    assert service.created is not None
    assert service.created["api_key"] == "server-side-secret"
    assert service.created["metadata"] == {"wire_protocol": "responses_v1"}


@pytest.mark.asyncio
async def test_mainstream_template_rejects_manual_provider_id_override() -> None:
    service = FakeProviderService()

    with pytest.raises(HTTPException) as exc:
        await create_provider_from_template(
            body=ProviderFromTemplateCreate(
                template_id="google-ai-studio",
                provider_id="anthropic",
                api_key="server-side-secret",
            ),
            provider_service=service,
            user=_admin(),
        )

    assert exc.value.status_code == 422
    assert exc.value.detail == "TEMPLATE_PROVIDER_ID_IS_FIXED_FOR_MAINSTREAM_PROVIDERS"


@pytest.mark.asyncio
async def test_create_provider_from_template_updates_existing_provider() -> None:
    service = FakeProviderService(
        existing={
            "provider_id": "google",
            "tenant_id": "tenant-a",
            "display_name": "Old Google",
            "api_type": "google",
            "base_url": "https://generativelanguage.googleapis.com",
            "has_api_key": True,
            "is_enabled": True,
        }
    )

    result = await create_provider_from_template(
        body=ProviderFromTemplateCreate(
            template_id="google-ai-studio",
            api_key=None,
            is_enabled=True,
        ),
        provider_service=service,
        user=_admin(),
    )

    assert result["provider_id"] == "google"
    assert result["display_name"] == "Google Gemini"
    assert service.updated is not None
    assert service.updated["api_key"] is None
