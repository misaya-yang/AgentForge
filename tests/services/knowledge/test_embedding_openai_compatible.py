"""Unit tests for the openai_compatible embedding provider (T3 wire-config).

PRD T3 item 3 / task-7 wording: "default model upgrade path (Qwen3-Embedding
via vLLM/TEI — deployment blocked until infra decision; wire config)". The
wiring under test is the full config chain a deployment flips once the infra
decision lands:

* ``create_embedding`` accepts provider ``openai_compatible`` (aliases
  ``vllm`` / ``tei``) and fails closed without an explicit base URL — a
  half-configured upgrade must never silently embed against SiliconFlow;
* the adapter speaks the OpenAI wire protocol (POST ``{model, input}`` →
  ``{data: [{embedding}]}``) against the operator-supplied endpoint, with an
  optional API key (self-hosted serving is usually unauthenticated);
* ``EmbeddingManager.resolve_embedding_config`` takes the endpoint from the
  server-owned env settings (never from the dataset payload) and appends
  ``/embeddings`` exactly like the siliconflow path;
* a dataset row with ``embedding_provider = 'vllm'`` resolves end-to-end
  through ``get_text_embedder`` without any dataset-carried credentials.
"""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import embedding_manager as embedding_module
from knowledge_service.services.knowledge.embedding import (
    EmbeddingConfig,
    EmbeddingError,
    OpenAICompatibleEmbedding,
    create_embedding,
)
from knowledge_service.services.knowledge.embedding_manager import EmbeddingManager

VLLM_MODEL_DEFAULT = "Qwen/Qwen3-Embedding-0.6B"


def _embedding_settings(base_url: str) -> SimpleNamespace:
    return SimpleNamespace(
        embeddings=SimpleNamespace(
            openai_compatible_api_key="",
            openai_compatible_base_url=base_url,
        )
    )


def _manager_settings() -> SimpleNamespace:
    return SimpleNamespace(
        knowledge=SimpleNamespace(
            dashscope=SimpleNamespace(api_key=""),
            gemini=SimpleNamespace(api_key=""),
            siliconflow=SimpleNamespace(api_key="", base_url=""),
            text_embedding_dimension=1024,
        )
    )


# ---------------------------------------------------------------------------
# Factory + identity
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("provider", ["openai_compatible", "openai-compatible", "vllm", "tei"])
def test_factory_accepts_provider_and_aliases(provider: str) -> None:
    embedder = create_embedding(
        EmbeddingConfig(
            provider=provider,
            model="",
            base_url="http://vllm.internal:8000/v1",
            extra={},
        )
    )
    assert isinstance(embedder, OpenAICompatibleEmbedding)
    # Identity is canonical regardless of which alias the dataset used: the
    # binding/provenance rows and cache profiles must all agree.
    assert embedder.provider == "openai_compatible"
    assert embedder.model == VLLM_MODEL_DEFAULT
    assert embedder.dimension == 1024
    assert embedder.base_url == "http://vllm.internal:8000/v1/embeddings"


def test_factory_requires_explicit_base_url() -> None:
    with pytest.raises(EmbeddingError, match="requires an explicit base_url"):
        create_embedding(EmbeddingConfig(provider="vllm", model="m", base_url=None, extra={}))
    with pytest.raises(EmbeddingError, match="cloud default"):
        create_embedding(EmbeddingConfig(provider="vllm", model="m", base_url="   ", extra={}))


def test_known_qwen3_dimensions() -> None:
    embedder = OpenAICompatibleEmbedding(
        base_url="http://tei:80/v1",
        model="Qwen/Qwen3-Embedding-8B",
    )
    assert embedder.dimension == 4096


@pytest.mark.parametrize(
    "base_url",
    [
        "http://tei:80/v1/embeddings",
        "http://tei:80/v1/embeddings/",
    ],
)
def test_factory_appends_embeddings_path_exactly_once(base_url: str) -> None:
    embedder = OpenAICompatibleEmbedding(base_url=base_url)
    assert embedder.base_url == "http://tei/v1/embeddings"


