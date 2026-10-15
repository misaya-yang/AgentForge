from __future__ import annotations

import asyncio
import json
import math
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, Mock

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence import database as database_module
from knowledge_service.persistence.database import (
    DatabaseStorage,
    dataset_index_deletion_fence,
    dataset_ingestion_identity,
    make_dataset_index_deletion_fence,
)
from knowledge_service.services.knowledge import embedding as embedding_module
from knowledge_service.services.knowledge import vector_store as vector_store_module
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.vector_store import VectorStore, VectorStoreError
from qdrant_client.http import models as qmodels


class _Embedder:
    _dimension = 3

    async def close(self) -> None:
        return None


def _user(*, tenant_id: str = "tenant-attacker") -> SimpleNamespace:
    return SimpleNamespace(
        user_id="user-attacker",
        tenant_id=tenant_id,
        roles=["user"],
        is_authenticated=True,
    )


INVALID_PERSISTED_RETRIEVAL_CONFIGS = [
    {"top_k": 101},
    {"top_k": "10"},
    {"vector_top_k": 1001},
    {"vector": {"top_k": 1001}},
    {"keyword": {"top_k": 1001}},
    {"rerank": {"top_n": 1001}},
    {"candidate_top_k": 2001},
    {"keyword_candidate_k": 501},
    {"keyword": {"candidate_pool_size": 501}},
    {"fusion": {"rrf_k": 10_001}},
    {"fusion": {"rrf_weights": {"dense": math.nan}}},
    {"fusion": {"rrf_weights": {"dense": 0.0, "bm25": 0.0}}},
    {"dense_weight": math.nan},
    {"fusion": {"alpha": math.inf}},
    {"score_threshold": -0.1},
    {"mmr": {"lambda": 1.1}},
    {"mmr": {"similarity_threshold": math.nan}},
    {"dense_weight": 0.0, "bm25_weight": 0.0},
]


def _service(monkeypatch: pytest.MonkeyPatch) -> tuple[DatasetService, AsyncMock, AsyncMock]:
    monkeypatch.setattr(
        embedding_module,
        "create_embedding",
        lambda _config, **_kwargs: _Embedder(),
    )

    database = AsyncMock()
    database.dataset_exists.return_value = False
    database.collection_name_in_use.return_value = False
    database.create_dataset.return_value = True
    database.create_dataset_with_owner.return_value = True
    database.get_dataset.return_value = {
        "dataset_id": "new-dataset",
        "tenant_id": "tenant-attacker",
        "created_by": "user-attacker",
        "collection_name": "new-collection",
        "embedding_config": {},
        "index_config": {},
    }

    vector_store = AsyncMock()
    vector_store.make_collection_name = Mock(
        side_effect=(
            lambda dataset_id, dimension, collection_name=None: (
                collection_name or f"kb_{dataset_id}_{dimension}"
            )
        )
    )
    vector_store.ensure_collection.return_value = "new-collection"
    resolver = Mock(return_value=SimpleNamespace(timeout_seconds=1.0))

    service = object.__new__(DatasetService)
    service.db = database
    service._ks = SimpleNamespace(
        vector_store=vector_store,
        _resolve_embedding_config=resolver,
    )
    service._transition_locks = {}
    return service, database, vector_store


@pytest.mark.asyncio
async def test_create_dataset_uses_public_dashscope_embedding_defaults(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    embedder = SimpleNamespace(_dimension=1024, close=AsyncMock())
    create_embedding = Mock(return_value=embedder)
    monkeypatch.setattr(embedding_module, "create_embedding", create_embedding)

    await service.create_dataset(
        _user(),
        {"dataset_id": "new-dataset"},
    )

    service._ks._resolve_embedding_config.assert_called_once_with(
        provider="dashscope",
        model="text-embedding-v4",
        embedding_config={},
        tenant_id="tenant-attacker",
    )
    create_embedding.assert_called_once_with(
        service._ks._resolve_embedding_config.return_value,
        dimension=1024,
    )
    vector_store.ensure_collection.assert_awaited_once()
    created_dataset = database.create_dataset_with_owner.await_args.args[0]
    assert created_dataset["embedding_provider"] == "dashscope"
    assert created_dataset["embedding_model"] == "text-embedding-v4"
    assert created_dataset["embedding_dimension"] == 1024


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "dimension",
    [0, -1, 8193, True, False, "1024", 1.5, None],
)
async def test_create_dataset_rejects_invalid_dimension_before_any_dependency(
    monkeypatch: pytest.MonkeyPatch,
    dimension: Any,
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="integer between 1 and 8192"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "embedding_dimension": dimension,
            },
        )

    database.dataset_exists.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("dimension", [0, -1, 8193, True, "1024", 1.5, None])
async def test_update_dataset_rejects_invalid_dimension_before_any_dependency(
    monkeypatch: pytest.MonkeyPatch,
    dimension: Any,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    service.require_dataset_access = AsyncMock()

    with pytest.raises(ValidationFailedError, match="integer between 1 and 8192"):
        await service.update_dataset(
            _user(),
            "dataset-a",
            {"embedding_dimension": dimension},
        )

    service.require_dataset_access.assert_not_awaited()
    database.list_documents.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("retrieval", INVALID_PERSISTED_RETRIEVAL_CONFIGS)
async def test_create_dataset_rejects_unbounded_retrieval_before_any_dependency(
    monkeypatch: pytest.MonkeyPatch,
    retrieval: dict[str, Any],
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="retrieval"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "index_config": {"retrieval": retrieval},
            },
        )

    database.dataset_exists.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("retrieval", INVALID_PERSISTED_RETRIEVAL_CONFIGS)
