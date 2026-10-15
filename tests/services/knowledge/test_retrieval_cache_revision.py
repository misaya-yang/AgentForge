from __future__ import annotations

import copy
from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import make_dataset_index_deletion_fence
from knowledge_service.persistence.datasets import IndexLeaseUnavailableError
from knowledge_service.services.knowledge.cache_manager import CacheManager
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.vector_store import CollectionReadAuthorityError


def _dataset() -> dict:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "content_revision": 7,
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v3",
        "embedding_dimension": 1024,
        "needs_reindex": False,
        "collection_name": "kb_dataset-a_1024",
        "index_config": {
            "retrieval": {
                "lexical": {
                    "active_version": "lexical_v1",
                    "bm25_v2": {
                        "shadow_write_enabled": True,
                        "k": 1.2,
                        "b": 0.75,
                        "avg_len": 256,
                        "tokenizer": "multilingual",
                        "language": "none",
                    },
                }
            }
        },
    }


def _service(dataset: dict):
    service = object.__new__(RetrievalService)
    cache: dict[str, tuple[list, dict]] = {}
    recall_count = 0

    async def require_dataset_access(*_args, **_kwargs):
        return copy.deepcopy(dataset)

    async def get_cached(key):
        return copy.deepcopy(cache.get(key))

    async def set_cached(key, results, meta):
        cache[key] = (copy.deepcopy(results), copy.deepcopy(meta))

    async def retrieve_queries(**_kwargs):
        nonlocal recall_count
        recall_count += 1
        return [], {"recall_count": recall_count}

    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _compute_retrieval_query_fingerprint=CacheManager.compute_fingerprint,
        _get_cached_retrieval=get_cached,
        _set_cached_retrieval=set_cached,
    )
    service.vector_store = SimpleNamespace(
        require_collection_readable=_allow_collection_read,
    )
    service._retrieve_queries = retrieve_queries
    return service, lambda: recall_count, cache


async def _allow_collection_read(*_args, **_kwargs) -> dict[str, str]:
    return {"tenant_id": "tenant-a", "dataset_id": "dataset-a"}


@pytest.mark.asyncio
@pytest.mark.parametrize("change", ["content_revision", "lexical_profile"])
async def test_text_only_v2_cache_misses_after_dataset_revision_change(change: str) -> None:
    dataset = _dataset()
    service, recall_count, cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")

    _, first_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cache this",
        include_images=False,
        vlm_rerank=False,
    )
    _, cached_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cache this",
        include_images=False,
        vlm_rerank=False,
    )

    assert recall_count() == 1
    assert cached_meta["retrieval_cache_hit"] is True
    first_fingerprint = first_meta["dataset_revision_fingerprint"]

    if change == "content_revision":
        dataset["content_revision"] += 1
    else:
        dataset["index_config"]["retrieval"]["lexical"]["bm25_v2"]["avg_len"] = 384

    _, changed_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cache this",
        include_images=False,
        vlm_rerank=False,
    )

    assert recall_count() == 2
    assert changed_meta["retrieval_cache_hit"] is False
    assert changed_meta["dataset_revision_fingerprint"] != first_fingerprint
    assert len(cache) == 2


@pytest.mark.asyncio
async def test_text_only_v2_cache_is_disabled_without_authoritative_revision() -> None:
    dataset = _dataset()
    dataset.pop("content_revision")
    service, recall_count, cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")

    _, first_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="uncacheable",
        include_images=False,
        vlm_rerank=False,
    )
    _, second_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="uncacheable",
        include_images=False,
        vlm_rerank=False,
    )

    assert recall_count() == 2
    assert cache == {}
    assert first_meta["dataset_revision_fingerprint"] is None
    assert second_meta["retrieval_cache_hit"] is False


@pytest.mark.asyncio
async def test_cached_retrieval_rejects_failure_marker_before_return() -> None:
    dataset = _dataset()
    service, _recall_count, _cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")
    await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cached during delete",
        include_images=False,
        vlm_rerank=False,
    )
    original_get = service._ks._get_cached_retrieval

    async def get_cached_and_publish_failure_marker(key: str):
        cached = await original_get(key)
        dataset["content_revision"] += 1
        dataset["index_config"]["retrieval"]["_index_deletion_fence"] = (
            make_dataset_index_deletion_fence("document_delete", "document-a")
        )
        return cached

    service._ks._get_cached_retrieval = get_cached_and_publish_failure_marker

    with pytest.raises(ValidationFailedError, match="deletion is pending"):
        await service.retrieve_with_images_v2(
            user=user,
            dataset_id="dataset-a",
            query="cached during delete",
            include_images=False,
            vlm_rerank=False,
        )


