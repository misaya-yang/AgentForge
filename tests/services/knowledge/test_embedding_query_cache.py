from __future__ import annotations

import sys
from collections.abc import Iterator
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.services.knowledge import embedding as embedding_module
from knowledge_service.services.knowledge.embedding import (
    BaseEmbedding,
    DashScopeEmbedding,
    DashScopeMultimodalEmbedding,
    EmbeddingConfig,
    EmbeddingError,
    _embedder_cache,
    _get_query_cache_key,
    _make_cache_key,
    _query_embedding_cache,
    get_cached_embedder,
)


class _CountingEmbedding(BaseEmbedding):
    def __init__(
        self,
        *,
        dimension: int,
        base_url: str,
        result_value: float,
        profile: dict[str, Any] | None = None,
        api_key: str = "test-api-key",
        timeout_seconds: float = 30.0,
    ) -> None:
        super().__init__(provider="dashscope", model="text-embedding-v4", dimension=dimension)
        self.api_key = api_key
        self.base_url = base_url
        self.result_value = result_value
        self.calls = 0
        self._configure_cache_profile(
            EmbeddingConfig(
                provider="dashscope",
                model="text-embedding-v4",
                api_key=api_key,
                base_url=base_url,
                timeout_seconds=timeout_seconds,
                extra=profile or {},
            )
        )

    async def embed_texts(
        self,
        texts: list[str],
        text_type: str | None = None,
    ) -> list[list[float]]:
        _ = text_type
        self.calls += 1
        return [[self.result_value] * self.dimension for _text in texts]


@pytest.fixture(autouse=True)
def _clear_query_embedding_cache() -> Iterator[None]:
    _embedder_cache.clear()
    _query_embedding_cache.clear()
    yield
    _embedder_cache.clear()
    _query_embedding_cache.clear()


@pytest.mark.asyncio
async def test_query_cache_isolated_by_embedding_dimension() -> None:
    embedder_512 = _CountingEmbedding(
        dimension=512,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=0.512,
    )
    embedder_1024 = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=1.024,
    )

    result_512 = await embedder_512.embed_query("same query")
    result_1024 = await embedder_1024.embed_query("same query")

    assert len(result_512) == 512
    assert len(result_1024) == 1024
    assert embedder_512.calls == 1
    assert embedder_1024.calls == 1


@pytest.mark.asyncio
async def test_query_cache_isolated_by_region_endpoint() -> None:
    singapore = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=1.0,
    )
    beijing = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.cn-beijing.maas.aliyuncs.com/api/v1",
        result_value=2.0,
    )

    singapore_result = await singapore.embed_query("same query")
    beijing_result = await beijing.embed_query("same query")

    assert singapore_result[0] == 1.0
    assert beijing_result[0] == 2.0
    assert singapore.calls == 1
    assert beijing.calls == 1


@pytest.mark.asyncio
async def test_query_cache_isolated_by_credential() -> None:
    first = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=1.0,
        api_key="credential-a",
    )
    second = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=2.0,
        api_key="credential-b",
    )

    first_result = await first.embed_query("same query")
    second_result = await second.embed_query("same query")

    assert first_result[0] == 1.0
    assert second_result[0] == 2.0
    assert first.calls == 1
    assert second.calls == 1


@pytest.mark.asyncio
async def test_query_cache_hits_for_equivalent_semantic_configuration() -> None:
    first = _CountingEmbedding(
        dimension=1024,
        base_url="HTTPS://WORKSPACE.AP-SOUTHEAST-1.MAAS.ALIYUNCS.COM/api/v1/",
        result_value=1.0,
        profile={"instruct": "not consumed by the current provider adapter"},
        timeout_seconds=5.0,
    )
    equivalent = _CountingEmbedding(
        dimension=1024,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=2.0,
        profile={"instruct": "different but still not consumed"},
        timeout_seconds=60.0,
    )

    first_result = await first.embed_query("same query")
    cached_result = await equivalent.embed_query("same query")

    assert cached_result == first_result
    assert first.calls == 1
    assert equivalent.calls == 0


@pytest.mark.asyncio
async def test_query_cache_returns_copy_instead_of_mutable_cached_value() -> None:
    first = _CountingEmbedding(
        dimension=4,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=1.0,
    )
    equivalent = _CountingEmbedding(
        dimension=4,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        result_value=2.0,
    )

    first_result = await first.embed_query("same query")
    first_result[0] = 999.0
    cached_result = await equivalent.embed_query("same query")

    assert cached_result == [1.0] * 4
    assert equivalent.calls == 0


