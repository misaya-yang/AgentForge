from __future__ import annotations

import contextlib
import hashlib
from copy import deepcopy
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.ingestion_service import IngestionService
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.lexical_config import (
    BM25_V2_AUTHORITY_KIND,
    BM25_V2_BACKFILL_METADATA_KEY,
    BM25_V2_FIELD,
    BM25_V2_MODEL,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1,
    LEXICAL_V1_FIELD,
    STRICT_FILTER_PAYLOAD_INDEXES,
    LexicalConfig,
    LexicalConfigError,
)
from knowledge_service.services.knowledge.retrieval import text_to_sparse_vector
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.vector_store import (
    CollectionReadAuthorityError,
    VectorStore,
    VectorStoreError,
)
from qdrant_client.http import models as qmodels


def _index_config(
    *,
    active: str = LEXICAL_V1,
    shadow: bool = True,
    strict: bool = False,
    k: float = 1.2,
) -> dict[str, Any]:
    return {
        "retrieval": {
            "lexical": {
                "active_version": active,
                "bm25_v2": {
                    "shadow_write_enabled": shadow,
                    "field": BM25_V2_FIELD,
                    "model": BM25_V2_MODEL,
                    "k": k,
                    "b": 0.75,
                    "avg_len": 256,
                    "tokenizer": "multilingual",
                    "language": "none",
                    "lowercase": True,
                    "ascii_folding": False,
                    "filtering": {
                        "required_payload_indexes": ["tenant_id", "dataset_id"],
                        "strict_unindexed_filtering": strict,
                    },
                },
            }
        }
    }


def _payload_schema(field_name: str) -> SimpleNamespace:
    data_type = {
        "level": qmodels.PayloadSchemaType.INTEGER,
        "enabled": qmodels.PayloadSchemaType.BOOL,
    }.get(field_name, qmodels.PayloadSchemaType.KEYWORD)
    return SimpleNamespace(
        data_type=data_type,
        params=SimpleNamespace(is_tenant=field_name == "tenant_id"),
    )


def _created_payload_schema(kwargs: dict[str, Any]) -> SimpleNamespace:
    field_schema = kwargs["field_schema"]
    data_type = getattr(field_schema, "type", field_schema)
    return SimpleNamespace(
        data_type=data_type,
        params=SimpleNamespace(
            is_tenant=bool(getattr(field_schema, "is_tenant", False))
        ),
    )


def _direct_filter_values(query_filter: Any) -> dict[str, Any]:
    return {
        condition.key: condition.match.value
        for condition in (getattr(query_filter, "must", None) or [])
        if isinstance(condition, qmodels.FieldCondition)
    }


def _bm25_v2_scope_defaults(query_filter: Any) -> dict[str, Any]:
    defaults: dict[str, Any] = {}
    for condition in getattr(query_filter, "must", None) or []:
        if not isinstance(condition, qmodels.Filter):
            continue
        should = list(condition.should or [])
        values = [item for item in should if isinstance(item, qmodels.FieldCondition)]
        missing = [item for item in should if isinstance(item, qmodels.IsEmptyCondition)]
        assert len(values) == 1
        assert len(missing) == 1
        field_name = values[0].key
        assert missing[0].is_empty.key == field_name
        defaults[field_name] = values[0].match.value
    return defaults


def _source_digest(entries: list[tuple[str, str]]) -> str:
    lines = []
    for point_id, text in sorted(entries):
        text_digest = hashlib.sha256(text.encode()).hexdigest()
        lines.append(f"{point_id}\0{text_digest}\n")
    return hashlib.sha256("".join(lines).encode()).hexdigest()


def _point_digest(point_ids: list[str]) -> str:
    return hashlib.sha256(
        "".join(f"{point_id}\n" for point_id in sorted(point_ids)).encode()
    ).hexdigest()


def _record(config: LexicalConfig, point_id: str, text: str) -> SimpleNamespace:
    return SimpleNamespace(
        id=point_id,
        vector={BM25_V2_FIELD: qmodels.SparseVector(indices=[1], values=[1.0])},
        payload={
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "text": text,
            "_lexical": {
                "versions": [LEXICAL_V1, BM25_V2_FIELD],
                "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
                "filtering_profile_fingerprint": config.filtering.fingerprint,
                "source_text_sha256": hashlib.sha256(text.encode()).hexdigest(),
            },
        },
    )


def _backfill_receipt(
    config: LexicalConfig,
    records: list[SimpleNamespace],
) -> dict[str, Any]:
    ids = [str(record.id) for record in records]
    texts = [(str(record.id), str(record.payload["text"])) for record in records]
    return {
        "schema_version": 1,
        "status": "complete",
        "collection_name": "collection-a",
        "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
        "filtering_profile_fingerprint": config.filtering.fingerprint,
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "point_count": len(records),
        "point_ids_sha256": _point_digest(ids),
        "manifest_algorithm": "sha256(sorted-point-id-newline-v1)",
        "source_text_sha256": _source_digest(texts),
        "source_text_algorithm": (
            "sha256(sorted-point-id-text-sha256-null-newline-v1)"
        ),
        "authority_kind": BM25_V2_AUTHORITY_KIND,
        "authority_content_revision": 7,
    }


def _grant_capability(store: VectorStore, config: LexicalConfig) -> None:
    store._bm25_v2_capability_receipts[store._capability_receipt_key(config)] = float(
        "inf"
    )


class _SharedDatasetCasDatabase:
    _CONFIG_FIELDS = (
        "embedding_provider",
        "embedding_model",
        "embedding_dimension",
        "embedding_config",
        "index_config",
        "collection_name",
    )

    def __init__(self, dataset: dict[str, Any]) -> None:
        self.dataset = deepcopy(dataset)
        self.patch_calls: list[dict[str, Any]] = []

    async def get_dataset(self, _dataset_id: str) -> dict[str, Any]:
        return deepcopy(self.dataset)

    async def list_documents(
        self,
        *,
        dataset_id: str,
        limit: int,
        offset: int,
    ) -> list[dict[str, Any]]:
        assert dataset_id == self.dataset["dataset_id"]
        assert limit == 1
        assert offset == 0
        return [{"document_id": "document-a"}]

    async def patch_dataset_fields(
        self,
        _dataset_id: str,
        changes: dict[str, Any],
        *,
        expected_config: dict[str, Any] | None = None,
    ) -> dict[str, Any] | None:
        self.patch_calls.append(
            {
                "changes": deepcopy(changes),
                "expected_config": deepcopy(expected_config),
            }
        )
        if expected_config is not None:
            current_config = {
                field_name: deepcopy(self.dataset.get(field_name))
                for field_name in self._CONFIG_FIELDS
            }
            if current_config != expected_config:
                return None
        self.dataset.update(deepcopy(changes))
        return deepcopy(self.dataset)


class _ReplicaLexicalStore:
    bm25_v2_active_cutover_enabled = False

    def __init__(self) -> None:
        self.selections: list[LexicalConfig] = []

    async def ensure_collection(self, **kwargs: Any) -> str:
        self.selections.append(kwargs["lexical_config"])
        return str(kwargs["collection_name"])


