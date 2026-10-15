import pytest
from knowledge_service.services.knowledge.hierarchical_retriever import (
    HierarchicalAuthorityError,
    HierarchicalRetriever,
    RetrievalStrategy,
)
from knowledge_service.services.knowledge.vector_store import VectorSearchHit


class StubEmbedder:
    async def embed_documents(self, texts):
        return [[0.1, 0.2, 0.3] for _ in texts]


class StubVectorStore:
    def __init__(self):
        self.calls = []

    async def search(
        self,
        collection_name,
        query_vector,
        top_k=5,
        query_filter=None,
        score_threshold=None,
        **kwargs,
    ):
        del query_vector, score_threshold
        self.calls.append(
            {
                "collection": collection_name,
                "query_filter": query_filter,
                "top_k": top_k,
                "tenant_id": kwargs.get("tenant_id"),
                "dataset_id": kwargs.get("dataset_id"),
            }
        )
        if collection_name.endswith("_summary"):
            return [
                VectorSearchHit(
                    point_id="sum1",
                    score=0.9,
                    payload={
                        "document_id": "doc1",
                        "summary": "doc summary",
                        "text": "doc summary",
                        "level": 1,
                    },
                )
            ]
        if collection_name.endswith("_sections"):
            return [
                VectorSearchHit(
                    point_id="sec1",
                    score=0.8,
                    payload={
                        "document_id": "doc1",
                        "segment_id": "sec1",
                        "text": "section text",
                        "level": 2,
                    },
                )
            ]
        return [
            VectorSearchHit(
                point_id="seg1",
                score=0.7,
                payload={
                    "document_id": "doc1",
                    "segment_id": "seg1",
                    "text": "para",
                    "level": 3,
                    "parent_segment_id": "sec1",
                },
            )
        ]


class StubDb:
    async def get_dataset(self, dataset_id):
        return {"dataset_id": dataset_id, "tenant_id": "tenant-a"}

    async def filter_active_document_ids(
        self,
        *,
        dataset_id,
        tenant_id,
        document_ids,
    ):
        assert (dataset_id, tenant_id) == ("ds", "tenant-a")
        return set(document_ids)

    async def filter_active_segment_ids(
        self,
        *,
        dataset_id,
        tenant_id,
        segment_ids,
    ):
        assert (dataset_id, tenant_id) == ("ds", "tenant-a")
        return set(segment_ids)

    async def filter_active_summary_vector_ids(
        self, dataset_id, tenant_id, vector_ids,
    ):
        assert (dataset_id, tenant_id) == ("ds", "tenant-a")
        return set(vector_ids)

    async def get_document_summary_scoped(
        self,
        *,
        document_id,
        dataset_id,
        tenant_id,
    ):
        assert (document_id, dataset_id, tenant_id) == ("doc1", "ds", "tenant-a")
        return {"summary": "doc summary"}

    async def get_segment_scoped(
        self,
        *,
        segment_id,
        dataset_id,
        tenant_id,
    ):
        assert (dataset_id, tenant_id) == ("ds", "tenant-a")
        if segment_id == "sec1":
            return {"text": "section text"}
        return None


@pytest.mark.asyncio
async def test_retriever_uses_wrapper_and_enriches_context():
    retriever = HierarchicalRetriever(StubVectorStore(), StubEmbedder(), StubDb())

    results, meta = await retriever.retrieve(
        query="test",
        dataset_id="ds",
        top_k=1,
        include_context=True,
        base_collection="kb_ds_3",
    )

    assert results[0].segment_id == "seg1"
    assert results[0].document_summary == "doc summary"
    assert results[0].parent_context == "section text"

    l3_call = [c for c in retriever.vector_store.calls if c["collection"] == "kb_ds_3"][0]
    assert l3_call["query_filter"] is not None
    assert l3_call["tenant_id"] == "tenant-a"
    assert l3_call["dataset_id"] == "ds"