async def test_update_dataset_rejects_unbounded_retrieval_before_any_dependency(
    monkeypatch: pytest.MonkeyPatch,
    retrieval: dict[str, Any],
) -> None:
    service, database, vector_store = _service(monkeypatch)
    service.require_dataset_access = AsyncMock()

    with pytest.raises(ValidationFailedError, match="retrieval"):
        await service.update_dataset(
            _user(),
            "dataset-a",
            {"index_config": {"retrieval": retrieval}},
        )

    service.require_dataset_access.assert_not_awaited()
    database.list_documents.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "embedding_config",
    [
        {"base_url": "http://169.254.169.254/latest/meta-data"},
        {"nested": {"api-key": "caller-secret"}},
        {"endpoint_url": "https://attacker.invalid", "token": "caller-secret"},
    ],
)
async def test_create_dataset_rejects_caller_embedding_endpoint_or_secret_before_resolution(
    monkeypatch: pytest.MonkeyPatch,
    embedding_config: dict[str, Any],
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="server-owned"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "embedding_config": embedding_config,
            },
        )

    database.dataset_exists.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "requested",
    [
        {"embedding_provider": "unified_multimodal"},
        {"embedding_model": "multimodal-embedding-v1"},
        {"index_config": {"multimodal_enabled": True}},
    ],
)
async def test_create_dataset_rejects_multimodal_profile_before_resolution(
    monkeypatch: pytest.MonkeyPatch,
    requested: dict[str, Any],
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="multimodal datasets are disabled"):
        await service.create_dataset(
            _user(),
            {"dataset_id": "new-dataset", **requested},
        )

    database.dataset_exists.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "rerank",
    [
        {"api_key": "caller-secret"},
        {"provider": {"endpoint_url": "https://attacker.invalid"}},
        {"transport": {"request_headers": {"Authorization": "Bearer secret"}}},
    ],
)
async def test_create_dataset_rejects_nested_rerank_transport_config(
    monkeypatch: pytest.MonkeyPatch,
    rerank: dict[str, Any],
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="rerank.*server-owned"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "index_config": {"retrieval": {"rerank": rerank}},
            },
        )

    database.dataset_exists.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
async def test_update_dataset_rejects_caller_endpoint_before_resolution(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    service.require_dataset_access = AsyncMock()

    with pytest.raises(ValidationFailedError, match="server-owned"):
        await service.update_dataset(
            _user(),
            "dataset-a",
            {"embedding_config": {"api_base": "https://attacker.invalid"}},
        )

    service.require_dataset_access.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    database.list_documents.assert_not_awaited()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
async def test_update_dataset_rejects_multimodal_profile_before_index_mutation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    existing = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-attacker",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
        "collection_name": "collection-a",
    }
    service.require_dataset_access = AsyncMock(return_value=existing)

    with pytest.raises(ValidationFailedError, match="multimodal datasets are disabled"):
        await service.update_dataset(
            _user(),
            "dataset-a",
            {"embedding_provider": "dashscope_multimodal"},
        )

    database.list_documents.assert_not_awaited()
    service._ks._resolve_embedding_config.assert_not_called()
    vector_store.ensure_collection.assert_not_awaited()


@pytest.mark.asyncio
async def test_update_dataset_rejects_nested_rerank_secret_before_access(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    service.require_dataset_access = AsyncMock()

    with pytest.raises(ValidationFailedError, match="rerank.*server-owned"):
        await service.update_dataset(
            _user(),
            "dataset-a",
            {
                "index_config": {
                    "retrieval": {
                        "rerank": {
                            "fallbacks": [
                                {"openai_api_key": "caller-secret"}
                            ]
                        }
                    }
                }
            },
        )

    service.require_dataset_access.assert_not_awaited()
    database.list_documents.assert_not_awaited()
    vector_store.ensure_collection.assert_not_awaited()


def test_dataset_response_redaction_is_recursive_and_does_not_mutate_source() -> None:
    raw = {
        "dataset_id": "dataset-a",
        "embedding_config": {
            "provider": {
                "openai_api_key": "embedding-secret",
                "base_url": (
                    "https://user:password@embedding.example/v1?token=query-secret"
                ),
            }
        },
        "index_config": {
            "retrieval": {
                "rerank": {
                    "fallbacks": [
                        {
                            "provider_secret_key": "rerank-secret",
                            "endpoint_url": (
                                "https://user:password@rerank.example/v2?key=query-secret"
                            ),
                        }
                    ],
                    "request_headers": {"Authorization": "Bearer secret"},
                }
            }
        },
    }
    source_snapshot = deepcopy(raw)
    service = object.__new__(DatasetService)

    sanitized = service.sanitize_dataset_for_response(raw)

    assert raw == source_snapshot
    provider = sanitized["embedding_config"]["provider"]
    assert provider["openai_api_key"] == "*****"
    assert provider["base_url"] == "*****"
    rerank = sanitized["index_config"]["retrieval"]["rerank"]
    assert rerank["fallbacks"][0]["provider_secret_key"] == "*****"
    assert rerank["fallbacks"][0]["endpoint_url"] == "*****"
    assert rerank["request_headers"] == "*****"

    sanitized["embedding_config"]["provider"]["openai_api_key"] = "changed"
    assert raw == source_snapshot


@pytest.mark.asyncio
async def test_create_dataset_rejects_existing_id_before_touching_qdrant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    database.dataset_exists.return_value = True

    with pytest.raises(ValidationFailedError, match="dataset_id already exists"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "victim-dataset",
                "name": "takeover attempt",
                "collection_name": "victim-collection",
            },
        )

    vector_store.ensure_collection.assert_not_awaited()
    database.create_dataset_with_owner.assert_not_awaited()
    database.save_dataset.assert_not_awaited()


