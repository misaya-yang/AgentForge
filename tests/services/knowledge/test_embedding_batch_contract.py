from __future__ import annotations

from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge.embedding import (
    DashScopeMultimodalEmbedding,
    EmbeddingError,
    UnifiedMultimodalEmbedding,
)


@pytest.mark.asyncio
async def test_dashscope_image_batch_fails_instead_of_returning_misaligned_vectors():
    embedder = object.__new__(DashScopeMultimodalEmbedding)
    embedder.model = "multimodal-embedding-v1"
    embedder.api_key = "test-key"
    embedder._dimension = None
    embedder._detect_media_type = lambda _image: "image/png"
    embedder._image_to_base64_data_uri = lambda image, _media_type: image.decode()

    class FakeMultiModalEmbedding:
        @staticmethod
        def call(*, input, **_kwargs):
            if input[0]["image"] == "bad":
                raise RuntimeError("simulated provider failure")
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0, 2.0]}]},
            )

    embedder._MultiModalEmbedding = FakeMultiModalEmbedding

    with pytest.raises(EmbeddingError, match="image 1"):
        await embedder.embed_images([b"good", b"bad"])


@pytest.mark.asyncio
async def test_unified_image_batch_fails_instead_of_returning_misaligned_vectors():
    embedder = object.__new__(UnifiedMultimodalEmbedding)
    embedder.model = "tongyi-embedding-vision-plus"
    embedder._dimension = None
    embedder._detect_media_type = lambda _image: "image/png"
    embedder._to_base64_data_uri = lambda image, _media_type: image.decode()

    async def fake_call_api(input_items):
        if input_items[0]["image"] == "bad":
            raise EmbeddingError("simulated provider failure")
        return [1.0, 2.0]

    embedder._call_api = fake_call_api

    with pytest.raises(EmbeddingError, match="simulated provider failure"):
        await embedder.embed_images([b"good", b"bad"])
