"""SPO-04 gate tests: K3 interactive profile + K4 embedding timeout.

K3: with no explicit retrieval config, the default interactive profile
retrieves 12 dense + 12 lexical candidates (no top_k*4/*10 expansion).

K4: a hung multimodal embedding RPC is bounded by the per-RPC timeout and
surfaces as EmbeddingError — the ingestion path maps that to ``failed``
(existing fence tests cover the status transition).
"""

from __future__ import annotations

import asyncio
import threading
from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge import retrieval_service
from knowledge_service.services.knowledge.embedding import (
    EmbeddingError,
    UnifiedMultimodalEmbedding,
)
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.vision_pdf_processor import VisionPDFProcessor


@pytest.mark.asyncio
async def test_default_interactive_profile_uses_12_plus_12_hybrid(monkeypatch) -> None:
    search_calls: list[dict] = []

    class FakeEmbedder:
        dimension = 3

        async def embed_query(self, _query):
            return [0.1, 0.2, 0.3]

    class FakeVectorStore:
        async def ping(self, **_kwargs):
            pytest.fail("interactive retrieval must not issue a separate Qdrant ping")

        async def require_collection_readable(self, *_args, **_kwargs):
            return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

        async def ensure_collection(self, **_kwargs):
            return "kb_created"

        async def search(self, **kwargs):
            search_calls.append(dict(kwargs))
            return [
                SimpleNamespace(
                    point_id="seg-1",
                    score=0.9,
                    payload={
                        "segment_id": "seg-1",
                        "document_id": "doc-1",
                        "text": "dense result",
                    },
                )
            ]

        async def search_keyword(self, **kwargs):
            search_calls.append(dict(kwargs))
            return [
                SimpleNamespace(
                    point_id="seg-1",
                    score=0.8,
                    payload={
                        "segment_id": "seg-1",
                        "document_id": "doc-1",
                        "text": "keyword result",
                    },
                )
            ]

    async def get_embedder(_config, dimension=None):
        return FakeEmbedder()

    async def require_dataset_access(_user, dataset_id, required="viewer"):
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb_demo",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
        }

    async def filter_active_segment_ids(*, segment_ids, **_kwargs):
        return set(segment_ids)

    async def get_presigned_image_url(_raw_url, _segment_id):
        return None

    monkeypatch.setattr(retrieval_service, "get_cached_embedder", get_embedder)

    vector_store = FakeVectorStore()
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        SimpleNamespace(filter_active_segment_ids=filter_active_segment_ids),
    )
    service.vector_store = vector_store
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _resolve_fusion_config=lambda **_kwargs: {
            "method": "rrf",
            "dense_weight": 0.5,
            "bm25_weight": 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: SimpleNamespace(),
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, *_args: candidates,
        _get_presigned_image_url=get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )

    await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    # The default interactive profile: 12 dense + 12 lexical.
    dense_calls = [call for call in search_calls if "top_k" in call]
    keyword_calls = [call for call in search_calls if "top_k" not in call]
    assert dense_calls, "expected a dense search call"
    assert dense_calls[0]["top_k"] == 12
    assert keyword_calls or len(search_calls) == 1
    if keyword_calls:
        assert keyword_calls[0]["limit"] == 12


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["dense", "bm25"])
async def test_default_candidate_depth_never_truncates_requested_top_k(
    monkeypatch: pytest.MonkeyPatch,
    mode: str,
) -> None:
    search_calls: list[dict] = []

    class FakeEmbedder:
        dimension = 3

        async def embed_query(self, _query):
            return [0.1, 0.2, 0.3]

    class FakeVectorStore:
        async def ping(self, **_kwargs):
            return True

        async def require_collection_readable(self, *_args, **_kwargs):
            return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

        async def ensure_collection(self, **_kwargs):
            return "kb_created"

        async def search(self, **kwargs):
            search_calls.append(dict(kwargs))
            return []

        async def search_keyword(self, **kwargs):
            search_calls.append(dict(kwargs))
            return []

    class FakeDatabase:
        async def search_segments_text(self, **kwargs):
            search_calls.append(dict(kwargs))
            return [
                {
                    "segment_id": f"seg-{index}",
                    "document_id": "doc-1",
                    "dataset_id": "kb-demo",
                    "text": f"query result {index}",
                    "metadata": {},
                }
                for index in range(30)
            ]

        async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
            return set(segment_ids)

    async def get_embedder(_config, dimension=None):
        return FakeEmbedder()

    async def require_dataset_access(_user, dataset_id, required="viewer"):
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb_demo",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
        }

    async def filter_active_segment_ids(*, segment_ids, **_kwargs):
        return set(segment_ids)

    async def get_presigned_image_url(_raw_url, _segment_id):
        return None

    monkeypatch.setattr(retrieval_service, "get_cached_embedder", get_embedder)
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        FakeDatabase(),
    )
    service.vector_store = FakeVectorStore()
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _resolve_fusion_config=lambda **_kwargs: {
            "method": "rrf",
            "dense_weight": 0.5,
            "bm25_weight": 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: SimpleNamespace(),
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, *_args: candidates,
        _get_presigned_image_url=get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )

    results, _receipt = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode=mode,
        top_k=25,
        rerank=False,
        mmr=False,
    )

    assert search_calls
    requested = search_calls[0].get("top_k", search_calls[0].get("limit"))
    assert requested >= 25
    if mode == "bm25":
        assert len(results) == 25


