import asyncio
import hashlib
from contextlib import asynccontextmanager
from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge import vector_store
from knowledge_service.services.knowledge.lexical_config import (
    BM25_V2,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1_FIELD,
    LexicalConfig,
)
from knowledge_service.services.knowledge.vector_store import (
    CollectionReadAuthorityError,
    VectorStore,
    VectorStoreError,
)
from qdrant_client.http import models as qmodels


def _direct_filter_values(query_filter):
    return {
        condition.key: condition.match.value
        for condition in (query_filter.must or [])
        if isinstance(condition, qmodels.FieldCondition)
    }


def _assert_legacy_or_enabled_filter(query_filter):
    enabled_filters = [
        condition
        for condition in (query_filter.must or [])
        if isinstance(condition, qmodels.Filter)
        and any(
            isinstance(item, qmodels.FieldCondition) and item.key == "enabled"
            for item in (condition.should or [])
        )
    ]
    assert len(enabled_filters) == 1
    should = enabled_filters[0].should or []
    assert any(
        isinstance(item, qmodels.FieldCondition)
        and item.key == "enabled"
        and item.match.value is True
        for item in should
    )
    assert any(
        isinstance(item, qmodels.IsEmptyCondition)
        and item.is_empty.key == "enabled"
        for item in should
    )


