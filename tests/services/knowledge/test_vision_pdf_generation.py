"""Scanned-page generation keeps stable identities and old rows on failure."""

from __future__ import annotations

from typing import Any

import pytest
from knowledge_service.services.knowledge.vision_pdf_processor import (
    VisionPDFProcessor,
)


class Embedder:
    def __init__(self, *, fail: bool = False) -> None:
        self.fail = fail

    async def embed_images(self, images: list[bytes]) -> list[list[float]]:
        if self.fail:
            raise RuntimeError("injected embed failure")
        return [[0.1, 0.2] for _ in images]


class VectorStore:
    def __init__(self) -> None:
        self.points: list[Any] = []

    async def upsert(self, *, collection_name: str, points: list[Any]) -> None:
        del collection_name
        by_id = {str(point.id): point for point in self.points}
        by_id.update({str(point.id): point for point in points})
        self.points = list(by_id.values())

    async def snapshot_points(
        self,
        collection_name: str,
        point_ids: list[str],
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any]:
        del collection_name, tenant_id, dataset_id
        requested = set(point_ids)
        return {
            str(point.id): point
            for point in self.points
            if str(point.id) in requested
        }

    async def delete_points(
        self,
        collection_name: str,
        point_ids: list[str],
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> None:
        del collection_name, tenant_id, dataset_id
        deleted = set(point_ids)
        self.points = [
            point for point in self.points if str(point.id) not in deleted
        ]


class Database:
    def __init__(self) -> None:
        self.rows: dict[str, dict[str, Any]] = {}
        self.fail_store_once = False

    async def get_image_segments_by_document(
        self,
        document_id: str,
    ) -> list[dict[str, Any]]:
        return [
            dict(row)
            for row in self.rows.values()
            if row["document_id"] == document_id
        ]

    async def store_image_segments(self, rows: list[dict[str, Any]]) -> None:
        if self.fail_store_once:
            self.fail_store_once = False
            raise RuntimeError("injected row publication failure")
        for row in rows:
            self.rows[str(row["segment_id"])] = dict(row)

    async def delete_segment(self, segment_id: str) -> None:
        self.rows.pop(segment_id, None)


def _processor(
    database: Database,
    vector_store: VectorStore,
    *,
    fail: bool = False,
) -> VisionPDFProcessor:
    processor = VisionPDFProcessor(
        Embedder(fail=fail),
        vector_store,
        database,
        batch_size=2,
    )
    processor._pdf_page_count = lambda _data: 2  # type: ignore[method-assign]
    processor._render_page_batch = (  # type: ignore[method-assign]
        lambda _data, start, stop: [
            (page, f"page-{page}".encode(), (100, 200))
            for page in range(start, stop)
        ]
    )
    return processor


@pytest.mark.asyncio
async def test_scanned_reprocess_reuses_page_segment_and_point_ids() -> None:
    database = Database()
    vector_store = VectorStore()

    first = await _processor(database, vector_store).process(
        b"pdf",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )
    first_ids = list(first.segment_ids or [])
    second = await _processor(database, vector_store).process(
        b"pdf-v2",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )

    assert first.success and second.success
    assert first_ids == second.segment_ids
    assert len(first_ids) == 2
    assert {point.payload["tenant_id"] for point in vector_store.points} == {
        "tenant-a"
    }
    assert set(database.rows) == set(first_ids)


@pytest.mark.asyncio
async def test_scanned_embed_failure_leaves_previous_rows_untouched() -> None:
    database = Database()
    vector_store = VectorStore()
    seeded = await _processor(database, vector_store).process(
        b"pdf",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )
    before = {key: dict(value) for key, value in database.rows.items()}

    failed = await _processor(
        database,
        vector_store,
        fail=True,
    ).process(
        b"pdf-v2",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )

    assert seeded.success and not failed.success
    assert database.rows == before


@pytest.mark.asyncio
async def test_scanned_row_failure_restores_previous_points_and_rows() -> None:
    database = Database()
    vector_store = VectorStore()
    seeded = await _processor(database, vector_store).process(
        b"pdf",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )
    before_rows = {key: dict(value) for key, value in database.rows.items()}
    before_points = {
        str(point.id): point.model_dump() for point in vector_store.points
    }
    database.fail_store_once = True

    failed = await _processor(database, vector_store).process(
        b"pdf-v2",
        "document-a",
        "dataset-a",
        "collection-a",
        tenant_id="tenant-a",
    )

    assert seeded.success and not failed.success
    assert "injected row publication failure" in str(failed.error)
    assert database.rows == before_rows
    assert {
        str(point.id): point.model_dump() for point in vector_store.points
    } == before_points