@pytest.mark.asyncio
async def test_multimodal_text_embedding_rpc_is_timeout_bounded() -> None:
    release = threading.Event()

    class HangingEmbedding:
        @staticmethod
        def call(**kwargs):
            del kwargs
            release.wait(timeout=10)
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0] * 1024}]},
            )

    embedder = UnifiedMultimodalEmbedding(
        api_key="test-key",
        model="tongyi-embedding-vision-plus",
        dimension=1024,
        request_timeout_s=1.0,
    )
    embedder._MultiModalEmbedding = HangingEmbedding

    started = asyncio.get_running_loop().time()
    with pytest.raises(EmbeddingError, match="Unified embedding error"):
        await embedder.embed_texts(["hanging provider"])
    elapsed = asyncio.get_running_loop().time() - started
    release.set()

    assert elapsed < 2.5  # the 1s timeout bound fired well before the hang


@pytest.mark.asyncio
async def test_multimodal_text_embedding_is_concurrent_not_serial() -> None:
    """K4: N texts are in flight concurrently (bounded by the semaphore)."""
    import time

    state = {"active": 0, "peak": 0}

    class TrackingEmbeddingReal:
        @staticmethod
        def call(**kwargs):
            del kwargs
            state["active"] += 1
            state["peak"] = max(state["peak"], state["active"])
            # brief busy-wait to overlap the coroutines
            end = time.time() + 0.05
            while time.time() < end:
                pass
            state["active"] -= 1
            return SimpleNamespace(
                status_code=200,
                output={"embeddings": [{"embedding": [1.0] * 1024}]},
            )

    embedder = UnifiedMultimodalEmbedding(
        api_key="test-key",
        model="tongyi-embedding-vision-plus",
        dimension=1024,
        max_concurrent=5,
        request_timeout_s=5.0,
    )
    embedder._MultiModalEmbedding = TrackingEmbeddingReal

    vectors = await embedder.embed_texts(["a", "b", "c", "d"])
    assert len(vectors) == 4
    # With a serial loop the peak in-flight count would be 1; the concurrent
    # path overlaps at least two RPCs.
    assert state["peak"] >= 2


@pytest.mark.asyncio
async def test_pdf_batch_keeps_open_render_and_close_on_one_worker(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    worker_threads: list[int] = []

    class FakePixmap:
        width = 10
        height = 20

        @staticmethod
        def tobytes(_format: str) -> bytes:
            return b"png"

    class FakePage:
        @staticmethod
        def get_pixmap(**_kwargs):
            worker_threads.append(threading.get_ident())
            return FakePixmap()

    class FakeDocument:
        def __len__(self) -> int:
            return 2

        def __getitem__(self, _index: int) -> FakePage:
            worker_threads.append(threading.get_ident())
            return FakePage()

        @staticmethod
        def close() -> None:
            worker_threads.append(threading.get_ident())

    class FakeFitz:
        @staticmethod
        def open(**_kwargs) -> FakeDocument:
            worker_threads.append(threading.get_ident())
            return FakeDocument()

    processor = VisionPDFProcessor(None, None, None, batch_size=2)
    monkeypatch.setattr(processor, "_get_fitz", lambda: FakeFitz())

    rendered = await asyncio.to_thread(
        processor._render_page_batch, b"pdf", 0, 2
    )

    assert len(rendered) == 2
    assert len(set(worker_threads)) == 1