def test_removed_openai_message_points_to_openai_compatible() -> None:
    with pytest.raises(EmbeddingError, match="openai_compatible"):
        create_embedding(EmbeddingConfig(provider="openai", model="text-embedding-3-small", extra={}))


# ---------------------------------------------------------------------------
# Wire protocol against a fake server
# ---------------------------------------------------------------------------
@pytest.mark.asyncio
async def test_embed_texts_posts_openai_payload_to_endpoint() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        body = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "data": [
                    {"object": "embedding", "index": i, "embedding": [0.5] * 8}
                    for i in range(len(body["input"]))
                ]
            },
        )

    embedder = OpenAICompatibleEmbedding(base_url="http://vllm:8000/v1")
    embedder._client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    try:
        vectors = await embedder.embed_texts(["alpha", "beta"], text_type="document")
    finally:
        await embedder._client.aclose()

    assert len(vectors) == 2 and all(len(v) == 8 for v in vectors)
    assert len(requests) == 1
    sent = requests[0]
    assert sent.url.path == "/v1/embeddings"
    body = json.loads(sent.content)
    assert body["model"] == VLLM_MODEL_DEFAULT
    assert body["input"] == ["alpha", "beta"]
    # Unauthenticated serving: a placeholder bearer token is still sent so
    # servers that require the header to be present don't 401.
    assert sent.headers["authorization"] == "Bearer unused"


@pytest.mark.asyncio
async def test_configured_api_key_is_sent_verbatim() -> None:
    seen: dict[str, Any] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["auth"] = request.headers.get("authorization")
        return httpx.Response(200, json={"data": [{"index": 0, "embedding": [0.1, 0.2]}]})

    embedder = OpenAICompatibleEmbedding(
        base_url="http://vllm:8000/v1",
        api_key="secret-token",
    )
    embedder._client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    try:
        await embedder.embed_texts(["x"])
    finally:
        await embedder._client.aclose()
    assert seen["auth"] == "Bearer secret-token"


# ---------------------------------------------------------------------------
# Server-owned config resolution (embedding_manager)
# ---------------------------------------------------------------------------
@pytest.mark.asyncio
@pytest.mark.parametrize("provider", ["openai_compatible", "vllm", "tei"])
async def test_resolve_config_takes_endpoint_from_env(
    provider: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        embedding_module,
        "get_settings",
        lambda: _embedding_settings("http://tei.internal:80/v1"),
    )
    manager = EmbeddingManager(_manager_settings())

    config = await manager.resolve_embedding_config(
        provider=provider,
        model=VLLM_MODEL_DEFAULT,
        embedding_config={},
    )

    assert config.provider == provider
    # Server-owned endpoint, /embeddings suffix appended like the siliconflow
    # path; no key required.
    assert config.base_url == "http://tei.internal:80/v1/embeddings"
    assert config.api_key is None


@pytest.mark.asyncio
async def test_resolve_config_fails_closed_without_endpoint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        embedding_module,
        "get_settings",
        lambda: _embedding_settings(""),
    )
    manager = EmbeddingManager(_manager_settings())

    with pytest.raises(ValidationFailedError, match="base URL"):
        await manager.resolve_embedding_config(
            provider="vllm",
            model=VLLM_MODEL_DEFAULT,
            embedding_config={},
        )


@pytest.mark.asyncio
async def test_get_text_embedder_resolves_vllm_dataset_end_to_end(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The full flip: a dataset row naming provider 'vllm' + env endpoint."""
    monkeypatch.setattr(
        embedding_module,
        "get_settings",
        lambda: _embedding_settings("http://vllm:8000/v1"),
    )
    manager = EmbeddingManager(_manager_settings())

    embedder = await manager.get_text_embedder(
        {
            "embedding_provider": "vllm",
            "embedding_model": "",
            "embedding_dimension": 0,
            "tenant_id": "tenant-a",
            "embedding_config": {},
        }
    )

    assert isinstance(embedder, OpenAICompatibleEmbedding)
    assert embedder.provider == "openai_compatible"
    assert embedder.model == VLLM_MODEL_DEFAULT
    assert embedder.base_url == "http://vllm:8000/v1/embeddings"