@pytest.mark.asyncio
async def test_retrieval_discards_complete_set_and_clear_generation_overlap() -> None:
    dataset = _dataset()
    service, _recall_count, _cache = _service(dataset)

    async def retrieve_across_complete_delete(**_kwargs):
        # A marker set and clear each advances content_revision. Even though no
        # marker remains at the end, the in-flight result belongs to an older
        # generation and must not escape.
        dataset["content_revision"] += 2
        return [], {"recall_count": 1}

    service._retrieve_queries = retrieve_across_complete_delete

    with pytest.raises(IndexLeaseUnavailableError, match="publication is still in progress"):
        await service.retrieve_with_images_v2(
            user=SimpleNamespace(user_id="user-a"),
            dataset_id="dataset-a",
            query="overlap delete and clear",
            include_images=False,
            vlm_rerank=False,
        )


@pytest.mark.asyncio
async def test_text_only_v2_cache_hit_rechecks_collection_authority_before_cache_read() -> None:
    dataset = _dataset()
    service, _recall_count, _cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")
    await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cached authority",
        include_images=False,
        vlm_rerank=False,
    )

    async def reject_collection(*_args, **_kwargs):
        raise CollectionReadAuthorityError("secondary collection became active")

    async def cache_must_not_be_read(*_args, **_kwargs):
        pytest.fail("cache lookup must follow collection authority preflight")

    service.vector_store = SimpleNamespace(
        require_collection_readable=reject_collection,
    )
    service._ks._get_cached_retrieval = cache_must_not_be_read

    with pytest.raises(ValidationFailedError, match="secondary collection became active"):
        await service.retrieve_with_images_v2(
            user=user,
            dataset_id="dataset-a",
            query="cached authority",
            include_images=False,
            vlm_rerank=False,
        )


@pytest.mark.asyncio
async def test_multimodal_cache_does_not_return_an_inactive_cached_segment() -> None:
    dataset = _dataset()
    service, _recall_count, _cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")
    recall_calls = 0
    authority_calls = []

    async def retrieve_queries(**_kwargs):
        nonlocal recall_calls
        recall_calls += 1
        if recall_calls == 1:
            return [
                SimpleNamespace(
                    segment_id="segment-stale",
                    document_id="document-a",
                    score=0.9,
                    text="stale cached text",
                    metadata={},
                    content_type="text",
                    image_url=None,
                    vlm_description=None,
                    associated_images=(),
                )
            ], {}
        return [], {}

    class Database:
        async def filter_active_segment_ids(
            self,
            *,
            dataset_id,
            tenant_id,
            segment_ids,
        ):
            authority_calls.append((dataset_id, tenant_id, list(segment_ids)))
            return set()

    service._retrieve_queries = retrieve_queries
    service.db = Database()

    first_results, _ = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cached stale candidate",
        include_images=False,
        vlm_rerank=False,
    )
    second_results, second_meta = await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cached stale candidate",
        include_images=False,
        vlm_rerank=False,
    )

    assert [result.segment_id for result in first_results] == ["segment-stale"]
    assert second_results == []
    assert recall_calls == 2
    assert authority_calls == [
        ("dataset-a", "tenant-a", ["segment-stale"])
    ]
    assert second_meta["retrieval_cache_hit"] is False


@pytest.mark.asyncio
async def test_multimodal_cache_active_authority_error_fails_closed() -> None:
    dataset = _dataset()
    service, _recall_count, _cache = _service(dataset)
    user = SimpleNamespace(user_id="user-a")

    async def retrieve_queries(**_kwargs):
        return [
            SimpleNamespace(
                segment_id="segment-a",
                document_id="document-a",
                score=0.9,
                text="cached text",
                metadata={},
                content_type="text",
                image_url=None,
                vlm_description=None,
                associated_images=(),
            )
        ], {}

    service._retrieve_queries = retrieve_queries
    await service.retrieve_with_images_v2(
        user=user,
        dataset_id="dataset-a",
        query="cached database outage",
        include_images=False,
        vlm_rerank=False,
    )

    class FailingDatabase:
        async def filter_active_segment_ids(self, **_kwargs):
            raise RuntimeError("postgres unavailable")

    service.db = FailingDatabase()
    with pytest.raises(ValidationFailedError, match="authority failed: postgres unavailable"):
        await service.retrieve_with_images_v2(
            user=user,
            dataset_id="dataset-a",
            query="cached database outage",
            include_images=False,
            vlm_rerank=False,
        )