def _dataset_replica(
    database: _SharedDatasetCasDatabase,
    store: _ReplicaLexicalStore,
    snapshot: dict[str, Any],
) -> DatasetService:
    service = DatasetService.__new__(DatasetService)
    service.db = database
    service._ks = SimpleNamespace(vector_store=store)
    service._transition_locks = {}

    async def require_dataset_access(
        _user: Any,
        _dataset_id: str,
        required: str = "viewer",
    ) -> dict[str, Any]:
        _ = required
        return deepcopy(snapshot)

    service.require_dataset_access = require_dataset_access
    return service


def _collection_info(
    config: LexicalConfig | None,
    *,
    include_v2: bool,
    include_filter_indexes: bool = True,
    strict: bool = False,
    dimension: int = 2,
    records: list[SimpleNamespace] | None = None,
    with_receipt: bool = False,
) -> SimpleNamespace:
    sparse = {LEXICAL_V1_FIELD: qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF)}
    if include_v2:
        sparse[BM25_V2_FIELD] = qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF)
    strict_config = (
        SimpleNamespace(
            enabled=True,
            unindexed_filtering_retrieve=False,
            unindexed_filtering_update=False,
        )
        if strict
        else None
    )
    metadata = config.to_collection_metadata() if config else {}
    if config:
        metadata[COLLECTION_SCOPE_METADATA_KEY] = {
            "schema_version": 1,
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
        }
        if with_receipt:
            metadata[BM25_V2_BACKFILL_METADATA_KEY] = _backfill_receipt(
                config,
                records or [],
            )
    payload_schema: dict[str, Any] = {}
    if include_filter_indexes:
        fields = STRICT_FILTER_PAYLOAD_INDEXES if strict else ("tenant_id", "dataset_id")
        payload_schema = {
            field_name: _payload_schema(field_name)
            for field_name in fields
        }
    return SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=SimpleNamespace(size=dimension),
                sparse_vectors=sparse,
            ),
            metadata=metadata,
            strict_mode_config=strict_config,
        ),
        payload_schema=payload_schema,
        points_count=len(records or []),
    )


def test_lexical_config_defaults_to_legacy_and_fingerprints_full_profile() -> None:
    legacy = LexicalConfig.from_index_config({})
    first = LexicalConfig.from_index_config(_index_config())
    changed = LexicalConfig.from_index_config(_index_config(k=1.5))

    assert legacy.active_version == LEXICAL_V1
    assert legacy.configured is False
    assert legacy.writes_bm25_v2 is False
    assert first.bm25_v2.fingerprint != changed.bm25_v2.fingerprint
    persisted = first.to_collection_metadata()["knowledge_lexical"]
    assert persisted["bm25_v2"]["field"] == BM25_V2_FIELD
    assert persisted["bm25_v2"]["model"] == BM25_V2_MODEL
    assert persisted["bm25_v2"]["tokenizer"] == "multilingual"
    assert persisted["bm25_v2"]["language"] == "none"
    assert persisted["filtering"]["required_payload_indexes"] == [
        "tenant_id",
        "dataset_id",
    ]
    assert LexicalConfig.from_collection_metadata(first.to_collection_metadata()) == first


def test_lexical_config_rejects_active_without_shadow_and_unknown_options() -> None:
    with pytest.raises(LexicalConfigError, match="shadow_write_enabled=true"):
        LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD, shadow=False))

    invalid = _index_config()
    invalid["retrieval"]["lexical"]["bm25_v2"]["typo_k"] = 1.2
    with pytest.raises(LexicalConfigError, match="unsupported bm25_v2 option"):
        LexicalConfig.from_index_config(invalid)


@pytest.mark.asyncio
async def test_shadow_collection_uses_separate_fields_and_filter_indexes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[tuple[str, Any]] = []
    state: dict[str, Any] = {"info": None}
    canary_points: list[Any] = []

    class Client:
        async def collection_exists(self, **_kwargs: Any) -> bool:
            return False

        async def query_points(self, **kwargs: Any) -> Any:
            calls.append(("capability", kwargs))
            repeated = next(
                point
                for point in canary_points
                if point.vector[BM25_V2_FIELD].text == "alpha alpha alpha"
            )
            diluted = next(
                point
                for point in canary_points
                if point.vector[BM25_V2_FIELD].text.startswith("alpha filler")
            )
            return SimpleNamespace(
                points=[
                    SimpleNamespace(id=repeated.id, score=2.0),
                    SimpleNamespace(id=diluted.id, score=1.0),
                ]
            )

        async def create_collection(self, **kwargs: Any) -> bool:
            calls.append(("create_collection", kwargs))
            if str(kwargs["collection_name"]).startswith("kb_bm25_v2_canary_"):
                return True
            sparse = kwargs["sparse_vectors_config"]
            state["info"] = SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        vectors=kwargs["vectors_config"],
                        sparse_vectors=sparse,
                    ),
                    metadata=kwargs.get("metadata") or {},
                    strict_mode_config=None,
                ),
                payload_schema={},
            )
            return True

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            canary_points.extend(kwargs["points"])
            return SimpleNamespace(status="completed")

        async def delete_collection(self, **_kwargs: Any) -> bool:
            calls.append(("delete_canary", None))
            return True

        async def create_payload_index(self, **kwargs: Any) -> None:
            calls.append(("payload_index", kwargs["field_name"]))
            field_name = kwargs["field_name"]
            state["info"].payload_schema[field_name] = _created_payload_schema(kwargs)

        async def get_collection(self, _collection_name: str) -> Any:
            return state["info"]

        async def close(self) -> None:
            return None

    constructor_kwargs: dict[str, Any] = {}

    def make_client(**kwargs: Any) -> Client:
        constructor_kwargs.update(kwargs)
        return Client()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        make_client,
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    config = LexicalConfig.from_index_config(_index_config())

    collection = await store.ensure_collection(
        dataset_id="dataset-a",
        dimension=2,
        lexical_config=config,
        tenant_id="tenant-a",
        bootstrap_unbound_dataset=True,
    )

    create = next(
        payload
        for name, payload in calls
        if name == "create_collection"
        and not str(payload["collection_name"]).startswith("kb_bm25_v2_canary_")
    )
    assert collection == "kb_dataset-a_2"
    assert constructor_kwargs["cloud_inference"] is True
    assert set(create["sparse_vectors_config"]) == {
        LEXICAL_V1_FIELD,
        BM25_V2_FIELD,
    }
    assert create["sparse_vectors_config"][BM25_V2_FIELD].modifier == qmodels.Modifier.IDF
    assert create["metadata"]["knowledge_lexical"] == config.to_collection_metadata()[
        "knowledge_lexical"
    ]
    assert create["metadata"][COLLECTION_SCOPE_METADATA_KEY]["tenant_id"] == "tenant-a"
    indexed = [payload for name, payload in calls if name == "payload_index"]
    assert "tenant_id" in indexed
    assert "dataset_id" in indexed
    assert not any(name == "update_collection" for name, _ in calls)


