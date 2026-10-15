from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval_service import RetrievalService


@pytest.mark.asyncio
@pytest.mark.parametrize("entrypoint", ["v1", "v2"])
async def test_multimodal_retrieval_entrypoints_fail_before_association_read(entrypoint):
    association_calls = []
    dataset_access_calls = []
    retrieval_calls = []

    class Database:
        async def get_segment_associations_batch(
            self,
            segment_ids,
            *,
            dataset_id,
            tenant_id,
        ):
            association_calls.append((list(segment_ids), dataset_id, tenant_id))
            return {"segment-a": []}

    async def require_dataset_access(*_args, **_kwargs):
        dataset_access_calls.append(True)
        raise AssertionError("multimodal release gate must run before dataset access")

    async def retrieve_queries(**_kwargs):
        retrieval_calls.append(True)
        raise AssertionError("multimodal release gate must run before retrieval")

    service = object.__new__(RetrievalService)
    service.db = Database()
    service.vector_store = SimpleNamespace(
        require_collection_readable=_allow_collection_read,
    )
    service._retrieve_queries = retrieve_queries
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _compute_retrieval_query_fingerprint=lambda _payload: "query-fingerprint",
        _normalize_local_image_url=lambda value, _segment_id: value,
        vlm_service=None,
        image_storage_service=None,
    )

    with pytest.raises(
        ValidationFailedError,
        match="multimodal retrieval is unavailable in this release",
    ):
        if entrypoint == "v1":
            await service.retrieve_with_images(
                user=SimpleNamespace(user_id="user-a"),
                dataset_id="dataset-a",
                query="question",
                include_images=True,
                multimodal_rerank=False,
            )
        else:
            await service.retrieve_with_images_v2(
                user=SimpleNamespace(user_id="user-a"),
                dataset_id="dataset-a",
                query="question",
                include_images=True,
                vlm_rerank=False,
            )

    assert association_calls == []
    assert dataset_access_calls == []
    assert retrieval_calls == []


async def _allow_collection_read(*_args, **_kwargs):
    return {"dataset_id": "dataset-a", "tenant_id": "tenant-a"}


@pytest.mark.asyncio
async def test_association_writer_binds_document_dataset_and_tenant():
    calls = []

    class Database:
        async def get_document(self, document_id):
            assert document_id == "document-a"
            return {"document_id": document_id, "dataset_id": "dataset-a"}

        async def get_dataset(self, dataset_id):
            assert dataset_id == "dataset-a"
            return {"dataset_id": dataset_id, "tenant_id": "tenant-a"}

        async def list_segments(self, **kwargs):
            assert kwargs["dataset_id"] == "dataset-a"
            assert kwargs["document_id"] == "document-a"
            return [
                {
                    "segment_id": "text-a",
                    "content_type": "text",
                    "text": "nearby text",
                    "position": 0,
                    "metadata": {},
                },
                {
                    "segment_id": "image-a",
                    "content_type": "image",
                    "position": 1,
                    "metadata": {},
                },
            ]

        async def add_segment_image_associations_batch(
            self,
            associations,
            *,
            dataset_id,
            tenant_id,
        ):
            calls.append((associations, dataset_id, tenant_id))
            return len(associations)

        async def update_segment_image_flags(self, segment_id):
            assert segment_id == "text-a"

    service = object.__new__(KnowledgeService)
    service.db = Database()

    result = await service.associate_images_to_chunks(
        "document-a",
        proximity_threshold=0.0,
    )

    assert result["associations_created"] == 1
    assert len(calls) == 1
    assert calls[0][1:] == ("dataset-a", "tenant-a")
    assert calls[0][0][0]["segment_id"] == "text-a"
    assert calls[0][0][0]["image_segment_id"] == "image-a"


@pytest.mark.asyncio
async def test_association_reader_uses_authorized_dataset_tenant():
    calls = []

    class Database:
        async def list_segments(self, **kwargs):
            assert kwargs["dataset_id"] == "dataset-a"
            return [
                {
                    "segment_id": "text-a",
                    "content_type": "text",
                    "has_images": True,
                }
            ]

        async def get_segment_associations_batch(
            self,
            segment_ids,
            *,
            dataset_id,
            tenant_id,
        ):
            calls.append((list(segment_ids), dataset_id, tenant_id))
            return {"text-a": []}

    async def require_dataset_access(*_args, **_kwargs):
        return {"dataset_id": "dataset-a", "tenant_id": "tenant-a"}

    service = object.__new__(KnowledgeService)
    service.db = Database()
    service.require_dataset_access = require_dataset_access

    results = await service.get_segments_with_images(
        user=SimpleNamespace(user_id="user-a"),
        dataset_id="dataset-a",
    )

    assert results[0]["associated_images"] == []
    assert calls == [(["text-a"], "dataset-a", "tenant-a")]