@pytest.mark.asyncio
async def test_unpublished_l1_candidate_is_not_used_as_document_filter():
    class CurrentSummaryDb(StubDb):
        async def filter_active_summary_vector_ids(
            self, dataset_id, tenant_id, vector_ids,
        ):
            assert (dataset_id, tenant_id, vector_ids) == ("ds", "tenant-a", ["sum1"])
            return set()

    retriever = HierarchicalRetriever(StubVectorStore(), StubEmbedder(), CurrentSummaryDb())
    filtered = await retriever._filter_active_layer(
        [{"document_id": "doc1", "segment_id": "sum1"}],
        dataset_id="ds", tenant_id="tenant-a",
        require_segments=False, require_summary=True,
    )
    assert filtered == []


@pytest.mark.asyncio
async def test_parallel_retrieval_hides_unpublished_l1_candidate():
    class CurrentSummaryDb(LifecycleDb):
        async def filter_active_summary_vector_ids(
            self, _dataset_id, _tenant_id, _vector_ids,
        ):
            return set()

    store = LayeredVectorStore(
        summary=[{"id": "summary-candidate", "document_id": "doc-live", "text": "new summary", "level": 1}],
        paragraphs=[{"id": "segment-live", "segment_id": "segment-live", "document_id": "doc-live", "text": "old paragraph", "level": 3}],
    )
    db = CurrentSummaryDb(active_documents={"doc-live"}, active_segments={"segment-live"})
    retriever = HierarchicalRetriever(store, StubEmbedder(), db)

    results, _metadata = await retriever.retrieve(
        query="test", dataset_id="ds", tenant_id="tenant-a",
        base_collection="kb_ds_3", strategy=RetrievalStrategy.PARALLEL,
        include_context=False, top_k=10,
    )
    assert {result.segment_id for result in results} == {"segment-live"}


class LayeredVectorStore:
    def __init__(self, *, summary=None, sections=None, paragraphs=None):
        self.layers = {
            "summary": summary or [],
            "sections": sections or [],
            "paragraphs": paragraphs or [],
        }
        self.calls = []

    async def search(
        self,
        collection_name,
        query_vector,
        top_k=5,
        query_filter=None,
        score_threshold=None,
        **_kwargs,
    ):
        del query_vector, top_k, score_threshold
        self.calls.append((collection_name, query_filter))
        if collection_name.endswith("_summary"):
            layer = "summary"
        elif collection_name.endswith("_sections"):
            layer = "sections"
        else:
            layer = "paragraphs"
        return [
            VectorSearchHit(
                point_id=item["id"],
                score=item.get("score", 0.8),
                payload={key: value for key, value in item.items() if key not in {"id", "score"}},
            )
            for item in self.layers[layer]
        ]


class LifecycleDb:
    def __init__(
        self,
        *,
        active_documents,
        active_segments,
        fail_document_call=None,
        fail_all_documents=False,
    ):
        self.active_documents = set(active_documents)
        self.active_segments = set(active_segments)
        self.fail_document_call = fail_document_call
        self.fail_all_documents = fail_all_documents
        self.document_calls = 0
        self.segment_calls = []

    async def filter_active_document_ids(self, *, document_ids, **_scope):
        self.document_calls += 1
        if self.fail_all_documents or self.document_calls == self.fail_document_call:
            raise RuntimeError("postgres authority unavailable")
        return set(document_ids) & self.active_documents

    async def filter_active_segment_ids(self, *, segment_ids, **_scope):
        self.segment_calls.append(list(segment_ids))
        return set(segment_ids) & self.active_segments

    async def filter_active_summary_vector_ids(
        self, _dataset_id, _tenant_id, vector_ids,
    ):
        return set(vector_ids)