@pytest.mark.asyncio
async def test_create_dataset_rejects_collection_bound_to_another_dataset(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    database.collection_name_in_use.return_value = True

    with pytest.raises(ValidationFailedError, match="collection_name already in use"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "name": "collection reuse attempt",
                "collection_name": "victim-collection",
                "embedding_provider": "local",
                "embedding_model": "hash-384",
                "embedding_dimension": 3,
            },
        )

    database.collection_name_in_use.assert_awaited_once_with("victim-collection")
    vector_store.ensure_collection.assert_not_awaited()
    database.create_dataset_with_owner.assert_not_awaited()


@pytest.mark.asyncio
async def test_create_dataset_claims_a_new_collection_and_uses_insert_only(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)

    created = await service.create_dataset(
        _user(),
        {
            "dataset_id": "new-dataset",
            "name": "legitimate dataset",
            "collection_name": "new-collection",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
        },
    )

    assert created["dataset_id"] == "new-dataset"
    vector_store.ensure_collection.assert_awaited_once_with(
        dataset_id="new-dataset",
        dimension=3,
        collection_name="new-collection",
        allow_existing=False,
        tenant_id="tenant-attacker",
        allow_lexical_transition=False,
        bootstrap_unbound_dataset=True,
    )
    database.create_dataset_with_owner.assert_awaited_once()
    create_args = database.create_dataset_with_owner.await_args.args
    assert create_args[0]["dataset_id"] == "new-dataset"
    assert create_args[1] == "user-attacker"
    database.create_dataset.assert_not_awaited()
    database.save_dataset.assert_not_awaited()
    database.grant_dataset_permission.assert_not_awaited()


@pytest.mark.asyncio
async def test_create_dataset_rejects_reserved_deletion_marker(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)

    with pytest.raises(ValidationFailedError, match="reserved lifecycle field"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "index_config": {
                    "retrieval": {
                        "_index_deletion_fence": make_dataset_index_deletion_fence(
                            "dataset_delete",
                            "new-dataset",
                        )
                    }
                },
            },
        )

    vector_store.ensure_collection.assert_not_awaited()
    database.create_dataset_with_owner.assert_not_awaited()


@pytest.mark.asyncio
async def test_create_dataset_rejects_a_concurrent_database_identity_claim(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    database.create_dataset_with_owner.return_value = False

    with pytest.raises(ValidationFailedError, match="dataset_id already exists"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "name": "concurrent loser",
                "collection_name": "new-collection",
                "embedding_provider": "local",
                "embedding_model": "hash-384",
                "embedding_dimension": 3,
            },
        )

    database.grant_dataset_permission.assert_not_awaited()
    vector_store.delete_collection.assert_awaited_once_with("new-collection")


@pytest.mark.asyncio
async def test_create_dataset_does_not_delete_a_collection_claimed_in_database_race(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, database, vector_store = _service(monkeypatch)
    database.collection_name_in_use.side_effect = [False, True]
    database.create_dataset_with_owner.return_value = False

    with pytest.raises(ValidationFailedError, match="dataset_id already exists"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "name": "collection race loser",
                "collection_name": "new-collection",
                "embedding_provider": "local",
                "embedding_model": "hash-384",
                "embedding_dimension": 3,
            },
        )

    vector_store.delete_collection.assert_not_awaited()


@pytest.mark.asyncio
async def test_create_dataset_compensates_when_owner_transaction_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _database, vector_store = _service(monkeypatch)
    service.db.create_dataset_with_owner.side_effect = RuntimeError(
        "owner ACL insert failed"
    )

    with pytest.raises(RuntimeError, match="owner ACL insert failed"):
        await service.create_dataset(
            _user(),
            {
                "dataset_id": "new-dataset",
                "name": "failed insert",
                "collection_name": "new-collection",
                "embedding_provider": "local",
                "embedding_model": "hash-384",
                "embedding_dimension": 3,
            },
        )

    vector_store.delete_collection.assert_awaited_once_with("new-collection")


class _Acquire:
    def __init__(self, connection: AsyncMock):
        self.connection = connection

    async def __aenter__(self) -> AsyncMock:
        return self.connection

    async def __aexit__(self, *_args) -> None:
        return None


class _Pool:
    def __init__(self, connection: AsyncMock):
        self.connection = connection

    def acquire(self) -> _Acquire:
        return _Acquire(self.connection)


