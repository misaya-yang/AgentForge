"""Focused fakes shared by Knowledge retrieval batch tests."""

from types import SimpleNamespace

from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval_service import RetrievalService


class _ReadableVectorStore:
    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}


class FakeDatabase:
    def __init__(self, rows):
        self.rows = rows
        self.calls = []

    async def search_segments_text(self, **kwargs):
        self.calls.append(kwargs)
        return self.rows

    async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
        return set(segment_ids)


def make_bm25_service(rows):
    async def require_dataset_access(_user, dataset_id, required="viewer"):
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb-demo-collection",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
        }

    async def get_presigned_image_url(_raw_url, _segment_id):
        return None

    database = FakeDatabase(rows)
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        database,
    )
    service.vector_store = _ReadableVectorStore()
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _resolve_fusion_config=lambda **kwargs: {
            "method": "rrf",
            "dense_weight": kwargs.get("dense_weight")
            if kwargs.get("dense_weight") is not None
            else 0.5,
            "bm25_weight": kwargs.get("bm25_weight")
            if kwargs.get("bm25_weight") is not None
            else 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, source_type, language, metadata: (
            KnowledgeService._filter_candidates_by_metadata(
                None, candidates, source_type, language, metadata
            )
        ),
        _get_presigned_image_url=get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return service, database