@pytest.mark.asyncio
async def test_embedding_migration_scope_scans_exact_point_and_source_digests(
    monkeypatch,
):
    calls = []

    class DummyClient:
        async def scroll(self, **kwargs):
            calls.append(kwargs)
            offset = kwargs.get("offset")
            point_id, text = (
                ("segment-b", "second") if offset is None else ("segment-a", "first")
            )
            return (
                [
                    SimpleNamespace(
                        id=point_id,
                        payload={
                            "tenant_id": "tenant-a",
                            "dataset_id": "dataset-a",
                            "text": text,
                            "embedding_model": "model-v2",
                            "embedding_model_version": "2026-08",
                        },
                        vector=[1.0, 0.0],
                    )
                ],
                "next" if offset is None else None,
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")
    evidence = await store.scan_embedding_migration_scope(
        "shadow-a",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        embedding_model="model-v2",
        embedding_model_version="2026-08",
        embedding_dimension=2,
    )

    expected_ids = hashlib.sha256(b"segment-a\nsegment-b\n").hexdigest()
    source_lines = "".join(
        f"{point_id}\0{hashlib.sha256(text.encode()).hexdigest()}\n"
        for point_id, text in [("segment-a", "first"), ("segment-b", "second")]
    )
    assert evidence == {
        "point_count": 2,
        "point_ids_sha256": expected_ids,
        "source_text_sha256": hashlib.sha256(source_lines.encode()).hexdigest(),
    }
    assert len(calls) == 2
    assert all(call["with_vectors"] is False for call in calls)


@pytest.mark.asyncio
async def test_embedding_migration_scope_rejects_stale_point_provenance(monkeypatch):
    class DummyClient:
        async def scroll(self, **_kwargs):
            return (
                [
                    SimpleNamespace(
                        id="segment-a",
                        payload={
                            "tenant_id": "tenant-a",
                            "dataset_id": "dataset-a",
                            "text": "first",
                            "embedding_model": "old-model",
                            "embedding_model_version": "",
                        },
                        vector=[1.0, 0.0],
                    )
                ],
                None,
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")
    with pytest.raises(VectorStoreError, match="stale embedding provenance"):
        await store.scan_embedding_migration_scope(
            "shadow-a",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            embedding_model="model-v2",
            embedding_model_version="2026-08",
            embedding_dimension=2,
        )


@pytest.mark.asyncio
async def test_search_passes_query_filter_and_score_threshold(monkeypatch):
    captured = {}

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def query_points(self, **kwargs):
            captured.update(kwargs)
            return SimpleNamespace(points=[])

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    flt = qmodels.Filter(
        must=[qmodels.FieldCondition(key="document_id", match=qmodels.MatchValue(value="doc1"))]
    )

    await vs.search(
        collection_name="kb_ds_3",
        query_vector=[0.1, 0.2, 0.3],
        top_k=7,
        tenant_id="tenant-a",
        dataset_id="ds",
        query_filter=flt,
        score_threshold=0.42,
    )

    assert _direct_filter_values(captured["query_filter"]) == {
        "document_id": "doc1",
        "tenant_id": "tenant-a",
        "dataset_id": "ds",
    }
    _assert_legacy_or_enabled_filter(captured["query_filter"])
    assert captured["score_threshold"] == 0.42
    assert captured["limit"] == 7


@pytest.mark.asyncio
async def test_search_pushes_nested_metadata_filters(monkeypatch):
    captured = {}

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def query_points(self, **kwargs):
            captured.update(kwargs)
            return SimpleNamespace(points=[])

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    await vs.search(
        collection_name="kb_ds_3",
        query_vector=[0.1, 0.2, 0.3],
        tenant_id="tenant-a",
        dataset_id="ds",
        metadata_filter={"madhab": "hanafi", "authority_rank": 2},
    )

    assert _direct_filter_values(captured["query_filter"]) == {
        "tenant_id": "tenant-a",
        "dataset_id": "ds",
        "metadata.madhab": "hanafi",
        "metadata.authority_rank": 2,
    }
    _assert_legacy_or_enabled_filter(captured["query_filter"])


@pytest.mark.asyncio
async def test_search_rejects_legacy_collection_without_explicit_scope(monkeypatch):
    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    with pytest.raises(VectorStoreError, match="requires non-empty tenant_id and dataset_id"):
        await vs.search(
            collection_name="kb_ds_3",
            query_vector=[0.1, 0.2, 0.3],
        )


@pytest.mark.asyncio
async def test_search_enforces_immutable_collection_scope(monkeypatch):
    captured = {}
    metadata = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "ds",
        }
    }

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata=metadata),
                payload_schema={},
            )

        async def query_points(self, **kwargs):
            captured.update(kwargs)
            return SimpleNamespace(points=[])

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    await vs.search(
        collection_name="kb_ds_3",
        query_vector=[0.1, 0.2, 0.3],
    )
    assert _direct_filter_values(captured["query_filter"]) == {
        "tenant_id": "tenant-a",
        "dataset_id": "ds",
    }
    _assert_legacy_or_enabled_filter(captured["query_filter"])

    with pytest.raises(VectorStoreError, match="tenant scope mismatch"):
        await vs.search(
            collection_name="kb_ds_3",
            query_vector=[0.1, 0.2, 0.3],
            tenant_id="tenant-b",
            dataset_id="ds",
        )


@pytest.mark.asyncio
async def test_snapshot_points_preserves_vector_payload_and_normalizes_scope(
    monkeypatch,
):
    metadata = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "ds",
        }
    }
    captured = {}

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata=metadata),
                payload_schema={},
            )

        async def retrieve(self, **kwargs):
            captured.update(kwargs)
            return [
                SimpleNamespace(
                    id="point-a",
                    vector=[0.2, 0.8],
                    payload={"document_id": "doc-a", "text": "old"},
                )
            ]

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    snapshots = await store.snapshot_points(
        "kb_ds_3",
        ["point-a", "point-new"],
        tenant_id="tenant-a",
        dataset_id="ds",
    )

    assert captured == {
        "collection_name": "kb_ds_3",
        "ids": ["point-a", "point-new"],
        "with_payload": True,
        "with_vectors": True,
    }
    assert set(snapshots) == {"point-a"}
    assert snapshots["point-a"].vector == [0.2, 0.8]
    assert snapshots["point-a"].payload == {
        "tenant_id": "tenant-a",
        "dataset_id": "ds",
        "document_id": "doc-a",
        "text": "old",
    }


def _read_info(metadata):
    return SimpleNamespace(
        config=SimpleNamespace(strict_mode_config=None, metadata=metadata),
        payload_schema={},
    )