@pytest.mark.asyncio
async def test_shadow_capability_failure_prevents_collection_mutation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    target_created = False
    canary_deleted = False

    class Client:
        async def collection_exists(self, **_kwargs: Any) -> bool:
            return False

        async def create_collection(self, **kwargs: Any) -> bool:
            nonlocal target_created
            if not str(kwargs["collection_name"]).startswith("kb_bm25_v2_canary_"):
                target_created = True
            return True

        async def upsert(self, **_kwargs: Any) -> Any:
            raise RuntimeError("400 unknown inference model")

        async def delete_collection(self, **_kwargs: Any) -> bool:
            nonlocal canary_deleted
            canary_deleted = True
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(VectorStoreError, match="capability canary failed"):
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            lexical_config=LexicalConfig.from_index_config(_index_config()),
            tenant_id="tenant-a",
            bootstrap_unbound_dataset=True,
        )
    assert target_created is False
    assert canary_deleted is True


@pytest.mark.asyncio
async def test_strict_filtering_is_enabled_only_after_required_indexes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(
        _index_config(active=BM25_V2_FIELD, strict=True)
    )
    info = _collection_info(
        config,
        include_v2=True,
        include_filter_indexes=False,
    )
    events: list[str] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def create_payload_index(self, **kwargs: Any) -> None:
            events.append(f"index:{kwargs['field_name']}")
            field_name = kwargs["field_name"]
            info.payload_schema[field_name] = _created_payload_schema(kwargs)

        async def update_collection(self, **kwargs: Any) -> bool:
            events.append("strict")
            strict = kwargs["strict_mode_config"]
            info.config.strict_mode_config = SimpleNamespace(
                enabled=strict.enabled,
                unindexed_filtering_retrieve=strict.unindexed_filtering_retrieve,
                unindexed_filtering_update=strict.unindexed_filtering_update,
            )
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    await store._ensure_filtering_profile(
        "collection-a",
        config,
        info=info,
        allow_mutation=True,
    )

    assert events == [
        *(f"index:{field_name}" for field_name in STRICT_FILTER_PAYLOAD_INDEXES),
        "strict",
    ]
    assert info.payload_schema["tenant_id"].data_type.value == "keyword"
    assert info.payload_schema["tenant_id"].params.is_tenant is True
    assert info.payload_schema["level"].data_type == qmodels.PayloadSchemaType.INTEGER
    assert info.payload_schema["enabled"].data_type == qmodels.PayloadSchemaType.BOOL
    assert store._strict_filtering_is_ready(info) is True


@pytest.mark.asyncio
async def test_strict_filtering_can_be_rolled_back_to_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config(strict=False))
    info = _collection_info(
        config,
        include_v2=True,
        include_filter_indexes=True,
        strict=True,
    )
    updates: list[Any] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def update_collection(self, **kwargs: Any) -> bool:
            strict = kwargs["strict_mode_config"]
            updates.append(strict)
            info.config.strict_mode_config = SimpleNamespace(enabled=strict.enabled)
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    await store._ensure_filtering_profile(
        "collection-a",
        config,
        info=info,
        allow_mutation=True,
        enforce_strict=False,
    )

    assert len(updates) == 1
    assert updates[0].enabled is False
    assert store._strict_filtering_is_disabled(info) is True


@pytest.mark.asyncio
async def test_new_shadow_collection_failure_deletes_unclaimed_orphan(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config())
    info: Any = None
    deleted: list[str] = []

    class Client:
        async def collection_exists(self, **_kwargs: Any) -> bool:
            return False

        async def create_collection(self, **kwargs: Any) -> bool:
            nonlocal info
            info = SimpleNamespace(
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        vectors=kwargs["vectors_config"],
                        sparse_vectors=kwargs["sparse_vectors_config"],
                    ),
                    metadata=kwargs.get("metadata") or {},
                    strict_mode_config=None,
                ),
                payload_schema={},
            )
            return True

        async def create_payload_index(self, **_kwargs: Any) -> None:
            raise RuntimeError("injected payload index failure")

        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def delete_collection(self, **kwargs: Any) -> bool:
            deleted.append(kwargs["collection_name"])
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, config)

    with pytest.raises(VectorStoreError, match="payload index failure"):
        await store.ensure_collection(
            dataset_id="dataset-a",
            dimension=2,
            lexical_config=config,
            tenant_id="tenant-a",
            bootstrap_unbound_dataset=True,
        )
    assert deleted == ["kb_dataset-a_2"]


@pytest.mark.asyncio
async def test_default_store_allows_shadow_but_rejects_active_cutover_before_mutation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shadow = LexicalConfig.from_index_config(_index_config())
    active = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    info = _collection_info(
        None,
        include_v2=False,
        include_filter_indexes=False,
    )
    info.config.metadata[COLLECTION_SCOPE_METADATA_KEY] = {
        "schema_version": 1,
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
    }
    mutations: list[tuple[str, str]] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def create_payload_index(self, **kwargs: Any) -> None:
            field_name = kwargs["field_name"]
            mutations.append(("index", field_name))
            info.payload_schema[field_name] = _created_payload_schema(kwargs)

        async def update_collection(self, **kwargs: Any) -> bool:
            mutations.append(("collection", kwargs["collection_name"]))
            if kwargs.get("sparse_vectors_config"):
                info.config.params.sparse_vectors.update(
                    kwargs["sparse_vectors_config"]
                )
            if kwargs.get("metadata") is not None:
                info.config.metadata.update(kwargs["metadata"])
            return True

        async def query_points(self, **_kwargs: Any) -> Any:
            raise AssertionError("capability canary should use the cached test receipt")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, shadow)

    assert await store.ensure_lexical_config(
        "collection-a",
        shadow,
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        allow_runtime_transition=True,
    ) is True
    mutation_count_after_shadow = len(mutations)

    with pytest.raises(VectorStoreError, match="active cutover is unavailable"):
        await store.ensure_lexical_config(
            "collection-a",
            active,
            dataset_id="dataset-a",
            tenant_id="tenant-a",
            allow_runtime_transition=True,
            authority_content_revision=7,
        )

    assert len(mutations) == mutation_count_after_shadow
    persisted = LexicalConfig.from_collection_metadata(info.config.metadata)
    assert persisted is not None
    assert persisted.active_version == LEXICAL_V1
    assert persisted.writes_bm25_v2 is True


@pytest.mark.asyncio
async def test_kill_switch_rejects_active_sparse_serving_before_any_qdrant_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """T6: serving an active profile stays gated by the kill switch (release
    decision) — and it must refuse before any Qdrant validation traffic."""
    config = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    qdrant_calls: list[str] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            qdrant_calls.append("get_collection")
            raise AssertionError("kill-switched serving must fail before Qdrant validation")

        async def query_points(self, **_kwargs: Any) -> Any:
            qdrant_calls.append("query_points")
            raise AssertionError("kill-switched serving must fail before the sparse query")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1, bm25_v2_enabled=False)

    with pytest.raises(VectorStoreError, match="active serving is unavailable"):
        await store.sparse_search(
            collection_name="collection-a",
            sparse_indices=[],
            sparse_values=[],
            query_text="alpha",
            lexical_config=config,
            authority_content_revision=7,
        )

    assert qdrant_calls == []