def test_query_cache_key_includes_text_type() -> None:
    query_key = _get_query_cache_key(
        "dashscope",
        "text-embedding-v4",
        "same text",
        profile={"dimension": 1024, "text_type": "query"},
    )
    document_key = _get_query_cache_key(
        "dashscope",
        "text-embedding-v4",
        "same text",
        profile={"dimension": 1024, "text_type": "document"},
    )

    assert query_key != document_key


@pytest.mark.asyncio
async def test_embedder_cache_reuses_only_equivalent_configuration(monkeypatch) -> None:
    created: list[object] = []

    def fake_create_embedding(config: EmbeddingConfig, dimension: int | None = None) -> object:
        _ = (config, dimension)
        instance = object()
        created.append(instance)
        return instance

    monkeypatch.setattr(embedding_module, "create_embedding", fake_create_embedding)
    base = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key="credential-a",
        base_url="HTTPS://WORKSPACE.AP-SOUTHEAST-1.MAAS.ALIYUNCS.COM/api/v1/",
        timeout_seconds=5.0,
        extra={"profile": "retrieval-v1"},
    )
    equivalent = EmbeddingConfig(
        provider="ALIYUN",
        model="text-embedding-v4",
        api_key="credential-a",
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        timeout_seconds=60.0,
        extra={"profile": "retrieval-v1"},
    )
    different_endpoint = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key="credential-a",
        base_url="https://workspace.cn-beijing.maas.aliyuncs.com/api/v1",
        extra={"profile": "retrieval-v1"},
    )
    different_profile = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key="credential-a",
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        extra={"profile": "retrieval-v2"},
    )
    different_credential = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key="credential-b",
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        extra={"profile": "retrieval-v1"},
    )

    first = await get_cached_embedder(base, dimension=512)

    assert await get_cached_embedder(equivalent, dimension=512) is first
    assert await get_cached_embedder(base, dimension=1024) is not first
    assert await get_cached_embedder(different_endpoint, dimension=512) is not first
    assert await get_cached_embedder(different_profile, dimension=512) is not first
    assert await get_cached_embedder(different_credential, dimension=512) is not first
    assert len(created) == 5


@pytest.mark.asyncio
async def test_dashscope_instances_keep_endpoint_and_dimension_per_call(monkeypatch) -> None:
    calls: list[dict[str, Any]] = []

    class FakeTextEmbedding:
        @staticmethod
        def call(**kwargs: Any) -> SimpleNamespace:
            calls.append(kwargs)
            dimension = kwargs.get("dimension") or 1024
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0] * dimension}]},
            )

    fake_dashscope = SimpleNamespace(
        TextEmbedding=FakeTextEmbedding,
        base_http_api_url="https://global.example.invalid/api/v1",
    )
    monkeypatch.setitem(sys.modules, "dashscope", fake_dashscope)
    singapore_input = "HTTPS://WORKSPACE.AP-SOUTHEAST-1.MAAS.ALIYUNCS.COM/api/v1/"
    singapore_url = "https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1"
    beijing_url = "https://workspace.cn-beijing.maas.aliyuncs.com/api/v1"
    singapore = DashScopeEmbedding(
        model="text-embedding-v4",
        api_key="credential-a",
        dimension=512,
        base_url=singapore_input,
    )
    beijing = DashScopeEmbedding(
        model="text-embedding-v4",
        api_key="credential-a",
        dimension=1024,
        base_url=beijing_url,
    )

    await singapore.embed_texts(["first"], text_type="query")
    await beijing.embed_texts(["second"], text_type="query")
    await singapore.embed_texts(["third"], text_type="query")
    defaulted = DashScopeEmbedding(
        model="text-embedding-v4",
        api_key="credential-a",
        dimension=512,
    )
    fake_dashscope.base_http_api_url = "https://mutated-global.example.invalid/api/v1"
    await defaulted.embed_texts(["fourth"], text_type="query")

    assert [(call["base_address"], call["dimension"]) for call in calls] == [
        (singapore_url, 512),
        (beijing_url, 1024),
        (singapore_url, 512),
        (DashScopeEmbedding.DASHSCOPE_API_URL, 512),
    ]
    assert all(call["text_type"] == "query" for call in calls)
    assert fake_dashscope.base_http_api_url == "https://mutated-global.example.invalid/api/v1"


@pytest.mark.asyncio
async def test_dashscope_dimension_mismatch_fails_closed(monkeypatch) -> None:
    class FakeTextEmbedding:
        @staticmethod
        def call(**_kwargs: Any) -> SimpleNamespace:
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0] * 1024}]},
            )

    monkeypatch.setitem(
        sys.modules,
        "dashscope",
        SimpleNamespace(TextEmbedding=FakeTextEmbedding),
    )
    embedder = DashScopeEmbedding(
        model="text-embedding-v4",
        api_key="credential-a",
        dimension=512,
    )

    with pytest.raises(EmbeddingError, match="dimension mismatch"):
        await embedder.embed_texts(["same query"], text_type="query")