async def _invoke_read_primitive(store, primitive):
    if primitive == "dense":
        return await store.search(
            "collection-a",
            [1.0, 0.0],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    if primitive == "sparse":
        return await store.sparse_search(
            "collection-a",
            [1],
            [1.0],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    return await store.hybrid_search_multi_native(
        "collection-a",
        routes=[
            {
                "query_vector": [1.0, 0.0],
                "sparse_indices": [1],
                "sparse_values": [1.0],
            }
        ],
        top_k=2,
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("primitive", ["dense", "sparse", "hybrid"])
@pytest.mark.parametrize("profile", ["malformed_scope", "malformed_lexical"])
async def test_read_primitives_reject_unsafe_collection_metadata(
    monkeypatch,
    primitive,
    profile,
):
    scope = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }
    if profile == "malformed_scope":
        metadata = {
            COLLECTION_SCOPE_METADATA_KEY: {
                "schema_version": "bad",
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
            }
        }
    else:
        metadata = {**scope, "knowledge_lexical": {"schema_version": 1}}

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def count(self, **_kwargs):
            pytest.fail("unsafe metadata must fail before sparse readiness")

        async def query_points(self, **_kwargs):
            pytest.fail("unsafe metadata must fail before query")

        async def query_batch_points(self, **_kwargs):
            pytest.fail("unsafe metadata must fail before query")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    with pytest.raises(CollectionReadAuthorityError):
        await _invoke_read_primitive(store, primitive)


@pytest.mark.asyncio
@pytest.mark.parametrize("primitive", ["dense", "sparse", "hybrid"])
async def test_read_primitives_serve_cut_over_collection_on_the_legacy_leg(
    monkeypatch,
    primitive,
):
    """T6 replaced the blanket shadow-only fence with a direction rule: a
    collection cut over to bm25_v2 retains the lexical_v1 field, so legacy
    reads stay servable. That direction is load-bearing: rollback flips
    PostgreSQL back to lexical_v1 FIRST and the Qdrant metadata follows, so
    refusing a legacy read on v2-active metadata would take the dataset
    offline mid-rollback. The opposite direction (an ACTIVE v2 read against a
    collection that never cut over) stays refused — pinned in
    test_bm25_v2_shadow.py ("not cut over")."""
    scope = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }
    active = LexicalConfig().with_runtime_selection(
        active_version=BM25_V2,
        shadow_write_enabled=True,
    )
    metadata = {**scope, **active.to_collection_metadata()}
    captured: dict[str, list] = {}

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def count(self, **kwargs):
            captured.setdefault("count", []).append(kwargs)
            return SimpleNamespace(count=3)

        async def query_points(self, **kwargs):
            captured.setdefault("query_points", []).append(kwargs)
            return SimpleNamespace(points=[])

        async def query_batch_points(self, **kwargs):
            captured.setdefault("query_batch_points", []).append(kwargs)
            return [SimpleNamespace(points=[]), SimpleNamespace(points=[])]

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    await _invoke_read_primitive(store, primitive)

    if primitive == "sparse":
        # The legacy leg must hit the retained lexical_v1 field, never the
        # bm25_v2 field of a collection this query never elected.
        assert captured["query_points"]
        assert captured["query_points"][0]["using"] == LEXICAL_V1_FIELD
    elif primitive == "hybrid":
        assert captured["query_batch_points"]
    else:
        assert captured["query_points"]


@pytest.mark.asyncio
async def test_native_legacy_collection_requires_tenant_and_dataset_scope(monkeypatch):
    queried = False

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info({})

        async def query_batch_points(self, **_kwargs):
            nonlocal queried
            queried = True

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    with pytest.raises(CollectionReadAuthorityError, match="tenant_id and dataset_id"):
        await store.hybrid_search_multi_native(
            "legacy-mixed",
            routes=[
                {
                    "query_vector": [1.0, 0.0],
                    "sparse_indices": [1],
                    "sparse_values": [1.0],
                }
            ],
            top_k=2,
            dataset_id="dataset-a",
        )
    assert queried is False


@pytest.mark.asyncio
async def test_retrieve_vectors_is_server_and_client_scope_checked(monkeypatch):
    captured = {}
    metadata = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def scroll(self, **kwargs):
            captured.update(kwargs)
            return (
                [
                    SimpleNamespace(
                        id="segment-a",
                        payload={"tenant_id": "tenant-a", "dataset_id": "dataset-a"},
                        vector=[1.0, 0.0],
                    )
                ],
                None,
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")
    vectors = await store.retrieve_vectors(
        "collection-a",
        ["segment-a"],
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )

    assert vectors == {"segment-a": [1.0, 0.0]}
    # B5 (PRD T1.8, lead-approved): MMR only needs vectors. The scroll must
    # request ONLY the two scope fields that the authority check below reads
    # — pinning this here so the payload scope never silently widens back to
    # full payloads (perf-review 2026-08-16 rolling-fetch finding).
    assert captured["with_payload"] == ["tenant_id", "dataset_id"]
    assert {
        condition.key: condition.match.value
        for condition in captured["scroll_filter"].must
        if isinstance(condition, qmodels.FieldCondition)
    } == {"dataset_id": "dataset-a", "tenant_id": "tenant-a"}


@pytest.mark.asyncio
async def test_retrieve_vectors_rejects_foreign_record_even_if_server_ignores_filter(
    monkeypatch,
):
    metadata = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def scroll(self, **_kwargs):
            return (
                [
                    SimpleNamespace(
                        id="segment-a",
                        payload={"tenant_id": "tenant-b", "dataset_id": "dataset-a"},
                        vector=[1.0, 0.0],
                    )
                ],
                None,
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    with pytest.raises(CollectionReadAuthorityError, match="out-of-scope"):
        await store.retrieve_vectors(
            "collection-a",
            ["segment-a"],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("profile", ["malformed_scope", "legacy_missing_scope"])
async def test_retrieve_vectors_rejects_unsafe_profile_before_scroll(
    monkeypatch,
    profile,
):
    tenant_id = "tenant-a"
    if profile == "malformed_scope":
        metadata = {
            COLLECTION_SCOPE_METADATA_KEY: {
                "schema_version": "bad",
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
            }
        }
    else:
        metadata = {}
        tenant_id = ""

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def scroll(self, **_kwargs):
            pytest.fail("unsafe profile must fail before vector scroll")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    with pytest.raises(CollectionReadAuthorityError):
        await store.retrieve_vectors(
            "collection-a",
            ["segment-a"],
            tenant_id=tenant_id,
            dataset_id="dataset-a",
        )


@pytest.mark.asyncio
async def test_retrieve_vectors_serves_cut_over_collection(monkeypatch):
    """Vector fetch for MMR must survive a bm25_v2 cutover: the lexical
    version in collection metadata never blocks vector retrieval (same
    rollback-window direction rule as the read primitives above)."""
    scope = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }
    active = LexicalConfig().with_runtime_selection(
        active_version=BM25_V2,
        shadow_write_enabled=True,
    )
    metadata = {**scope, **active.to_collection_metadata()}
    scrolled = []

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def scroll(self, **kwargs):
            scrolled.append(kwargs)
            return (
                [
                    SimpleNamespace(
                        id="segment-a",
                        payload={"tenant_id": "tenant-a", "dataset_id": "dataset-a"},
                        vector=[1.0, 0.0],
                    )
                ],
                None,
            )

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    vectors = await store.retrieve_vectors(
        "collection-a",
        ["segment-a"],
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    assert vectors == {"segment-a": [1.0, 0.0]}
    assert scrolled


@pytest.mark.asyncio
async def test_multi_native_rrf_uses_one_batch_request_with_per_query_fusion(monkeypatch):
    captured = {}
    count_calls = []

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def count(self, **kwargs):
            count_calls.append(kwargs)
            return SimpleNamespace(count=3)

        async def query_batch_points(self, **kwargs):
            captured.update(kwargs)
            return [SimpleNamespace(points=[]), SimpleNamespace(points=[])]

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    await vs.hybrid_search_multi_native(
        collection_name="kb_ds_3",
        routes=[
            {
                "query_vector": [1.0, 0.0],
                "sparse_indices": [1],
                "sparse_values": [1.0],
                "dense_limit": 12,
                "sparse_limit": 13,
                "metadata_filter": {"madhab": "hanafi"},
            },
            {
                "query_vector": [2.0, 0.0],
                "sparse_indices": [2],
                "sparse_values": [1.0],
                "dense_limit": 14,
                "sparse_limit": 15,
            },
        ],
        top_k=60,
        rrf_k=60,
        dense_weight=0.7,
        sparse_weight=0.3,
        tenant_id="tenant-a",
        dataset_id="ds",
    )

    requests = captured["requests"]
    assert len(requests) == 2
    assert [len(request.prefetch) for request in requests] == [2, 2]
    assert [prefetch.limit for request in requests for prefetch in request.prefetch] == [
        12,
        13,
        14,
        15,
    ]
    assert [request.query.rrf.k for request in requests] == [60, 60]
    assert [request.query.rrf.weights for request in requests] == [
        [0.7, 0.3],
        [0.7, 0.3],
    ]
    assert [request.limit for request in requests] == [60, 60]
    assert any(
        condition.key == "metadata.madhab"
        for condition in requests[0].prefetch[0].filter.must
    )
    assert len(count_calls) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("dense_weight", "sparse_weight"),
    [(-1.0, 1.0), (float("nan"), 1.0), (0.0, 0.0)],
)
async def test_multi_native_rrf_rejects_invalid_weights(
    monkeypatch, dense_weight, sparse_weight
):
    class DummyClient:
        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    vs = VectorStore(url="http://localhost:6333")

    with pytest.raises(vector_store.VectorStoreError, match="RRF weight"):
        await vs.hybrid_search_multi_native(
            collection_name="kb_ds_3",
            routes=[],
            top_k=10,
            dense_weight=dense_weight,
            sparse_weight=sparse_weight,
        )


@pytest.mark.asyncio
async def test_multi_native_rrf_requires_sparse_backfill(monkeypatch):
    query_called = False

    class DummyClient:
        async def get_collection(self, _collection_name):
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def count(self, **kwargs):
            count_filter = kwargs.get("count_filter")
            return SimpleNamespace(count=0 if count_filter else 3)

        async def query_batch_points(self, **_kwargs):
            nonlocal query_called
            query_called = True

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    with pytest.raises(vector_store.VectorStoreError, match="sparse-vector backfill"):
        await vs.hybrid_search_multi_native(
            collection_name="legacy",
            routes=[
                {
                    "query_vector": [1.0, 0.0],
                    "sparse_indices": [1],
                    "sparse_values": [1.0],
                }
            ],
            top_k=10,
            tenant_id="tenant-a",
            dataset_id="ds",
        )

    assert query_called is False


@pytest.mark.asyncio
async def test_upsert_adds_sparse_vector_when_collection_supports_it(monkeypatch):
    captured = {}
    collection_info = SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=SimpleNamespace(size=2),
                sparse_vectors={"bm25": object()},
            ),
            metadata={
                "knowledge_scope": {
                    "schema_version": 1,
                    "dataset_id": "dataset-a",
                    "tenant_id": "tenant-a",
                }
            },
        )
    )

    class DummyClient:
        async def get_collection(self, _collection_name):
            return collection_info

        async def upsert(self, **kwargs):
            captured.update(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())

    vs = VectorStore(url="http://localhost:6333")
    collection = "kb_ds_2"
    vs._sparse_readiness[collection] = False
    await vs.upsert(
        collection,
        [
            qmodels.PointStruct(
                id="segment-1",
                vector=[1.0, 0.0],
                payload={"segment_id": "segment-1", "text": "alpha beta"},
            )
        ],
    )

    stored = captured["points"][0]
    assert stored.vector[""] == [1.0, 0.0]
    assert stored.vector["bm25"].indices
    assert stored.vector["bm25"].values
    assert stored.payload["dataset_id"] == "dataset-a"
    assert stored.payload["tenant_id"] == "tenant-a"
    assert collection not in vs._sparse_readiness