@pytest.mark.asyncio
async def test_active_sparse_search_refuses_uncut_collection_before_query(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """T6 replaced the blanket query-time rejection with a per-collection
    authority proof: an active profile may consult Qdrant, but a collection
    that never cut over must fail loudly and never receive the v2 query."""
    config = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    info = _collection_info(
        LexicalConfig.from_index_config(_index_config()),
        include_v2=True,
    )
    qdrant_calls: list[str] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            qdrant_calls.append("get_collection")
            return info

        async def query_points(self, **_kwargs: Any) -> Any:
            qdrant_calls.append("query_points")
            raise AssertionError("active query must not reach an uncut collection")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(CollectionReadAuthorityError, match="not cut over"):
        await store.sparse_search(
            collection_name="collection-a",
            sparse_indices=[],
            sparse_values=[],
            query_text="alpha",
            lexical_config=config,
            authority_content_revision=7,
        )

    assert qdrant_calls == ["get_collection"]


@pytest.mark.asyncio
async def test_existing_collection_shadow_enable_and_rollback_are_non_destructive(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shadow = LexicalConfig.from_index_config(_index_config())
    rollback = LexicalConfig.from_index_config(_index_config(shadow=False))
    info = _collection_info(
        None,
        include_v2=False,
        include_filter_indexes=False,
    )
    info.config.metadata[COLLECTION_SCOPE_METADATA_KEY] = {
        "schema_version": 1,
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
    }
    updates: list[dict[str, Any]] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def query_points(self, **_kwargs: Any) -> Any:
            raise RuntimeError("404 collection not found")

        async def create_payload_index(self, **kwargs: Any) -> None:
            field_name = kwargs["field_name"]
            info.payload_schema[field_name] = _created_payload_schema(kwargs)

        async def update_collection(self, **kwargs: Any) -> bool:
            updates.append(kwargs)
            if kwargs.get("sparse_vectors_config"):
                info.config.params.sparse_vectors.update(kwargs["sparse_vectors_config"])
            if kwargs.get("metadata") is not None:
                # Qdrant collection metadata updates merge keys rather than
                # replacing the whole metadata object.
                info.config.metadata = {
                    **info.config.metadata,
                    **kwargs["metadata"],
                }
            if kwargs.get("strict_mode_config") is not None:
                strict = kwargs["strict_mode_config"]
                info.config.strict_mode_config = SimpleNamespace(
                    enabled=strict.enabled,
                    unindexed_filtering_retrieve=(strict.unindexed_filtering_retrieve),
                    unindexed_filtering_update=strict.unindexed_filtering_update,
                )
            return True

        async def scroll(self, **_kwargs: Any) -> Any:
            return [], None

        async def count(self, **_kwargs: Any) -> Any:
            return SimpleNamespace(count=0)

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, shadow)

    assert await store.ensure_lexical_config(
        "collection-a",
        shadow,
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        allow_runtime_transition=True,
        authority_content_revision=7,
    ) is True
    assert BM25_V2_FIELD in info.config.params.sparse_vectors
    assert await store.ensure_lexical_config(
        "collection-a",
        rollback,
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        allow_runtime_transition=True,
    ) is True
    assert BM25_V2_FIELD in info.config.params.sparse_vectors
    persisted = LexicalConfig.from_collection_metadata(info.config.metadata)
    assert persisted is not None
    assert persisted.active_version == LEXICAL_V1
    assert persisted.writes_bm25_v2 is False
    assert all("delete" not in call for update in updates for call in update)


@pytest.mark.asyncio
async def test_kill_switch_allows_emergency_active_to_v1_rollback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    active = LexicalConfig.from_index_config(
        _index_config(active=BM25_V2_FIELD, strict=True)
    )
    rollback = LexicalConfig.from_index_config(
        _index_config(active=LEXICAL_V1, shadow=False, strict=True)
    )
    records = [_record(active, "segment-a", "alpha")]
    info = _collection_info(
        active,
        include_v2=True,
        strict=True,
        records=records,
        with_receipt=True,
    )
    events: list[str] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def update_collection(self, **kwargs: Any) -> bool:
            if kwargs.get("strict_mode_config") is not None:
                events.append("strict_off")
                info.config.strict_mode_config = SimpleNamespace(
                    enabled=kwargs["strict_mode_config"].enabled
                )
            if kwargs.get("metadata") is not None:
                events.append("metadata_v1")
                info.config.metadata.update(kwargs["metadata"])
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        bm25_v2_enabled=False,
    )

    await store.ensure_lexical_config(
        "collection-a",
        rollback,
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        allow_runtime_transition=True,
    )

    persisted = LexicalConfig.from_collection_metadata(info.config.metadata)
    assert persisted is not None
    assert persisted.active_version == LEXICAL_V1
    assert persisted.writes_bm25_v2 is False
    assert store._strict_filtering_is_disabled(info) is True
    assert events == ["strict_off", "metadata_v1"]


@pytest.mark.asyncio
async def test_dataset_create_rejects_active_v2_before_persistent_mutation() -> None:
    class Database:
        def __init__(self) -> None:
            self.created = False

        async def dataset_exists(self, _dataset_id: str) -> bool:
            return False

        async def create_dataset_with_owner(
            self,
            _dataset: dict[str, Any],
            _user_id: str,
        ) -> bool:
            self.created = True
            raise AssertionError("hard-disable must precede dataset creation")

    class Store:
        bm25_v2_active_cutover_enabled = False

        async def ensure_collection(self, **_kwargs: Any) -> str:
            raise AssertionError("hard-disable must precede collection creation")

    database = Database()
    service = DatasetService.__new__(DatasetService)
    service.db = database
    service._ks = SimpleNamespace(vector_store=Store())
    service._transition_locks = {}
    user = SimpleNamespace(
        is_authenticated=True,
        roles=[],
        tenant_id="tenant-a",
        user_id="user-a",
    )

    with pytest.raises(ValidationFailedError, match="active cutover is hard-disabled"):
        await service.create_dataset(
            user,
            {
                "dataset_id": "dataset-a",
                "index_config": _index_config(active=BM25_V2_FIELD),
            },
        )

    assert database.created is False


@pytest.mark.asyncio
async def test_dataset_update_rejects_active_v2_before_persistent_mutation() -> None:
    original = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_dimension": 2,
        "index_config": {},
    }

    class Database:
        def __init__(self) -> None:
            self.saved = False

        async def patch_dataset_fields(
            self,
            _dataset_id: str,
            _changes: dict[str, Any],
            *,
            expected_config: dict[str, Any] | None = None,
        ) -> dict[str, Any]:
            _ = expected_config
            self.saved = True
            raise AssertionError("hard-disable must precede dataset patch")

    class Store:
        bm25_v2_active_cutover_enabled = False

        async def ensure_collection(self, **_kwargs: Any) -> str:
            raise AssertionError("hard-disable must precede collection transition")

    database = Database()
    service = DatasetService.__new__(DatasetService)
    service.db = database
    service._ks = SimpleNamespace(vector_store=Store())
    service._transition_locks = {}

    async def require_dataset_access(
        _user: Any,
        _dataset_id: str,
        required: str = "viewer",
    ) -> dict[str, Any]:
        _ = required
        return dict(original)

    service.require_dataset_access = require_dataset_access

    with pytest.raises(ValidationFailedError, match="active cutover is hard-disabled"):
        await service.update_dataset(
            SimpleNamespace(),
            "dataset-a",
            {"index_config": _index_config(active=BM25_V2_FIELD)},
        )

    assert database.saved is False


