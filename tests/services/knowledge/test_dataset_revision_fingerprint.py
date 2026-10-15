from __future__ import annotations

import copy
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from knowledge_service.persistence.database import DatabaseStorage
from knowledge_service.services.knowledge.dataset_service import (
    DatasetService,
    _dataset_revision_fingerprint,
)


class _Database:
    def __init__(self) -> None:
        self.content_revision = 7
        self.index_config = {
            "retrieval": {
                "mode": "hybrid",
                "score_threshold": 0.2,
                "rerank": {"enabled": False, "api_key": "secret-a"},
            }
        }
        self.embedding_config = {
            "api_key": "embedding-secret-a",
            "base_url": (
                "https://user-a:embedded-secret-a@embedding.example/v1?api_key=query-secret-a"
            ),
        }

    async def list_datasets(self, **_values):
        return [
            {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "name": "Dataset A",
                "visibility": "private",
                "created_by": "user-a",
                "updated_at": "2026-07-19T00:00:00Z",
                "content_revision": self.content_revision,
                "embedding_provider": "dashscope",
                "embedding_model": "text-embedding-v3",
                "embedding_dimension": 1024,
                "needs_reindex": False,
                "collection_name": "dataset-a",
                "index_config": self.index_config,
                "embedding_config": self.embedding_config,
            }
        ]

    async def get_datasets_statistics_batch(self, _dataset_ids):
        return {"dataset-a": {"document_count": 2, "segment_count": 8}}


@pytest.mark.asyncio
async def test_catalog_fingerprint_changes_with_authoritative_content_revision() -> None:
    database = _Database()
    service = object.__new__(DatasetService)
    service.db = database
    service._effective_dataset_permission = AsyncMock(return_value="owner")
    user = SimpleNamespace(
        tenant_id="tenant-a",
        user_id="user-a",
        tier="normal",
        roles=["user"],
    )

    first = await service.list_datasets(user)
    database.content_revision += 1
    second = await service.list_datasets(user)

    assert first[0]["updated_at"] == second[0]["updated_at"]
    assert first[0]["statistics"] == second[0]["statistics"]
    assert first[0]["revision_fingerprint"].startswith("sha256:")
    assert first[0]["revision_fingerprint"] != second[0]["revision_fingerprint"]


@pytest.mark.asyncio
async def test_catalog_fingerprint_changes_with_retrieval_effective_config() -> None:
    database = _Database()
    service = object.__new__(DatasetService)
    service.db = database
    service._effective_dataset_permission = AsyncMock(return_value="owner")
    user = SimpleNamespace(
        tenant_id="tenant-a",
        user_id="user-a",
        tier="normal",
        roles=["user"],
    )

    first = await service.list_datasets(user)
    database.index_config = {
        "retrieval": {
            "mode": "bm25",
            "score_threshold": 0.9,
            "rerank": {"enabled": True, "provider": "bge", "api_key": "secret-b"},
        }
    }
    second = await service.list_datasets(user)

    assert first[0]["content_revision"] == second[0]["content_revision"]
    assert first[0]["updated_at"] == second[0]["updated_at"]
    assert first[0]["revision_fingerprint"] != second[0]["revision_fingerprint"]


@pytest.mark.asyncio
async def test_catalog_fingerprint_excludes_credential_values() -> None:
    database = _Database()
    service = object.__new__(DatasetService)
    service.db = database
    service._effective_dataset_permission = AsyncMock(return_value="owner")
    user = SimpleNamespace(
        tenant_id="tenant-a",
        user_id="user-a",
        tier="normal",
        roles=["user"],
    )

    first = await service.list_datasets(user)
    assert database.index_config["retrieval"]["rerank"]["api_key"] == "secret-a"
    assert database.embedding_config["api_key"] == "embedding-secret-a"
    database.index_config["retrieval"]["rerank"]["api_key"] = "rotated-secret"
    database.embedding_config["api_key"] = "rotated-embedding-secret"
    database.embedding_config["base_url"] = (
        "https://user-b:embedded-secret-b@embedding.example/v1?token=query-secret-b"
    )
    second = await service.list_datasets(user)

    assert first[0]["revision_fingerprint"] == second[0]["revision_fingerprint"]
    assert first[0]["index_config"]["retrieval"]["rerank"]["api_key"] == "*****"
    assert second[0]["embedding_config"]["api_key"] == "*****"
    assert first[0]["embedding_config"]["base_url"] == "*****"
    assert second[0]["embedding_config"]["base_url"] == "*****"

    database.embedding_config["base_url"] = "https://embedding-alt.example/v2"
    third = await service.list_datasets(user)
    assert second[0]["revision_fingerprint"] != third[0]["revision_fingerprint"]


@pytest.mark.asyncio
async def test_catalog_omits_fingerprint_without_authoritative_revision() -> None:
    database = _Database()
    database.content_revision = None  # type: ignore[assignment]
    service = object.__new__(DatasetService)
    service.db = database
    service._effective_dataset_permission = AsyncMock(return_value="owner")
    user = SimpleNamespace(
        tenant_id="tenant-a",
        user_id="user-a",
        tier="normal",
        roles=["user"],
    )

    datasets = await service.list_datasets(user)

    assert "revision_fingerprint" not in datasets[0]


def test_fingerprint_includes_nested_fusion_rrf_weights() -> None:
    dataset = {
        "dataset_id": "dataset-a",
        "content_revision": 7,
        "index_config": {
            "retrieval": {
                "mode": "hybrid",
                "fusion": {
                    "strategy": "rrf",
                    "rrf_k": 60,
                    "rrf_weights": {"vector": 0.8, "keyword": 0.2},
                },
            }
        },
    }
    changed = copy.deepcopy(dataset)
    changed["index_config"]["retrieval"]["fusion"]["rrf_weights"] = {
        "vector": 0.2,
        "keyword": 0.8,
    }

    assert _dataset_revision_fingerprint(dataset) != _dataset_revision_fingerprint(changed)


def test_fingerprint_includes_versioned_lexical_profile() -> None:
    dataset = {
        "dataset_id": "dataset-a",
        "content_revision": 7,
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
    changed = copy.deepcopy(dataset)
    changed["index_config"]["retrieval"]["lexical"]["bm25_v2"]["avg_len"] = 384

    assert _dataset_revision_fingerprint(dataset) != _dataset_revision_fingerprint(changed)


class _Acquire:
    def __init__(self, connection):
        self.connection = connection

    async def __aenter__(self):
        return self.connection

    async def __aexit__(self, *_args):
        return None


class _Pool:
    def __init__(self, connection):
        self.connection = connection

    def acquire(self):
        return _Acquire(self.connection)


@pytest.mark.asyncio
async def test_successful_reindex_advances_authoritative_content_revision() -> None:
    connection = AsyncMock()
    storage = object.__new__(DatabaseStorage)
    storage._pool = _Pool(connection)

    await storage.clear_dataset_needs_reindex("dataset-a")

    query, dataset_id = connection.execute.await_args.args
    assert "content_revision = content_revision + 1" in query
    assert dataset_id == "dataset-a"