@pytest.mark.asyncio
async def test_cascade_inactive_l1_cannot_drive_document_filter_and_stale_layers_are_pruned():
    store = LayeredVectorStore(
        summary=[
            {
                "id": "summary-stale",
                "document_id": "doc-stale",
                "text": "stale summary",
                "level": 1,
            }
        ],
        sections=[
            {
                "id": "section-stale",
                "segment_id": "section-stale",
                "document_id": "doc-stale",
                "text": "stale section",
                "level": 2,
            }
        ],
        paragraphs=[
            {
                "id": "segment-live",
                "segment_id": "segment-live",
                "document_id": "doc-live",
                "text": "live paragraph",
                "level": 3,
            }
        ],
    )
    db = LifecycleDb(
        active_documents={"doc-live"},
        active_segments={"segment-live"},
    )
    retriever = HierarchicalRetriever(store, StubEmbedder(), db)

    results, metadata = await retriever.retrieve(
        query="test",
        dataset_id="ds",
        tenant_id="tenant-a",
        base_collection="kb_ds_3",
        include_context=False,
    )

    assert [result.segment_id for result in results] == ["segment-live"]
    assert metadata.l1_candidates == 0
    assert metadata.l2_candidates == 0
    section_call = next(call for call in store.calls if call[0].endswith("_sections"))
    paragraph_call = next(call for call in store.calls if call[0] == "kb_ds_3")
    assert section_call[1] is None
    assert paragraph_call[1] is None