@pytest.mark.asyncio
async def test_cross_replica_stale_name_patch_preserves_completed_shadow_config() -> None:
    initial = {
        "dataset_id": "dataset-a",
        "name": "Original",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "content_revision": 7,
    }
    stale_config_snapshot = deepcopy(initial)
    stale_name_snapshot = deepcopy(initial)
    database = _SharedDatasetCasDatabase(initial)
    store = _ReplicaLexicalStore()
    config_replica = _dataset_replica(database, store, stale_config_snapshot)
    name_replica = _dataset_replica(database, store, stale_name_snapshot)
    shadow_config = _index_config()

    await config_replica.update_dataset(
        SimpleNamespace(),
        "dataset-a",
        {"index_config": shadow_config},
    )
    selections_after_shadow = len(store.selections)
    await name_replica.update_dataset(
        SimpleNamespace(),
        "dataset-a",
        {"name": "Renamed by stale replica"},
    )

    assert database.dataset["name"] == "Renamed by stale replica"
    assert database.dataset["index_config"] == shadow_config
    assert database.patch_calls[0]["expected_config"] is not None
    assert database.patch_calls[1] == {
        "changes": {"name": "Renamed by stale replica"},
        "expected_config": None,
    }
    assert len(store.selections) == selections_after_shadow
    assert store.selections[-1].active_version == LEXICAL_V1
    assert store.selections[-1].writes_bm25_v2 is True


@pytest.mark.asyncio
async def test_cross_replica_config_cas_loser_reconciles_qdrant_to_winner() -> None:
    initial = {
        "dataset_id": "dataset-a",
        "name": "Original",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "content_revision": 7,
    }
    winner_snapshot = deepcopy(initial)
    loser_snapshot = deepcopy(initial)
    database = _SharedDatasetCasDatabase(initial)
    store = _ReplicaLexicalStore()
    winner = _dataset_replica(database, store, winner_snapshot)
    loser = _dataset_replica(database, store, loser_snapshot)
    winner_config = _index_config(k=1.2)
    loser_config = _index_config(k=1.5)

    await winner.update_dataset(
        SimpleNamespace(),
        "dataset-a",
        {"index_config": winner_config},
    )
    with pytest.raises(
        ValidationFailedError,
        match="dataset configuration changed concurrently; retry",
    ):
        await loser.update_dataset(
            SimpleNamespace(),
            "dataset-a",
            {"index_config": loser_config},
        )

    assert database.dataset["index_config"] == winner_config
    assert len(database.patch_calls) == 2
    assert database.patch_calls[1]["expected_config"] == {
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "collection_name": "collection-a",
    }
    assert store.selections[-2].bm25_v2.k == 1.5
    assert store.selections[-1].bm25_v2.k == 1.2
    assert store.selections[-1].active_version == LEXICAL_V1
    assert store.selections[-1].writes_bm25_v2 is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "patch",
    [
        {"embedding_model": "same-dimension-but-incompatible"},
        {"embedding_config": {"max_concurrent": 5}},
        {"index_config": {"chunking": {"chunk_size": 777}}},
    ],
)
async def test_existing_documents_freeze_dense_and_chunking_identity(
    patch: dict[str, Any],
) -> None:
    initial = {
        "dataset_id": "dataset-a",
        "name": "Original",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "content_revision": 7,
    }
    database = _SharedDatasetCasDatabase(initial)
    store = _ReplicaLexicalStore()
    service = _dataset_replica(database, store, deepcopy(initial))

    with pytest.raises(
        ValidationFailedError,
        match="Cannot change embedding or ingestion index identity",
    ):
        await service.update_dataset(
            SimpleNamespace(),
            "dataset-a",
            patch,
        )

    assert database.patch_calls == []
    assert store.selections == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("patch", "expected_fragment"),
    [
        # An embedding provider/model/dimension change is a blue-green
        # migration once vectors exist (T3): the freeze points the operator
        # at the migration endpoint instead of a dead end.
        (
            {"embedding_model": "other-model"},
            "/embedding-migration/start",
        ),
        (
            {"embedding_dimension": 4},
            "/embedding-migration/start",
        ),
        # A config-only change keeps the legacy reindex-generation guidance.
        (
            {"embedding_config": {"max_concurrent": 5}},
            "create a reindexed generation",
        ),
    ],
)
async def test_embedding_identity_freeze_offers_migration_path(
    patch: dict[str, Any], expected_fragment: str
) -> None:
    initial = {
        "dataset_id": "dataset-a",
        "name": "Original",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "content_revision": 7,
    }
    database = _SharedDatasetCasDatabase(initial)
    store = _ReplicaLexicalStore()
    service = _dataset_replica(database, store, deepcopy(initial))

    with pytest.raises(ValidationFailedError) as excinfo:
        await service.update_dataset(SimpleNamespace(), "dataset-a", patch)

    assert expected_fragment in str(excinfo.value)
    assert database.patch_calls == []
    assert store.selections == []


@pytest.mark.asyncio
async def test_existing_documents_allow_shadow_lexical_transition() -> None:
    initial = {
        "dataset_id": "dataset-a",
        "name": "Original",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 2,
        "embedding_config": {},
        "index_config": {},
        "content_revision": 7,
    }
    database = _SharedDatasetCasDatabase(initial)
    store = _ReplicaLexicalStore()
    service = _dataset_replica(database, store, deepcopy(initial))

    result = await service.update_dataset(
        SimpleNamespace(),
        "dataset-a",
        {"index_config": _index_config()},
    )

    assert result["index_config"] == _index_config()
    assert store.selections[-1].active_version == LEXICAL_V1
    assert store.selections[-1].writes_bm25_v2 is True


@pytest.mark.asyncio
async def test_existing_v2_rejects_changed_encoding_fingerprint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    stored = LexicalConfig.from_index_config(_index_config())
    changed = LexicalConfig.from_index_config(_index_config(k=1.8))
    info = _collection_info(stored, include_v2=True)

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)

    with pytest.raises(VectorStoreError, match="encoding is immutable"):
        await store.ensure_lexical_config("collection-a", changed)


@pytest.mark.asyncio
async def test_rollback_v1_write_invalidates_cached_v1_readiness(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    rollback = LexicalConfig.from_index_config(_index_config(shadow=False))
    info = _collection_info(rollback, include_v2=True)
    captured: dict[str, Any] = {}

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            captured.update(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    store._collection_dims["collection-a"] = 2
    store._sparse_collections.add("collection-a")
    store._sparse_readiness["collection-a"] = True

    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id="segment-a",
                vector=[1.0, 0.0],
                payload={"text": "alpha"},
            )
        ],
    )

    assert BM25_V2_FIELD not in captured["points"][0].vector
    assert "collection-a" not in store._sparse_readiness


