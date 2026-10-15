"""T4 runtime wiring: durable IR reuse, exact boundaries, page-cache resume."""

from __future__ import annotations

from typing import Any

import pytest
from knowledge_service.services.knowledge.ingestion_service import IngestionService


class ArtifactDatabase:
    def __init__(self, *, fail_ir_store_once: bool = False) -> None:
        self.document_ir: dict[str, Any] | None = None
        self.page_rows: dict[str, dict[str, Any]] = {}
        self.ir_loads = 0
        self.ir_stores = 0
        self.page_loads = 0
        self.page_stores = 0
        self.fail_ir_store_once = fail_ir_store_once

    async def load_parsing_ir(self, **_scope: Any) -> dict[str, Any] | None:
        self.ir_loads += 1
        return self.document_ir

    async def store_parsing_ir(self, **values: Any) -> bool:
        self.ir_stores += 1
        if self.fail_ir_store_once:
            self.fail_ir_store_once = False
            raise RuntimeError("injected document IR publication failure")
        self.document_ir = {"ir": values["ir"]}
        return True

    async def load_parsing_page_cache(
        self,
        *,
        cache_key: str,
        **_scope: Any,
    ) -> dict[str, Any] | None:
        self.page_loads += 1
        return self.page_rows.get(cache_key)

    async def store_parsing_page_cache(
        self,
        *,
        cache_key: str,
        page_ir: dict[str, Any],
        backend: str,
        backend_version: str,
        content_hash: str,
        page_number: int,
        confidence: float | None,
        hard_page: bool,
        **_scope: Any,
    ) -> bool:
        self.page_stores += 1
        self.page_rows[cache_key] = {
            "page_ir": page_ir,
            "backend": backend,
            "backend_version": backend_version,
            "content_hash": content_hash,
            "page_number": page_number,
            "confidence": confidence,
            "hard_page": hard_page,
        }
        return True


def _service(database: ArtifactDatabase) -> IngestionService:
    service = object.__new__(IngestionService)
    service.db = database
    return service


DATASET = {"dataset_id": "dataset-a", "tenant_id": "tenant-a"}
DOCUMENT = {
    "document_id": "document-a",
    "current_version": 4,
    "title": "source.txt",
    "mime_type": "text/plain",
}
ENABLED = {
    "parsing": {
        "enabled": True,
        "cascade": {
            "stages": [
                {
                    "backend": "text_layer",
                    "require_text_layer": True,
                }
            ]
        },
    }
}


@pytest.mark.asyncio
async def test_opt_in_parsing_persists_and_rechunk_reuses_ir_exactly() -> None:
    database = ArtifactDatabase()
    service = _service(database)
    source = "  第一段。\n\n第二段保留尾空格。  \n"

    first = await service._load_or_parse_document_ir(
        dataset=DATASET,
        document=DOCUMENT,
        index_config=ENABLED,
        source_text=source,
    )
    second = await service._load_or_parse_document_ir(
        dataset=DATASET,
        document=DOCUMENT,
        index_config=ENABLED,
        source_text=source,
    )

    assert first.encode("utf-8") == source.encode("utf-8")
    assert second.encode("utf-8") == source.encode("utf-8")
    assert database.page_stores == 1
    assert database.ir_stores == 1
    assert database.ir_loads == 2


@pytest.mark.asyncio
async def test_page_cache_resumes_after_document_ir_publication_failure() -> None:
    database = ArtifactDatabase(fail_ir_store_once=True)
    service = _service(database)

    with pytest.raises(RuntimeError, match="injected document IR"):
        await service._load_or_parse_document_ir(
            dataset=DATASET,
            document=DOCUMENT,
            index_config=ENABLED,
            source_text="cached page",
        )

    rendered = await service._load_or_parse_document_ir(
        dataset=DATASET,
        document=DOCUMENT,
        index_config=ENABLED,
        source_text="cached page",
    )

    assert rendered == "cached page"
    assert database.page_stores == 1
    assert database.page_loads == 2
    assert database.ir_stores == 2


@pytest.mark.asyncio
async def test_feature_flag_off_keeps_legacy_path_and_touches_no_artifact_store() -> None:
    database = ArtifactDatabase()
    source = "legacy\n\nbytes"

    rendered = await _service(database)._load_or_parse_document_ir(
        dataset=DATASET,
        document=DOCUMENT,
        index_config={"parsing": {"enabled": False}},
        source_text=source,
    )

    assert rendered == source
    assert database.ir_loads == 0
    assert database.ir_stores == 0
    assert database.page_loads == 0
    assert database.page_stores == 0
