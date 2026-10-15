from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.auth.user_context import UserContext
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService

USER = UserContext(user_id="user-a", tenant_id="tenant-a")
DATASET = {
    "dataset_id": "dataset-a",
    "tenant_id": "tenant-a",
    "created_by": "user-a",
    "visibility": "private",
    "content_revision": 4,
    "index_config": {},
}


class AggregateDatabase:
    async def get_datasets_statistics_batch(
        self, dataset_ids: list[str]
    ) -> dict[str, dict[str, int]]:
        assert dataset_ids == ["dataset-a"]
        return {
            "dataset-a": {
                "document_count": 2,
                "available_document_count": 1,
                "segment_count": 60_001,
                "available_segment_count": 60_000,
                "word_count": 500,
                "hit_count": 75_123,
            }
        }

    async def get_document_statistics_aggregate(
        self, dataset_id: str, document_id: str
    ) -> dict[str, Any]:
        assert (dataset_id, document_id) == ("dataset-a", "document-a")
        return {
            "document_id": document_id,
            "segment_count": 12_345,
            "word_count": 100,
            "hit_count": 22_222,
            "status": "completed",
            "enabled": True,
            "archived": False,
        }


class StatisticsService:
    db = AggregateDatabase()

    async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return dict(DATASET)


@pytest.mark.asyncio
async def test_dataset_statistics_use_uncapped_database_aggregate() -> None:
    service = StatisticsService()
    result = await KnowledgeService.get_dataset_statistics(  # type: ignore[arg-type]
        service, USER, "dataset-a"
    )
    assert result["segment_count"] == 60_001
    assert result["hit_count"] == 75_123


@pytest.mark.asyncio
async def test_document_statistics_use_uncapped_database_aggregate() -> None:
    service = StatisticsService()
    result = await KnowledgeService.get_document_statistics(  # type: ignore[arg-type]
        service, USER, "dataset-a", "document-a"
    )
    assert result["segment_count"] == 12_345
    assert result["hit_count"] == 22_222


class DatasetListDatabase(AggregateDatabase):
    async def list_datasets(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return [dict(DATASET)]

    async def get_dataset_permission(self, *_args: Any) -> None:
        return None


@pytest.mark.asyncio
async def test_dataset_list_projects_aggregated_hit_count_instead_of_zero() -> None:
    database = DatasetListDatabase()
    service = DatasetService(SimpleNamespace(), database)  # type: ignore[arg-type]
    rows = await service.list_datasets(USER)
    assert rows[0]["statistics"]["hit_count"] == 75_123
    assert rows[0]["statistics"]["available_segment_count"] == 60_000