@pytest.mark.asyncio
async def test_shadow_upsert_preserves_v1_and_adds_native_bm25_document(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config())
    info = _collection_info(config, include_v2=True)
    captured: dict[str, Any] = {"markers": []}

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            captured["base"] = kwargs
            return SimpleNamespace(status="completed")

        async def update_vectors(self, **kwargs: Any) -> Any:
            captured["shadow"] = kwargs
            return SimpleNamespace(status="completed")

        async def set_payload(self, **kwargs: Any) -> Any:
            captured["markers"].append(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    # Writers refresh collection metadata on every call, so another replica's
    # shadow enablement is observed without process-local lexical state.
    _grant_capability(store, config)
    text = "alpha alpha beta"
    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id="segment-a",
                vector=[1.0, 0.0],
                payload={
                    "dataset_id": "dataset-a",
                    "tenant_id": "tenant-a",
                    "segment_id": "segment-a",
                    "text": text,
                },
            )
        ],
    )

    point = captured["base"]["points"][0]
    expected_indices, expected_values = text_to_sparse_vector(text)
    assert point.vector[LEXICAL_V1_FIELD].indices == expected_indices
    assert point.vector[LEXICAL_V1_FIELD].values == expected_values
    assert BM25_V2_FIELD not in point.vector
    assert "_lexical" not in point.payload
    shadow_point = captured["shadow"]["points"][0]
    native = shadow_point.vector[BM25_V2_FIELD]
    assert isinstance(native, qmodels.Document)
    assert native.model == BM25_V2_MODEL
    assert native.text == text
    assert native.options.k == 1.2
    assert native.options.b == 0.75
    assert native.options.avg_len == 256
    assert native.options.tokenizer == qmodels.TokenizerType.MULTILINGUAL
    assert native.options.language == "none"
    assert captured["markers"][0]["payload"]["_lexical"]["versions"] == [
        "lexical_v1",
        "bm25_v2",
    ]
    assert store.bm25_v2_shadow_write_stats() == {"failures": 0, "failed_points": 0}


@pytest.mark.asyncio
async def test_shadow_upsert_excludes_non_text_non_l3_and_disabled_points(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config())
    info = _collection_info(config, include_v2=True)
    captured: dict[str, Any] = {"markers": []}

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            captured["base"] = kwargs
            return SimpleNamespace(status="completed")

        async def update_vectors(self, **kwargs: Any) -> Any:
            captured["shadow"] = kwargs
            return SimpleNamespace(status="completed")

        async def set_payload(self, **kwargs: Any) -> Any:
            captured["markers"].append(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, config)
    payloads = {
        "eligible-defaults": {},
        "eligible-explicit": {
            "content_type": "text",
            "level": 3,
            "enabled": True,
        },
        "image": {"content_type": "image"},
        "page-image": {"content_type": "page_image"},
        "mixed": {"content_type": "mixed"},
        "empty-content-type": {"content_type": ""},
        "level-zero": {"level": 0},
        "level-four": {"level": 4},
        "level-string": {"level": "3"},
        "disabled": {"enabled": False},
        "enabled-string": {"enabled": "true"},
    }

    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id=point_id,
                vector=[1.0, 0.0],
                payload={"text": f"text for {point_id}", **scope_payload},
            )
            for point_id, scope_payload in payloads.items()
        ],
    )

    assert all(
        LEXICAL_V1_FIELD in point.vector for point in captured["base"]["points"]
    )
    assert [point.id for point in captured["shadow"]["points"]] == [
        "eligible-defaults",
        "eligible-explicit",
    ]
    assert [call["points"] for call in captured["markers"]] == [
        ["eligible-defaults"],
        ["eligible-explicit"],
    ]
    assert store.bm25_v2_shadow_write_stats() == {"failures": 0, "failed_points": 0}


@pytest.mark.asyncio
async def test_shadow_vector_failure_keeps_base_write_and_logs_cleanly(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    config = LexicalConfig.from_index_config(_index_config())
    info = _collection_info(config, include_v2=True)
    events: list[str] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def upsert(self, **_kwargs: Any) -> SimpleNamespace:
            events.append("base")
            return SimpleNamespace(status="completed")

        async def update_vectors(self, **_kwargs: Any) -> Any:
            events.append("shadow")
            raise RuntimeError("injected shadow failure")

        async def set_payload(self, **_kwargs: Any) -> Any:
            raise AssertionError("marker must not be published after vector failure")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, config)

    with caplog.at_level("WARNING"):
        await store.upsert(
            "collection-a",
            [
                qmodels.PointStruct(
                    id="segment-a",
                    vector=[1.0, 0.0],
                    payload={"text": "alpha"},
                )
            ],
        )

    assert events == ["base", "shadow"]
    assert store.bm25_v2_shadow_write_stats() == {"failures": 1, "failed_points": 1}
    messages = [record.getMessage() for record in caplog.records]
    assert any(
        "bm25_v2_shadow_write_failed" in message
        and "injected shadow failure" in message
        for message in messages
    )


@pytest.mark.asyncio
async def test_kill_switch_rejects_admin_enable_and_active_query_but_not_shadow_base(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shadow = LexicalConfig.from_index_config(_index_config())
    active = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    legacy_info = _collection_info(None, include_v2=False)
    shadow_info = _collection_info(shadow, include_v2=True)
    current_info = legacy_info
    base_writes = 0

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return current_info

        async def upsert(self, **_kwargs: Any) -> SimpleNamespace:
            nonlocal base_writes
            base_writes += 1
            return SimpleNamespace(status="completed")

        async def update_vectors(self, **_kwargs: Any) -> Any:
            raise AssertionError("kill switch must suppress shadow vector writes")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(
        url="http://qdrant",
        max_retries=1,
        bm25_v2_enabled=False,
    )

    with pytest.raises(VectorStoreError, match="disabled by the service kill switch"):
        await store.ensure_lexical_config(
            "collection-a",
            shadow,
            dataset_id="dataset-a",
            tenant_id="tenant-a",
            allow_runtime_transition=True,
        )
    with pytest.raises(VectorStoreError, match="active serving is unavailable"):
        await store.sparse_search(
            collection_name="collection-a",
            sparse_indices=[],
            sparse_values=[],
            query_text="alpha",
            lexical_config=active,
        )

    current_info = shadow_info
    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id="segment-a",
                vector=[1.0, 0.0],
                payload={"text": "alpha"},
            )
        ],
    )
    assert base_writes == 1
    assert store.bm25_v2_shadow_write_stats() == {"failures": 1, "failed_points": 1}