class _Transaction:
    def __init__(self) -> None:
        self.entered = False
        self.exit_exception: type[BaseException] | None = None

    async def __aenter__(self) -> _Transaction:
        self.entered = True
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        _exc: BaseException | None,
        _traceback: object,
    ) -> None:
        self.exit_exception = exc_type


@pytest.mark.asyncio
async def test_database_create_dataset_is_insert_only() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {"dataset_id": "new-dataset"}
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    created = await storage.create_dataset(
        {
            "dataset_id": "new-dataset",
            "name": "New dataset",
            "description": "",
            "tenant_id": "tenant-a",
            "visibility": "private",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
            "embedding_config": {},
            "index_config": {},
            "collection_name": "new-collection",
            "created_by": "user-a",
        }
    )

    assert created is True
    query = connection.fetchrow.await_args.args[0]
    assert "INSERT INTO datasets" in query
    assert "ON CONFLICT" not in query


@pytest.mark.asyncio
async def test_database_create_dataset_reports_a_concurrent_identity_conflict() -> None:
    connection = AsyncMock()
    connection.fetchrow.side_effect = database_module.asyncpg.UniqueViolationError(
        "duplicate dataset identity"
    )
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    created = await storage.create_dataset(
        {
            "dataset_id": "claimed-dataset",
            "name": "Concurrent request",
            "tenant_id": "tenant-b",
            "collection_name": "claimed-collection",
        }
    )

    assert created is False


@pytest.mark.asyncio
async def test_database_name_patch_does_not_compare_or_write_retrieval_config() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {
        "dataset_id": "dataset-a",
        "name": "Renamed",
        "index_config": '{"retrieval":{"lexical":{"active_version":"lexical_v1"}}}',
    }
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    updated = await storage.patch_dataset_fields(
        "dataset-a",
        {"name": "Renamed"},
    )

    query, *values = connection.fetchrow.await_args.args
    assert "SET name = $2" in query
    assert "index_config =" not in query
    assert "IS NOT DISTINCT FROM" not in query
    assert values == ["dataset-a", "Renamed"]
    assert updated is not None
    assert updated["index_config"]["retrieval"]["lexical"]["active_version"] == (
        "lexical_v1"
    )


@pytest.mark.asyncio
async def test_database_config_patch_uses_complete_retrieval_config_cas() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = None
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)
    replacement = {"retrieval": {"lexical": {"active_version": "lexical_v1"}}}
    expected = {
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
        "collection_name": "collection-a",
    }

    updated = await storage.patch_dataset_fields(
        "dataset-a",
        {"index_config": replacement},
        expected_config=expected,
    )

    query, *values = connection.fetchrow.await_args.args
    assert "index_config = $2::jsonb" in query
    for field_name in expected:
        assert f"{field_name} IS NOT DISTINCT FROM" in query
    assert values[0] == "dataset-a"
    assert json.loads(values[1]) == replacement
    assert values[2:5] == ["local", "hash-384", 384]
    assert json.loads(values[5]) == {}
    assert json.loads(values[6]) == {}
    assert values[7] == "collection-a"
    assert updated is None


@pytest.mark.asyncio
async def test_identity_patch_holds_exclusive_lock_and_requires_empty_dataset() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = None
    transaction = _Transaction()
    connection.transaction = Mock(return_value=transaction)
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)
    expected = {
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
        "collection_name": "collection-a",
    }

    updated = await storage.patch_dataset_fields(
        "dataset-a",
        {"embedding_model": "replacement-model"},
        expected_config=expected,
        require_no_documents=True,
    )

    assert updated is None
    assert transaction.entered is True
    lock_query, lock_name = connection.fetchval.await_args.args
    assert "pg_advisory_xact_lock(" in lock_query
    assert lock_name == "knowledge-dataset-index:dataset-a"
    patch_query = connection.fetchrow.await_args.args[0]
    assert "NOT EXISTS (SELECT 1 FROM documents" in patch_query
    assert "documents.dataset_id = datasets.dataset_id" in patch_query


@pytest.mark.asyncio
async def test_fenced_document_save_shares_identity_lock_with_config_patch() -> None:
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {"chunking": {"chunk_size": 400}},
    }
    connection = AsyncMock()
    connection.fetchrow.return_value = dataset
    transaction = _Transaction()
    connection.transaction = Mock(return_value=transaction)
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    await storage.save_document(
        {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "title": "Document A",
        },
        expected_ingestion_identity=dataset_ingestion_identity(dataset),
    )

    lock_query, lock_name = connection.fetchval.await_args.args
    assert "pg_advisory_xact_lock_shared" in lock_query
    assert lock_name == "knowledge-dataset-index:dataset-a"
    identity_query = connection.fetchrow.await_args.args[0]
    assert "FROM datasets" in identity_query
    assert "is_deleted = FALSE" in identity_query
    assert "INSERT INTO documents" in connection.execute.await_args.args[0]
    assert transaction.entered is True