@pytest.mark.asyncio
async def test_dashscope_multimodal_dimension_is_forwarded_and_validated(monkeypatch) -> None:
    calls: list[dict[str, Any]] = []

    class FakeMultiModalEmbedding:
        return_wrong_dimension = False

        @classmethod
        def call(cls, **kwargs: Any) -> SimpleNamespace:
            calls.append(kwargs)
            dimension = 1024 if cls.return_wrong_dimension else kwargs["dimension"]
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0] * dimension}]},
            )

    monkeypatch.setitem(
        sys.modules,
        "dashscope",
        SimpleNamespace(MultiModalEmbedding=FakeMultiModalEmbedding),
    )
    embedder = DashScopeMultimodalEmbedding(
        model="multimodal-embedding-v1",
        api_key="credential-a",
        dimension=512,
        base_url="HTTPS://WORKSPACE.AP-SOUTHEAST-1.MAAS.ALIYUNCS.COM/api/v1/",
    )

    vectors = await embedder.embed_texts(["same query"], text_type="query")

    assert len(vectors[0]) == 512
    assert calls[0]["dimension"] == 512
    assert calls[0]["base_address"] == ("https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1")

    FakeMultiModalEmbedding.return_wrong_dimension = True
    with pytest.raises(EmbeddingError, match="dimension mismatch"):
        await embedder.embed_texts(["same query"], text_type="query")


def test_embedder_cache_key_hashes_credentials_and_profiles_configuration() -> None:
    secret = "synthetic-cache-key-alpha"
    base = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key=secret,
        base_url="HTTPS://WORKSPACE.AP-SOUTHEAST-1.MAAS.ALIYUNCS.COM/api/v1/",
        extra={"instruct": "retrieve relevant passages", "output_type": "dense"},
    )
    equivalent = EmbeddingConfig(
        provider="DASHSCOPE",
        model="text-embedding-v4",
        api_key=secret,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        extra={"output_type": "dense", "instruct": "retrieve relevant passages"},
    )
    different_credential = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key="synthetic-cache-key-beta",
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        extra={"instruct": "retrieve relevant passages", "output_type": "dense"},
    )
    different_profile = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key=secret,
        base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
        extra={"instruct": "retrieve relevant passages", "output_type": "dense&sparse"},
    )
    different_endpoint = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key=secret,
        base_url="https://workspace.cn-beijing.maas.aliyuncs.com/api/v1",
        extra={"instruct": "retrieve relevant passages", "output_type": "dense"},
    )

    base_key = _make_cache_key(base, 1024)

    assert base_key == _make_cache_key(equivalent, 1024)
    assert base_key != _make_cache_key(base, 512)
    assert base_key != _make_cache_key(different_credential, 1024)
    assert base_key != _make_cache_key(different_profile, 1024)
    assert base_key != _make_cache_key(different_endpoint, 1024)
    assert secret not in base_key
    assert secret[:8] not in base_key

    for changed_options in (
        {"instruct": "retrieve a different task", "output_type": "dense"},
        {"instruct": "retrieve relevant passages", "output_type": "sparse"},
        {
            "instruct": "retrieve relevant passages",
            "output_type": "dense",
            "normalization": "l2",
        },
        {
            "instruct": "retrieve relevant passages",
            "output_type": "dense",
            "version": "v2",
        },
    ):
        changed = EmbeddingConfig(
            provider="dashscope",
            model="text-embedding-v4",
            api_key=secret,
            base_url="https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1",
            extra=changed_options,
        )
        assert base_key != _make_cache_key(changed, 1024)

    basic_auth_a = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key=secret,
        base_url="https://account:password-a@workspace.example.com/api/v1",
    )
    basic_auth_b = EmbeddingConfig(
        provider="dashscope",
        model="text-embedding-v4",
        api_key=secret,
        base_url="https://account:password-b@workspace.example.com/api/v1",
    )
    assert _make_cache_key(basic_auth_a, 1024) != _make_cache_key(basic_auth_b, 1024)

    unified_one = EmbeddingConfig(
        provider="unified_multimodal",
        model="tongyi-embedding-vision-plus",
        api_key=secret,
        extra={"max_concurrent": 1},
    )
    unified_nine = EmbeddingConfig(
        provider="unified_multimodal",
        model="tongyi-embedding-vision-plus",
        api_key=secret,
        extra={"max_concurrent": 9},
    )
    assert _make_cache_key(unified_one, 1024) != _make_cache_key(unified_nine, 1024)