@pytest.mark.asyncio
async def test_shadow_upsert_empty_text_keeps_v1_and_records_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config())
    info = _collection_info(config, include_v2=True)
    captured: dict[str, Any] = {}

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            captured.update(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    _grant_capability(store, config)
    await store.upsert(
        "collection-a",
        [qmodels.PointStruct(id="segment-a", vector=[1.0, 0.0], payload={})],
    )
    assert BM25_V2_FIELD not in captured["points"][0].vector
    assert captured["points"][0].payload["dataset_id"] == "dataset-a"
    assert store.bm25_v2_shadow_write_stats() == {"failures": 1, "failed_points": 1}


@pytest.mark.asyncio
async def test_upsert_fails_when_lexical_schema_cannot_be_discovered(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    upsert_called = False

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            raise RuntimeError("metadata unavailable")

        async def upsert(self, **_kwargs: Any) -> None:
            nonlocal upsert_called
            upsert_called = True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    with pytest.raises(VectorStoreError, match="metadata unavailable"):
        await store.upsert(
            "collection-a",
            [
                qmodels.PointStruct(
                    id="segment-a",
                    vector=[1.0, 0.0],
                    payload={"text": "alpha"},
                )
            ],
        )
    assert upsert_called is False


@pytest.mark.asyncio
async def test_segment_batch_compensates_qdrant_when_database_write_fails() -> None:
    class Store:
        def __init__(self) -> None:
            self.deleted: list[str] = []
            self.points = {
                "segment-a": qmodels.PointStruct(
                    id="segment-a",
                    vector=[0.25, 0.75],
                    payload={
                        "dataset_id": "dataset-a",
                        "document_id": "document-a",
                        "text": "old serving payload",
                    },
                )
            }

        async def snapshot_points(
            self, _collection: str, point_ids: list[str], **_kwargs: Any
        ) -> dict[str, qmodels.PointStruct]:
            return {
                point_id: deepcopy(self.points[point_id])
                for point_id in point_ids
                if point_id in self.points
            }

        async def upsert(self, *, points: list[Any], **_kwargs: Any) -> None:
            for point in points:
                self.points[str(point.id)] = deepcopy(point)

        async def delete_points(
            self,
            _collection: str,
            point_ids: list[str],
            **_scope: Any,
        ) -> None:
            self.deleted.extend(point_ids)
            for point_id in point_ids:
                self.points.pop(point_id, None)

    class Database:
        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(
            self,
            _dataset_id: str,
            _document_ids: list[str],
            *,
            expected_ingestion_identity: str,
        ):
            assert expected_ingestion_identity == "identity-a"
            yield

        async def insert_segments(self, _rows: list[dict[str, Any]]) -> None:
            raise RuntimeError("database rejected batch")

    store = Store()
    service = IngestionService(SimpleNamespace(), Database(), store)
    points = [
        qmodels.PointStruct(
            id="segment-a",
            vector=[1.0, 0.0],
            payload={"dataset_id": "dataset-a", "document_id": "document-a"},
        ),
        qmodels.PointStruct(
            id="segment-b",
            vector=[0.0, 1.0],
            payload={"dataset_id": "dataset-a", "document_id": "document-a"},
        ),
    ]

    with pytest.raises(RuntimeError, match="database rejected batch"):
        await service._persist_segment_batch(
            collection="collection-a",
            points=points,
            segment_rows=[{"segment_id": "segment-a"}, {"segment_id": "segment-b"}],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            expected_ingestion_identity="identity-a",
        )
    # The overwritten serving point is restored exactly; compensation deletes
    # only the point that did not exist before this attempted generation.
    assert store.deleted == ["segment-b"]
    assert set(store.points) == {"segment-a"}
    assert store.points["segment-a"].vector == [0.25, 0.75]
    assert store.points["segment-a"].payload == {
        "dataset_id": "dataset-a",
        "document_id": "document-a",
        "text": "old serving payload",
    }


@pytest.mark.asyncio
async def test_segment_batch_does_not_touch_database_after_qdrant_failure() -> None:
    class Store:
        async def snapshot_points(
            self, _collection: str, _point_ids: list[str], **_kwargs: Any
        ) -> dict[str, qmodels.PointStruct]:
            return {}

        async def upsert(self, **_kwargs: Any) -> None:
            raise RuntimeError("qdrant rejected batch")

    class Database:
        def __init__(self) -> None:
            self.called = False

        @contextlib.asynccontextmanager
        async def dataset_index_write_lease(
            self,
            _dataset_id: str,
            _document_ids: list[str],
            *,
            expected_ingestion_identity: str,
        ):
            assert expected_ingestion_identity == "identity-a"
            yield

        async def insert_segments(self, _rows: list[dict[str, Any]]) -> None:
            self.called = True

    database = Database()
    service = IngestionService(SimpleNamespace(), database, Store())

    with pytest.raises(RuntimeError, match="qdrant rejected batch"):
        await service._persist_segment_batch(
            collection="collection-a",
            points=[
                qmodels.PointStruct(
                    id="segment-a",
                    vector=[1.0, 0.0],
                    payload={"dataset_id": "dataset-a", "document_id": "document-a"},
                )
            ],
            segment_rows=[{"segment_id": "segment-a"}],
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            expected_ingestion_identity="identity-a",
        )
    assert database.called is False


@pytest.mark.asyncio
async def test_active_profile_strictly_dual_writes_and_invalidates_receipt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    records = [_record(config, "segment-a", "old text")]
    info = _collection_info(
        config,
        include_v2=True,
        records=records,
        with_receipt=True,
    )
    upserts: list[Any] = []
    deletes: list[Any] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def update_collection(self, **kwargs: Any) -> bool:
            info.config.metadata.update(kwargs.get("metadata") or {})
            return True

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            upserts.extend(kwargs["points"])
            return SimpleNamespace(status="completed")

        async def delete(self, **kwargs: Any) -> SimpleNamespace:
            deletes.append(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1, bm25_v2_enabled=True)
    _grant_capability(store, config)

    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id="segment-a",
                vector=[1.0, 0.0],
                payload={"text": "new text"},
            )
        ],
    )
    await store.delete_points("collection-a", ["segment-a"])

    assert len(upserts) == 1
    assert set(upserts[0].vector) == {"", LEXICAL_V1_FIELD, BM25_V2_FIELD}
    assert set(upserts[0].payload["_lexical"]["versions"]) == {
        "lexical_v1",
        "bm25_v2",
    }
    assert info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY]["status"] == "invalidated"
    assert len(deletes) == 1


@pytest.mark.asyncio
async def test_active_nonlexical_image_write_preserves_bm25_receipt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    config = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))
    records = [_record(config, "segment-a", "old text")]
    info = _collection_info(
        config,
        include_v2=True,
        records=records,
        with_receipt=True,
    )
    original_receipt = dict(info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY])
    upserts: list[Any] = []
    deletes: list[Any] = []

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def update_collection(self, **kwargs: Any) -> bool:
            raise AssertionError(f"image write invalidated lexical receipt: {kwargs}")

        async def upsert(self, **kwargs: Any) -> SimpleNamespace:
            upserts.extend(kwargs["points"])
            return SimpleNamespace(status="completed")

        async def delete(self, **kwargs: Any) -> SimpleNamespace:
            deletes.append(kwargs)
            return SimpleNamespace(status="completed")

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1, bm25_v2_enabled=True)
    _grant_capability(store, config)
    await store.upsert(
        "collection-a",
        [
            qmodels.PointStruct(
                id="image-a",
                vector=[1.0, 0.0],
                payload={"text": "image description", "content_type": "image"},
            )
        ],
    )
    await store.delete_points(
        "collection-a",
        ["image-a"],
        affects_bm25_scope=False,
    )

    assert len(upserts) == 1
    assert set(upserts[0].vector) == {"", LEXICAL_V1_FIELD}
    assert "_lexical" not in upserts[0].payload
    assert info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY] == original_receipt
    assert len(deletes) == 1



class _NoFtsDatabase:
    def __init__(self) -> None:
        self.calls = 0

    async def search_segments_text(self, **_kwargs: Any) -> list[Any]:
        self.calls += 1
        raise AssertionError("active bm25_v2 must not call PostgreSQL FTS")