@pytest.mark.asyncio
async def test_legacy_document_save_still_serializes_on_shared_dataset_lock() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {"dataset_id": "dataset-a"}
    transaction = _Transaction()
    connection.transaction = Mock(return_value=transaction)
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    await storage.save_document(
        {
            "document_id": "document-a",
            "dataset_id": "dataset-a",
            "title": "Document A",
        }
    )

    lock_query, lock_name = connection.fetchval.await_args.args
    assert "pg_advisory_xact_lock_shared" in lock_query
    assert lock_name == "knowledge-dataset-index:dataset-a"
    active_query = connection.fetchrow.await_args.args[0]
    assert "FROM datasets" in active_query
    assert "is_deleted = FALSE" in active_query
    assert "INSERT INTO documents" in connection.execute.await_args.args[0]
    assert transaction.entered is True


@pytest.mark.asyncio
async def test_document_save_queued_after_dataset_delete_cannot_insert() -> None:
    events: list[str] = []
    connection = AsyncMock()

    async def fetchval(query: str, *_args):
        assert "pg_advisory_xact_lock_shared" in query
        events.append("lock")

    async def fetchrow(query: str, *_args):
        assert "is_deleted = FALSE" in query
        events.append("active-dataset-check")
        return None

    async def execute(*_args):
        events.append("insert")

    connection.fetchval.side_effect = fetchval
    connection.fetchrow.side_effect = fetchrow
    connection.execute.side_effect = execute
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="dataset was deleted"):
        await storage.save_document(
            {
                "document_id": "document-a",
                "dataset_id": "dataset-a",
                "title": "Document A",
            }
        )

    assert events == ["lock", "active-dataset-check"]


@pytest.mark.asyncio
async def test_fenced_document_save_rejects_stale_identity_before_insert() -> None:
    current = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "replacement-model",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
    }
    stale = dict(current, embedding_model="hash-384")
    connection = AsyncMock()
    connection.fetchrow.return_value = current
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="mixed index generation"):
        await storage.save_document(
            {
                "document_id": "document-a",
                "dataset_id": "dataset-a",
                "title": "Document A",
            },
            expected_ingestion_identity=dataset_ingestion_identity(stale),
        )

    connection.execute.assert_not_awaited()


@pytest.mark.asyncio
async def test_dataset_service_marks_ingestion_identity_patch_as_empty_only() -> None:
    original = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {
            "chunking": {"chunk_size": 400},
            "retrieval": {"top_k": 10},
        },
    }

    class Database:
        def __init__(self) -> None:
            self.require_no_documents: bool | None = None

        async def list_documents(self, **_kwargs):
            return []

        async def patch_dataset_fields(
            self,
            _dataset_id: str,
            _changes: dict,
            *,
            expected_config: dict | None = None,
            require_no_documents: bool = False,
        ):
            assert expected_config is not None
            self.require_no_documents = require_no_documents
            return None

        async def get_dataset(self, _dataset_id: str):
            return dict(original)

    database = Database()
    vector_store = AsyncMock()
    vector_store.ensure_collection.return_value = "collection-a"
    service = object.__new__(DatasetService)
    service.db = database
    service._ks = SimpleNamespace(vector_store=vector_store)
    service._transition_locks = {}

    async def require_dataset_access(*_args, **_kwargs):
        return dict(original)

    service.require_dataset_access = require_dataset_access

    with pytest.raises(ValidationFailedError, match="changed concurrently"):
        await service.update_dataset(
            _user(tenant_id="tenant-a"),
            "dataset-a",
            {
                "index_config": {
                    "chunking": {"chunk_size": 800},
                    "retrieval": {"top_k": 10},
                }
            },
        )

    assert database.require_no_documents is True


@pytest.mark.asyncio
async def test_dataset_index_leases_share_one_advisory_lock_namespace() -> None:
    write_connection = AsyncMock()
    write_connection.fetchval.side_effect = [True, 1]
    write_connection.fetchrow.return_value = {
        "dataset_id": "dataset-a",
        "index_config": {},
    }
    write_connection.transaction = Mock(return_value=_Transaction())
    write_storage = object.__new__(DatabaseStorage)
    write_storage._pool = _Pool(write_connection)

    async with write_storage.dataset_index_write_lease(
        "dataset-a",
        ["document-a"],
    ):
        pass

    write_lock_call = write_connection.fetchval.await_args_list[0]
    assert "pg_try_advisory_xact_lock_shared" in write_lock_call.args[0]
    assert write_lock_call.args[1] == "knowledge-dataset-index:dataset-a"
    document_check = write_connection.fetchval.await_args_list[1]
    assert "FROM documents" in document_check.args[0]
    assert document_check.args[1:] == ("dataset-a", ["document-a"])

    delete_connection = AsyncMock()
    delete_connection.fetchval.side_effect = [True, True]
    delete_connection.transaction = Mock(return_value=_Transaction())
    delete_storage = object.__new__(DatabaseStorage)
    delete_storage._pool = _Pool(delete_connection)

    async with delete_storage.dataset_index_delete_lease("dataset-a"):
        pass

    delete_lock_call = delete_connection.fetchval.await_args_list[0]
    assert "pg_try_advisory_lock(" in delete_lock_call.args[0]
    assert delete_lock_call.args[1] == write_lock_call.args[1]
    assert "pg_advisory_unlock(" in delete_connection.fetchval.await_args_list[1].args[0]