@pytest.mark.asyncio
@pytest.mark.parametrize("primitive", ["dense", "sparse", "hybrid"])
async def test_read_primitives_exclude_explicitly_disabled_points(
    monkeypatch,
    primitive,
):
    captured = {}
    metadata = {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        }
    }

    class DummyClient:
        async def get_collection(self, _collection_name):
            return _read_info(metadata)

        async def count(self, **_kwargs):
            return SimpleNamespace(count=0)

        async def query_points(self, **kwargs):
            captured["filter"] = kwargs["query_filter"]
            return SimpleNamespace(points=[])

        async def query_batch_points(self, **kwargs):
            captured["prefetch_filters"] = [
                prefetch.filter
                for request in kwargs["requests"]
                for prefetch in request.prefetch
            ]
            return [SimpleNamespace(points=[]) for _ in kwargs["requests"]]

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    await _invoke_read_primitive(store, primitive)

    filters = (
        captured["prefetch_filters"]
        if primitive == "hybrid"
        else [captured["filter"]]
    )
    assert filters
    for query_filter in filters:
        _assert_legacy_or_enabled_filter(query_filter)


def _scope_metadata(tenant_id="tenant-a", dataset_id="dataset-a"):
    return {
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "tenant_id": tenant_id,
            "dataset_id": dataset_id,
        }
    }


