import sys
import types

import pytest
from knowledge_service.services.knowledge.embedding import DashScopeEmbedding, EmbeddingError


@pytest.mark.asyncio
async def test_dashscope_embedding_falls_back_to_singleton_on_timeout(monkeypatch):
    class DummyTextEmbedding:
        @staticmethod
        def call(**_kwargs):
            return None

    dummy = types.SimpleNamespace(TextEmbedding=DummyTextEmbedding, base_http_api_url="")
    monkeypatch.setitem(sys.modules, "dashscope", dummy)

    emb = DashScopeEmbedding(model="text-embedding-v4", api_key="k")

    calls = []

    async def fake_call_with_retry(batch, batch_info, **kwargs):
        calls.append(list(batch))
        if len(batch) > 1:
            raise EmbeddingError("timeout")
        return [[0.0] * 1024 for _ in batch]

    monkeypatch.setattr(emb, "_call_with_retry", fake_call_with_retry)

    vectors = await emb.embed_texts(["a", "b", "c"])

    assert len(vectors) == 3
    assert any(len(c) > 1 for c in calls)
    assert any(len(c) == 1 for c in calls)