def _make_v2_retrieval_service(vector_store: Any) -> tuple[RetrievalService, _NoFtsDatabase]:
    config = _index_config(active=BM25_V2_FIELD)

    async def require_dataset_access(
        _user: Any, dataset_id: str, required: str = "viewer"
    ) -> dict[str, Any]:
        _ = required
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "index_config": config,
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "content_revision": 7,
        }

    async def image_url(_raw_url: Any, _segment_id: Any) -> None:
        return None

    database = _NoFtsDatabase()
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        database,
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
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, source_type, language, metadata: (
            KnowledgeService._filter_candidates_by_metadata(
                None, candidates, source_type, language, metadata
            )
        ),
        _get_presigned_image_url=image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return service, database


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["dense", "bm25", "hybrid"])
async def test_retrieval_active_v2_serves_from_store_authority_not_blanket_gate(
    mode: str,
) -> None:
    """T6: the blanket "shadow-only" retrieval refusal is gone — a protocol-
    cut-over dataset serves. Safety moved to the per-collection authority
    check, so an active profile against a non-readable collection still fails
    loudly, and the PostgreSQL FTS fallback leg is never reached."""

    class AuthorityStore:
        readable_calls: list[tuple[Any, ...]] = []

        async def require_collection_readable(self, *args: Any, **kwargs: Any) -> dict:
            self.readable_calls.append((args, kwargs))
            raise CollectionReadAuthorityError(
                "collection 'collection-a' is not cut over to bm25_v2"
            )

    service, database = _make_v2_retrieval_service(AuthorityStore())
    with pytest.raises(ValidationFailedError, match="not readable"):
        await service.retrieve(
            user=SimpleNamespace(),
            dataset_id="dataset-a",
            query="alpha",
            mode=mode,
            top_k=1,
        )
    assert database.calls == 0
    assert len(service.vector_store.readable_calls) == 1


@pytest.mark.asyncio
async def test_retrieval_batch_active_v2_reaches_store_before_error_fallback() -> None:
    class AuthorityStore:
        def __init__(self) -> None:
            self.calls = 0

        async def require_collection_readable(self, *_args: Any, **_kwargs: Any) -> dict:
            self.calls += 1
            raise CollectionReadAuthorityError("not cut over")

    store = AuthorityStore()
    service, database = _make_v2_retrieval_service(store)
    with pytest.raises(ValidationFailedError, match="not readable"):
        await service.retrieve_batch(
            user=SimpleNamespace(),
            dataset_id="dataset-a",
            queries=["alpha"],
            mode="dense",
        )
    assert database.calls == 0
    assert store.calls >= 1


@pytest.mark.asyncio
async def test_hierarchical_route_continues_past_active_v2_profile() -> None:
    """The route-level blanket refusal was a shadow-release artifact; T6
    replaces it with store-side per-query gating, so the hierarchical route
    must no longer reject an active profile — it proceeds to the store."""
    from knowledge_service.api.routes.knowledge import _run_hierarchical_retrieval

    class Service:
        async def require_dataset_access(
            self,
            _user: Any,
            _dataset_id: str,
            required: str = "viewer",
        ) -> dict[str, Any]:
            _ = required
            return {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "index_config": _index_config(active=BM25_V2_FIELD),
            }

        def __getattr__(self, name: str) -> Any:
            raise AssertionError(f"active hierarchical retrieval continued: {name}")

    with pytest.raises(AssertionError, match="hierarchical retrieval continued"):
        await _run_hierarchical_retrieval(
            dataset_id="dataset-a",
            query="alpha",
            top_k=1,
            strategy="top_down",
            l1_top_k=1,
            l2_top_k=1,
            include_context=True,
            score_threshold=None,
            svc=Service(),  # type: ignore[arg-type]
            user=SimpleNamespace(),
        )


@pytest.mark.asyncio
async def test_hierarchical_route_discards_deletion_generation_overlap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from knowledge_service.api.routes.knowledge import _run_hierarchical_retrieval

    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "content_revision": 9,
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 3,
        "embedding_config": {},
        "index_config": _index_config(active=LEXICAL_V1),
    }

    class Service:
        class VectorStore:
            async def require_hierarchical_collections_readable(
                self,
                _collection_name: str,
                **_kwargs: Any,
            ) -> None:
                return None

        vector_store = VectorStore()
        db = SimpleNamespace()

        async def require_dataset_access(
            self,
            _user: Any,
            _dataset_id: str,
            required: str = "viewer",
        ) -> dict[str, Any]:
            assert required == "viewer"
            return dict(dataset)

        def _is_multimodal_dataset(self, _dataset: dict[str, Any]) -> bool:
            return False

        def _resolve_embedding_config(self, **_kwargs: Any) -> SimpleNamespace:
            return SimpleNamespace()

    async def get_embedder(*_args: Any, **_kwargs: Any) -> SimpleNamespace:
        return SimpleNamespace()

    async def hierarchical_retrieve(**_kwargs: Any):
        dataset["content_revision"] += 2
        return [], SimpleNamespace()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.embedding.get_cached_embedder",
        get_embedder,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.hierarchical_retriever.hierarchical_retrieve",
        hierarchical_retrieve,
    )

    with pytest.raises(ValidationFailedError, match="generation changed"):
        await _run_hierarchical_retrieval(
            dataset_id="dataset-a",
            query="alpha",
            top_k=1,
            strategy="top_down",
            l1_top_k=1,
            l2_top_k=1,
            include_context=True,
            score_threshold=None,
            svc=Service(),  # type: ignore[arg-type]
            user=SimpleNamespace(),
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "reason",
    ["summary collection stored active", "sections collection malformed scope"],
)
async def test_hierarchical_secondary_authority_rejects_before_embedding(
    monkeypatch: pytest.MonkeyPatch,
    reason: str,
) -> None:
    from knowledge_service.api.routes.knowledge import _run_hierarchical_retrieval

    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "content_revision": 9,
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 3,
        "embedding_config": {},
        "index_config": _index_config(active=LEXICAL_V1),
    }

    class VectorStoreProbe:
        async def require_hierarchical_collections_readable(
            self,
            _collection_name: str,
            **_kwargs: Any,
        ) -> None:
            raise CollectionReadAuthorityError(reason)

    class Service:
        vector_store = VectorStoreProbe()
        db = SimpleNamespace()

        @staticmethod
        def _is_multimodal_dataset(_dataset: dict[str, Any]) -> bool:
            return False

        async def require_dataset_access(self, *_args: Any, **_kwargs: Any):
            return dict(dataset)

    async def embedding_must_not_start(*_args: Any, **_kwargs: Any):
        pytest.fail("hierarchical collection authority must precede embedding")

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.embedding.get_cached_embedder",
        embedding_must_not_start,
    )

    with pytest.raises(ValidationFailedError, match=reason):
        await _run_hierarchical_retrieval(
            dataset_id="dataset-a",
            query="alpha",
            top_k=1,
            strategy="cascade",
            l1_top_k=1,
            l2_top_k=1,
            include_context=True,
            score_threshold=None,
            svc=Service(),  # type: ignore[arg-type]
            user=SimpleNamespace(),
        )
