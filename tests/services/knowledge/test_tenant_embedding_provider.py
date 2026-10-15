from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.security import encrypt_value
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import embedding_manager as embedding_module
from knowledge_service.services.knowledge.embedding_manager import EmbeddingManager
from knowledge_service.services.knowledge.tenant_provider import (
    TenantEmbeddingCredentialResolver,
)


class ProviderDatabase:
    def __init__(self, rows: list[dict[str, Any]]) -> None:
        self.rows = rows
        self.tenant_ids: list[str] = []

    async def fetch(self, _query: str, tenant_id: str) -> list[dict[str, Any]]:
        self.tenant_ids.append(tenant_id)
        return list(self.rows)


def _settings() -> SimpleNamespace:
    return SimpleNamespace(
        knowledge=SimpleNamespace(
            dashscope=SimpleNamespace(api_key="environment-key"),
            gemini=SimpleNamespace(api_key=""),
            siliconflow=SimpleNamespace(api_key="", base_url=""),
            text_embedding_dimension=1024,
            multimodal_embedding_model="tongyi-embedding-vision-plus",
            multimodal_embedding_max_concurrent=2,
        )
    )


@pytest.mark.asyncio
async def test_resolver_reads_latest_tenant_key_and_normalizes_dashscope_endpoint() -> None:
    encryption_key = "unit-test-encryption-key"
    database = ProviderDatabase(
        [
            {
                "provider_id": "dashscope-intl",
                "api_type": "openai",
                "base_url": "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
                "api_key_encrypted": encrypt_value("first-key", encryption_key),
            }
        ]
    )
    resolver = TenantEmbeddingCredentialResolver(
        database,
        encryption_key=encryption_key,
    )

    first = await resolver.resolve("tenant-a", "dashscope")
    assert first is not None
    assert first.api_key == "first-key"
    assert first.base_url == "https://dashscope-intl.aliyuncs.com/api/v1"

    database.rows[0]["api_key_encrypted"] = encrypt_value(
        "updated-key",
        encryption_key,
    )
    second = await resolver.resolve("tenant-a", "dashscope")
    assert second is not None
    assert second.api_key == "updated-key"
    assert database.tenant_ids == ["tenant-a", "tenant-a"]


@pytest.mark.asyncio
async def test_resolver_does_not_select_another_provider_family() -> None:
    database = ProviderDatabase(
        [
            {
                "provider_id": "openai",
                "api_type": "openai",
                "base_url": "https://api.openai.com",
                "api_key_encrypted": "plaintext-key",
            }
        ]
    )
    resolver = TenantEmbeddingCredentialResolver(database)

    assert await resolver.resolve("tenant-a", "dashscope") is None


@pytest.mark.asyncio
async def test_encrypted_key_with_wrong_server_key_fails_closed() -> None:
    database = ProviderDatabase(
        [
            {
                "provider_id": "dashscope-cn",
                "api_type": "openai",
                "base_url": "https://dashscope.aliyuncs.com/compatible-mode",
                "api_key_encrypted": encrypt_value("secret-key", "correct-key"),
            }
        ]
    )
    resolver = TenantEmbeddingCredentialResolver(
        database,
        encryption_key="wrong-key",
    )

    with pytest.raises(ValidationFailedError, match="could not be decrypted"):
        await resolver.resolve("tenant-a", "dashscope")


@pytest.mark.asyncio
async def test_embedding_manager_prefers_tenant_credential_over_environment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    class Resolver:
        async def resolve(self, tenant_id: str, provider: str) -> SimpleNamespace:
            assert tenant_id == "tenant-a"
            assert provider == "dashscope"
            return SimpleNamespace(
                api_key="tenant-key",
                base_url="https://dashscope-intl.aliyuncs.com/api/v1",
            )

    def capture(config: Any, *, dimension: int | None = None) -> Any:
        captured["config"] = config
        captured["dimension"] = dimension
        return SimpleNamespace(provider=config.provider)

    monkeypatch.setattr(embedding_module, "create_embedding", capture)
    manager = EmbeddingManager(_settings(), credential_resolver=Resolver())

    await manager.get_text_embedder(
        {
            "tenant_id": "tenant-a",
            "embedding_provider": "dashscope",
            "embedding_model": "text-embedding-v4",
            "embedding_dimension": 1024,
            "embedding_config": {},
        }
    )

    assert captured["config"].api_key == "tenant-key"
    assert captured["config"].base_url == "https://dashscope-intl.aliyuncs.com/api/v1"