@pytest.mark.asyncio
async def test_dataset_write_lease_rejects_deleted_document() -> None:
    connection = AsyncMock()
    connection.fetchval.side_effect = [True, 0]
    connection.fetchrow.return_value = {
        "dataset_id": "dataset-a",
        "index_config": {},
    }
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="refusing orphan or disabled points"):
        async with storage.dataset_index_write_lease(
            "dataset-a",
            ["deleted-document"],
        ):
            raise AssertionError("deleted document must not reach Qdrant")


@pytest.mark.asyncio
async def test_dataset_write_lease_checks_identity_after_lock_before_yield() -> None:
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
    }
    events: list[str] = []
    connection = AsyncMock()

    async def fetchval(query: str, *_args):
        if "try_advisory_xact_lock_shared" in query:
            events.append("lock")
            return True
        if "COUNT(*)" in query:
            events.append("documents")
            return 1
        raise AssertionError(query)

    async def fetchrow(query: str, *_args):
        assert "FROM datasets" in query
        events.append("identity")
        return dataset

    connection.fetchval.side_effect = fetchval
    connection.fetchrow.side_effect = fetchrow
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    async with storage.dataset_index_write_lease(
        "dataset-a",
        ["document-a"],
        expected_ingestion_identity=dataset_ingestion_identity(dataset),
    ):
        events.append("yield")

    assert events == ["lock", "identity", "documents", "yield"]


@pytest.mark.asyncio
async def test_dataset_write_lease_rejects_instead_of_queueing_behind_delete() -> None:
    connection = AsyncMock()
    connection.fetchval.return_value = False
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="refusing a queued vector write"):
        async with storage.dataset_index_write_lease(
            "dataset-a",
            ["document-a"],
        ):
            raise AssertionError("contended writer must not reach Qdrant")

    connection.fetchrow.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["write_lease", "save_document"])
@pytest.mark.parametrize("fence_operation", ["document_delete", "segment_delete"])
async def test_durable_deletion_marker_blocks_central_index_writes(
    operation: str,
    fence_operation: str,
) -> None:
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "index_config": {
            "retrieval": {
                "_index_deletion_fence": make_dataset_index_deletion_fence(
                    fence_operation,
                    "document-a" if fence_operation == "document_delete" else "segment-a",
                )
            }
        },
    }
    connection = AsyncMock()
    connection.fetchval.return_value = True
    connection.fetchrow.return_value = dataset
    connection.transaction = Mock(return_value=_Transaction())
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="deletion is pending"):
        if operation == "write_lease":
            async with storage.dataset_index_write_lease(
                "dataset-a",
                ["document-a"],
            ):
                raise AssertionError("pending deletion must reject before yield")
        else:
            await storage.save_document(
                {
                    "document_id": "document-a",
                    "dataset_id": "dataset-a",
                    "title": "must not persist",
                }
            )

    connection.execute.assert_not_awaited()


@pytest.mark.asyncio
async def test_deletion_fence_cas_is_target_bound_and_idempotent() -> None:
    marker = make_dataset_index_deletion_fence("document_delete", "document-a")
    fenced_dataset = {
        "dataset_id": "dataset-a",
        "index_config": {"retrieval": {"_index_deletion_fence": marker}},
    }
    storage = object.__new__(DatabaseStorage)

    create_connection = AsyncMock()
    create_connection.fetchrow.return_value = fenced_dataset
    created, was_created = await storage.set_dataset_index_deletion_fence(
        "dataset-a",
        operation="document_delete",
        target_id="document-a",
        connection=create_connection,
    )
    assert was_created is True
    assert dataset_index_deletion_fence(created) == marker
    assert "content_revision = COALESCE(content_revision, 0) + 1" in (
        create_connection.fetchrow.await_args.args[0]
    )

    retry_connection = AsyncMock()
    retry_connection.fetchrow.side_effect = [None, fenced_dataset]
    retried, was_created = await storage.set_dataset_index_deletion_fence(
        "dataset-a",
        operation="document_delete",
        target_id="document-a",
        connection=retry_connection,
    )
    assert was_created is False
    assert dataset_index_deletion_fence(retried) == marker

    foreign_connection = AsyncMock()
    foreign_connection.fetchrow.side_effect = [None, fenced_dataset]
    with pytest.raises(RuntimeError, match="another dataset index deletion target"):
        await storage.set_dataset_index_deletion_fence(
            "dataset-a",
            operation="document_delete",
            target_id="document-b",
            connection=foreign_connection,
        )


@pytest.mark.asyncio
async def test_clear_deletion_fence_is_exact_target_cas() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {"dataset_id": "dataset-a"}
    storage = object.__new__(DatabaseStorage)

    assert await storage.clear_dataset_index_deletion_fence(
        "dataset-a",
        operation="document_delete",
        target_id="document-a",
        connection=connection,
    ) is True

    query, dataset_id, marker_json = connection.fetchrow.await_args.args
    assert "content_revision = COALESCE(content_revision, 0) + 1" in query
    assert "_index_deletion_fence" in query
    assert dataset_id == "dataset-a"
    assert json.loads(marker_json) == make_dataset_index_deletion_fence(
        "document_delete",
        "document-a",
    )