@pytest.mark.asyncio
async def test_segment_payload_visibility_sweeps_all_owned_collections_and_is_retryable(
    monkeypatch,
):
    writes = []
    failed_once = False
    metadata_by_collection = {
        "base": _scope_metadata(),
        "base_sections": _scope_metadata(),
        "foreign": _scope_metadata(tenant_id="tenant-b"),
    }

    class DummyClient:
        async def get_collections(self):
            return SimpleNamespace(
                collections=[
                    SimpleNamespace(name=name) for name in metadata_by_collection
                ]
            )

        async def get_collection(self, collection_name):
            return _read_info(metadata_by_collection[collection_name])

        async def set_payload(self, **kwargs):
            nonlocal failed_once
            writes.append(kwargs)
            if kwargs["collection_name"] == "base_sections" and not failed_once:
                failed_once = True
                raise RuntimeError("second collection unavailable")
            return SimpleNamespace(status="completed")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333", max_retries=1)
    kwargs = {
        "tenant_id": "tenant-a",
        "dataset_id": "dataset-a",
        "document_id": "document-a",
        "segment_id": "segment-a",
        "enabled": False,
        "lifecycle_lease_held": True,
    }

    with pytest.raises(VectorStoreError, match="second collection unavailable"):
        await store.set_segment_payload_enabled(**kwargs)
    touched = await store.set_segment_payload_enabled(**kwargs)

    assert touched == ["base", "base_sections"]
    assert [call["collection_name"] for call in writes] == [
        "base",
        "base_sections",
        "base",
        "base_sections",
    ]
    assert all(call["payload"] == {"enabled": False} for call in writes)
    assert all(call["wait"] is True for call in writes)
    assert not any(call["collection_name"] == "foreign" for call in writes)
    selector = writes[0]["points"].filter
    assert _direct_filter_values(selector) == {
        "dataset_id": "dataset-a",
        "document_id": "document-a",
    }
    identity_filters = [
        condition
        for condition in (selector.must or [])
        if isinstance(condition, qmodels.Filter)
        and any(
            isinstance(item, qmodels.FieldCondition) and item.key == "segment_id"
            for item in (condition.should or [])
        )
    ]
    assert len(identity_filters) == 1
    assert any(
        isinstance(item, qmodels.FieldCondition)
        and item.key == "segment_id"
        and item.match.value == "segment-a"
        for item in (identity_filters[0].should or [])
    )