@pytest.mark.asyncio
async def test_parallel_retains_live_l1_summary_without_segment_authority_and_prunes_stale():
    store = LayeredVectorStore(
        summary=[
            {
                "id": "summary-live",
                "document_id": "doc-live",
                "text": "live summary",
                "level": 1,
            },
            {
                "id": "summary-stale",
                "document_id": "doc-stale",
                "text": "stale summary",
                "level": 1,
            },
        ],
        sections=[
            {
                "id": "section-stale",
                "segment_id": "section-stale",
                "document_id": "doc-live",
                "text": "disabled section",
                "level": 2,
            }
        ],
        paragraphs=[
            {
                "id": "segment-live",
                "segment_id": "segment-live",
                "document_id": "doc-live",
                "text": "live paragraph",
                "level": 3,
            },
            {
                "id": "segment-stale",
                "segment_id": "segment-stale",
                "document_id": "doc-live",
                "text": "disabled paragraph",
                "level": 3,
            },
        ],
    )
    db = LifecycleDb(
        active_documents={"doc-live"},
        active_segments={"segment-live"},
    )
    retriever = HierarchicalRetriever(store, StubEmbedder(), db)

    results, _metadata = await retriever.retrieve(
        query="test",
        dataset_id="ds",
        tenant_id="tenant-a",
        base_collection="kb_ds_3",
        strategy=RetrievalStrategy.PARALLEL,
        include_context=False,
        top_k=10,
    )

    assert {result.segment_id for result in results} == {"summary-live", "segment-live"}
    assert all("summary-live" not in call for call in db.segment_calls)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "strategy",
    [RetrievalStrategy.CASCADE, RetrievalStrategy.PARALLEL],
)
async def test_text_profile_recursively_filters_image_like_hierarchical_results(
    strategy: RetrievalStrategy,
):
    blocked_ids = {
        "summary-image",
        "section-page-image",
        "paragraph-image",
        "paragraph-mixed",
        "paragraph-multimodal",
    }
    store = LayeredVectorStore(
        summary=[
            {
                "id": "summary-text",
                "document_id": "doc-live",
                "text": "text summary",
                "level": 1,
                "content_type": "text",
            },
            {
                "id": "summary-image",
                "document_id": "doc-image",
                "text": "image summary",
                "level": 1,
                "content_type": "image",
            },
        ],
        sections=[
            {
                "id": "section-text",
                "segment_id": "section-text",
                "document_id": "doc-live",
                "text": "text section",
                "level": 2,
                "metadata": {"content_type": "text"},
            },
            {
                "id": "section-page-image",
                "segment_id": "section-page-image",
                "document_id": "doc-live",
                "text": "page image section",
                "level": 2,
                "metadata": {"content_type": "page_image"},
            },
        ],
        paragraphs=[
            {
                "id": "paragraph-image",
                "segment_id": "paragraph-image",
                "document_id": "doc-live",
                "text": "image paragraph",
                "level": 3,
                "content_type": "image",
            },
            {
                "id": "paragraph-mixed",
                "segment_id": "paragraph-mixed",
                "document_id": "doc-live",
                "text": "mixed paragraph",
                "level": 3,
                "metadata": {"metadata": {"content_type": "mixed"}},
            },
            {
                "id": "paragraph-multimodal",
                "segment_id": "paragraph-multimodal",
                "document_id": "doc-live",
                "text": "multimodal paragraph",
                "level": 3,
                "metadata": [{"content_type": "multimodal"}],
            },
            {
                "id": "paragraph-text",
                "segment_id": "paragraph-text",
                "document_id": "doc-live",
                "text": "text paragraph",
                "level": 3,
                "metadata": {"content_type": "text"},
            },
        ],
    )
    db = LifecycleDb(
        active_documents={"doc-live", "doc-image"},
        active_segments={
            "section-text",
            "section-page-image",
            "paragraph-image",
            "paragraph-mixed",
            "paragraph-multimodal",
            "paragraph-text",
        },
    )

    results, _metadata = await HierarchicalRetriever(
        store,
        StubEmbedder(),
        db,
    ).retrieve(
        query="test",
        dataset_id="ds",
        tenant_id="tenant-a",
        base_collection="kb_ds_3",
        strategy=strategy,
        include_context=False,
        top_k=20,
    )

    returned_ids = {result.segment_id for result in results}
    assert "paragraph-text" in returned_ids
    assert returned_ids.isdisjoint(blocked_ids)
    assert all(
        blocked_ids.isdisjoint(set(segment_call))
        for segment_call in db.segment_calls
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("fail_document_call", [1, 2])
async def test_cascade_document_authority_outage_is_not_swallowed(fail_document_call):
    store = LayeredVectorStore(
        summary=[{"id": "summary", "document_id": "doc", "text": "s", "level": 1}],
        sections=[
            {
                "id": "section",
                "segment_id": "section",
                "document_id": "doc",
                "text": "s",
                "level": 2,
            }
        ],
        paragraphs=[
            {
                "id": "paragraph",
                "segment_id": "paragraph",
                "document_id": "doc",
                "text": "p",
                "level": 3,
            }
        ],
    )
    db = LifecycleDb(
        active_documents={"doc"},
        active_segments={"section", "paragraph"},
        fail_document_call=fail_document_call,
    )

    with pytest.raises(HierarchicalAuthorityError, match="document authority failed"):
        await HierarchicalRetriever(store, StubEmbedder(), db).retrieve(
            query="test",
            dataset_id="ds",
            tenant_id="tenant-a",
            base_collection="kb_ds_3",
            include_context=False,
        )


@pytest.mark.asyncio
async def test_parallel_authority_outage_fails_the_whole_request():
    item = {"id": "summary", "document_id": "doc", "text": "s", "level": 1}
    store = LayeredVectorStore(summary=[item], sections=[item], paragraphs=[item])
    db = LifecycleDb(
        active_documents={"doc"},
        active_segments={"summary"},
        fail_all_documents=True,
    )

    with pytest.raises(HierarchicalAuthorityError, match="document authority failed"):
        await HierarchicalRetriever(store, StubEmbedder(), db).retrieve(
            query="test",
            dataset_id="ds",
            tenant_id="tenant-a",
            base_collection="kb_ds_3",
            strategy=RetrievalStrategy.PARALLEL,
            include_context=False,
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("require_segments", [False, True])
async def test_shared_layer_authority_filters_nested_unreleased_image_payloads(
    require_segments,
):
    retriever = HierarchicalRetriever(StubVectorStore(), StubEmbedder(), StubDb())
    results = [
        {
            "id": "text-live",
            "segment_id": "text-live",
            "document_id": "doc1",
            "metadata": {"content_type": "text"},
        },
        {
            "id": "image-stale",
            "segment_id": "image-stale",
            "document_id": "doc1",
            "metadata": {"nested": [{"content_type": "page_image"}]},
        },
        {
            "id": "mixed-stale",
            "segment_id": "mixed-stale",
            "document_id": "doc1",
            "metadata": {"deep": {"modality": "mixed"}},
        },
    ]

    filtered = await retriever._filter_active_layer(
        results,
        dataset_id="ds",
        tenant_id="tenant-a",
        require_segments=require_segments,
    )

    assert [item["id"] for item in filtered] == ["text-live"]