@pytest.mark.asyncio
async def test_database_create_dataset_with_owner_uses_one_transaction() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {"dataset_id": "new-dataset"}
    transaction = _Transaction()
    connection.transaction = Mock(return_value=transaction)
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    created = await storage.create_dataset_with_owner(
        {
            "dataset_id": "new-dataset",
            "name": "New dataset",
            "tenant_id": "tenant-a",
            "collection_name": "new-collection",
            "created_by": "user-a",
        },
        "user-a",
    )

    assert created is True
    assert transaction.entered is True
    assert transaction.exit_exception is None
    permission_query = connection.execute.await_args.args[0]
    assert "INSERT INTO dataset_permissions" in permission_query
    assert connection.execute.await_args.args[1:] == (
        "new-dataset",
        "user-a",
    )


@pytest.mark.asyncio
async def test_database_owner_acl_failure_rolls_back_dataset_transaction() -> None:
    connection = AsyncMock()
    connection.fetchrow.return_value = {"dataset_id": "new-dataset"}
    connection.execute.side_effect = RuntimeError("owner ACL insert failed")
    transaction = _Transaction()
    connection.transaction = Mock(return_value=transaction)
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    with pytest.raises(RuntimeError, match="owner ACL insert failed"):
        await storage.create_dataset_with_owner(
            {
                "dataset_id": "new-dataset",
                "name": "New dataset",
                "tenant_id": "tenant-a",
                "collection_name": "new-collection",
                "created_by": "user-a",
            },
            "user-a",
        )

    assert transaction.entered is True
    assert transaction.exit_exception is RuntimeError


def test_collection_identity_migration_is_additive_and_nonempty_only() -> None:
    from database.migrate_per_service import _files_for

    root = Path(__file__).resolve().parents[3]
    legacy_sql = (
        root / "database" / "migrations" / "082_kb_dataset_collection_identity.sql"
    ).read_text(encoding="utf-8")
    service_sql = (
        root
        / "database"
        / "migrations"
        / "per_service"
        / "knowledge"
        / "001_dataset_collection_identity.sql"
    ).read_text(encoding="utf-8")
    normalized = " ".join(legacy_sql.upper().split())
    service_normalized = " ".join(service_sql.upper().split())

    assert "CREATE UNIQUE INDEX IF NOT EXISTS" in normalized
    assert "ON DATASETS (COLLECTION_NAME)" in normalized
    assert "COLLECTION_NAME IS NOT NULL" in normalized
    assert "BTRIM(COLLECTION_NAME) <> ''" in normalized
    assert "DELETE FROM" not in normalized
    assert "UPDATE DATASETS" not in normalized
    assert "DROP " not in normalized
    assert "CREATE UNIQUE INDEX IF NOT EXISTS" in service_normalized
    assert "ON KNOWLEDGE.DATASETS (COLLECTION_NAME)" in service_normalized
    assert "GROUP BY COLLECTION_NAME" in service_normalized
    assert "HAVING COUNT(*) > 1" in service_normalized
    assert "BEGIN;" not in service_normalized
    assert "COMMIT;" not in service_normalized
    assert "001_dataset_collection_identity.sql" in {
        path.name for path in _files_for("knowledge")
    }
    migrate_dockerfile = (root / "docker" / "migrate" / "Dockerfile").read_text(
        encoding="utf-8"
    )
    assert "COPY database /app/database" in migrate_dockerfile
    assert 'ENTRYPOINT ["python", "-m", "database.authority"]' in migrate_dockerfile


@pytest.mark.asyncio
async def test_delete_collection_rejects_a_false_result_when_collection_remains(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Client:
        async def delete_collection(self, *, collection_name: str) -> bool:
            assert collection_name == "stuck-collection"
            return False

        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name == "stuck-collection"
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)
    vector_store._collection_dims["stuck-collection"] = 3
    vector_store._sparse_collections.add("stuck-collection")
    vector_store._sparse_readiness["stuck-collection"] = True

    with pytest.raises(VectorStoreError, match="could not be deleted"):
        await vector_store.delete_collection("stuck-collection")

    assert vector_store._collection_dims["stuck-collection"] == 3
    assert "stuck-collection" in vector_store._sparse_collections
    assert vector_store._sparse_readiness["stuck-collection"] is True


@pytest.mark.asyncio
async def test_delete_collection_accepts_false_only_after_confirming_absence(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Client:
        async def delete_collection(self, *, collection_name: str) -> bool:
            assert collection_name == "already-absent"
            return False

        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name == "already-absent"
            return False

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)
    vector_store._collection_dims["already-absent"] = 3
    vector_store._sparse_collections.add("already-absent")
    vector_store._sparse_readiness["already-absent"] = True

    await vector_store.delete_collection("already-absent")

    assert "already-absent" not in vector_store._collection_dims
    assert "already-absent" not in vector_store._sparse_collections
    assert "already-absent" not in vector_store._sparse_readiness


@pytest.mark.asyncio
async def test_ensure_collection_exclusive_rejects_an_existing_collection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    create_called = False
    collection_info = SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=qmodels.VectorParams(size=3, distance=qmodels.Distance.COSINE),
                sparse_vectors={"bm25": object()},
            )
        )
    )

    class _Client:
        async def collection_exists(self, *, collection_name: str) -> bool:
            return collection_name == "victim-collection"

        async def get_collection(self, _collection_name: str) -> SimpleNamespace:
            return collection_info

        async def create_collection(self, **_kwargs) -> None:
            nonlocal create_called
            create_called = True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333")

    with pytest.raises(VectorStoreError, match="already exists"):
        await vector_store.ensure_collection(
            dataset_id="attacker-dataset",
            dimension=3,
            collection_name="victim-collection",
            allow_existing=False,
            bootstrap_unbound_dataset=True,
        )

    assert create_called is False