@pytest.mark.asyncio
async def test_segment_payload_visibility_requires_lifecycle_lease(monkeypatch):
    class DummyClient:
        async def get_collections(self):
            pytest.fail("lease rejection must precede Qdrant discovery")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(url="http://localhost:6333")

    with pytest.raises(VectorStoreError, match="require a lifecycle lease"):
        await store.set_segment_payload_enabled(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            document_id="document-a",
            segment_id="segment-a",
            enabled=True,
        )


@pytest.mark.asyncio
async def test_outer_lifecycle_lease_bypasses_nested_ensure_lease(monkeypatch):
    lease_calls = 0
    ensure_calls = []

    @asynccontextmanager
    async def dataset_lease(*_args, **_kwargs):
        nonlocal lease_calls
        lease_calls += 1
        yield

    class DummyClient:
        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(
        url="http://localhost:6333",
        dataset_write_lease=dataset_lease,
    )

    async def ensure_unfenced(**kwargs):
        ensure_calls.append(kwargs)
        return "collection-a"

    store._ensure_collection_unfenced = ensure_unfenced
    await store.ensure_collection("dataset-a", 2, tenant_id="tenant-a")
    await store.ensure_collection(
        "dataset-a",
        2,
        tenant_id="tenant-a",
        lifecycle_lease_held=True,
    )

    assert lease_calls == 1
    assert len(ensure_calls) == 2