@pytest.mark.asyncio
async def test_ensure_collection_exclusive_creates_a_missing_collection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    create_calls: list[dict] = []

    class _Client:
        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name
            return False

        async def create_collection(self, **kwargs) -> bool:
            create_calls.append(kwargs)
            return True

        async def create_payload_index(self, **_kwargs) -> None:
            return None

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)

    actual = await vector_store.ensure_collection(
        dataset_id="new-dataset",
        dimension=3,
        collection_name="new-collection",
        allow_existing=False,
        tenant_id="tenant-a",
        bootstrap_unbound_dataset=True,
    )

    assert actual == "new-collection"
    assert len(create_calls) == 1
    assert create_calls[0]["collection_name"] == "new-collection"
    assert create_calls[0]["metadata"]["knowledge_scope"] == {
        "schema_version": 1,
        "dataset_id": "new-dataset",
        "tenant_id": "tenant-a",
    }


@pytest.mark.asyncio
async def test_ensure_collection_exclusive_fails_when_concurrent_claim_wins(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Client:
        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name
            return False

        async def create_collection(self, **_kwargs) -> bool:
            # Qdrant returns False when another request wins after our
            # collection_exists preflight.
            return False

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)

    with pytest.raises(VectorStoreError, match="could not be claimed"):
        await vector_store.ensure_collection(
            dataset_id="tenant-b-dataset",
            dimension=3,
            collection_name="contended-collection",
            allow_existing=False,
            tenant_id="tenant-b",
            bootstrap_unbound_dataset=True,
        )


@pytest.mark.asyncio
async def test_ensure_collection_idempotent_mode_reuses_a_concurrent_claim(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    collection_info = SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=qmodels.VectorParams(size=3, distance=qmodels.Distance.COSINE),
                sparse_vectors={"bm25": object()},
            ),
            metadata={
                "knowledge_scope": {
                    "schema_version": 1,
                    "dataset_id": "new-dataset",
                    "tenant_id": "tenant-a",
                }
            },
        )
    )

    class _Client:
        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name
            return False

        async def create_collection(self, **_kwargs) -> bool:
            return False

        async def get_collection(self, _collection_name: str) -> SimpleNamespace:
            return collection_info

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)

    actual = await vector_store.ensure_collection(
        dataset_id="new-dataset",
        dimension=3,
        collection_name="shared-worker-collection",
        tenant_id="tenant-a",
        bootstrap_unbound_dataset=True,
    )

    assert actual == "shared-worker-collection"


@pytest.mark.asyncio
async def test_ensure_collection_exclusive_has_one_winner_under_a_forced_race(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Client:
        def __init__(self) -> None:
            self.preflight_count = 0
            self.both_preflighted = asyncio.Event()
            self.claim_lock = asyncio.Lock()
            self.claimed = False

        async def collection_exists(self, *, collection_name: str) -> bool:
            assert collection_name == "contended-collection"
            self.preflight_count += 1
            if self.preflight_count == 2:
                self.both_preflighted.set()
            await self.both_preflighted.wait()
            return False

        async def create_collection(self, **_kwargs) -> bool:
            async with self.claim_lock:
                if self.claimed:
                    return False
                self.claimed = True
                return True

        async def create_payload_index(self, **_kwargs) -> None:
            return None

        async def close(self) -> None:
            return None

    client = _Client()
    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: client,
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)

    results = await asyncio.gather(
        vector_store.ensure_collection(
            dataset_id="tenant-a-dataset",
            dimension=3,
            collection_name="contended-collection",
            allow_existing=False,
            tenant_id="tenant-a",
            bootstrap_unbound_dataset=True,
        ),
        vector_store.ensure_collection(
            dataset_id="tenant-b-dataset",
            dimension=3,
            collection_name="contended-collection",
            allow_existing=False,
            tenant_id="tenant-b",
            bootstrap_unbound_dataset=True,
        ),
        return_exceptions=True,
    )

    assert results.count("contended-collection") == 1
    errors = [result for result in results if isinstance(result, VectorStoreError)]
    assert len(errors) == 1
    assert "could not be claimed" in str(errors[0])


@pytest.mark.asyncio
async def test_ensure_collection_exclusive_fails_closed_on_preflight_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    create_called = False

    class _Client:
        async def collection_exists(self, *, collection_name: str) -> bool:
            raise RuntimeError(f"cannot inspect {collection_name}")

        async def create_collection(self, **_kwargs) -> bool:
            nonlocal create_called
            create_called = True
            return True

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        vector_store_module,
        "AsyncQdrantClient",
        lambda **_kwargs: _Client(),
    )
    vector_store = VectorStore(url="http://localhost:6333", max_retries=1)

    with pytest.raises(VectorStoreError, match="cannot inspect"):
        await vector_store.ensure_collection(
            dataset_id="new-dataset",
            dimension=3,
            collection_name="uncertain-collection",
            allow_existing=False,
            bootstrap_unbound_dataset=True,
        )

    assert create_called is False