@pytest.mark.asyncio
async def test_concurrent_outer_leased_upserts_take_zero_nested_database_leases(
    monkeypatch,
):
    nested_lease_calls = 0
    qdrant_writes = []
    collection_info = SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=SimpleNamespace(size=2),
                sparse_vectors={},
            ),
            metadata=_scope_metadata(),
        )
    )

    @asynccontextmanager
    async def nested_lease(*_args, **_kwargs):
        nonlocal nested_lease_calls
        nested_lease_calls += 1
        yield

    class DummyClient:
        async def get_collection(self, _collection_name):
            return collection_info

        async def upsert(self, **kwargs):
            qdrant_writes.append(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(
        url="http://localhost:6333",
        dataset_write_lease=nested_lease,
    )

    await asyncio.gather(
        *[
            store.upsert(
                "collection-a",
                [
                    qmodels.PointStruct(
                        id=f"vector-{index}",
                        vector=[1.0, 0.0],
                        payload={
                            "segment_id": f"segment-{index}",
                            "document_id": f"document-{index}",
                            "text": "value",
                        },
                    )
                ],
                lifecycle_lease_held=True,
            )
            for index in range(4)
        ]
    )

    assert nested_lease_calls == 0
    assert len(qdrant_writes) == 4


@pytest.mark.asyncio
async def test_qdrant_call_does_not_retry_non_transient_404(monkeypatch):
    class DummyClient:
        async def close(self):
            return None

    class MissingCollectionError(RuntimeError):
        status_code = 404

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(
        url="http://localhost:6333",
        max_retries=5,
        retry_base_delay=0.001,
    )
    attempts = 0

    async def missing_collection():
        nonlocal attempts
        attempts += 1
        raise MissingCollectionError("collection not found")

    with pytest.raises(VectorStoreError, match="collection not found"):
        await store._call(missing_collection)

    assert attempts == 1


@pytest.mark.asyncio
async def test_qdrant_call_still_retries_transient_connection_failure(monkeypatch):
    class DummyClient:
        async def close(self):
            return None

    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: DummyClient())
    store = VectorStore(
        url="http://localhost:6333",
        max_retries=3,
        retry_base_delay=0.001,
    )
    attempts = 0

    async def flaky_call():
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise ConnectionError("temporary network failure")
        return "ok"

    assert await store._call(flaky_call) == "ok"
    assert attempts == 2
