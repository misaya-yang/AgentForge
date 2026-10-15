"""Async PostgreSQL adapter for the parser cascade page cache."""

from __future__ import annotations

import json
from typing import Any

from .ir import PageIR


class PostgresPageCache:
    """Tenant-scoped async cache backed by ``kb_parsing_page_cache``."""

    def __init__(
        self,
        database: Any,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        generation_key: str,
        parser_config_hash: str,
        page_content_hashes: dict[int, str],
    ) -> None:
        self._database = database
        self._tenant_id = tenant_id
        self._dataset_id = dataset_id
        self._document_id = document_id
        self._generation_key = generation_key
        self._parser_config_hash = parser_config_hash
        self._page_content_hashes = dict(page_content_hashes)

    async def get(self, key: str) -> PageIR | None:
        row = await self._database.load_parsing_page_cache(
            tenant_id=self._tenant_id,
            dataset_id=self._dataset_id,
            document_id=self._document_id,
            generation_key=self._generation_key,
            cache_key=key,
            parser_config_hash=self._parser_config_hash,
        )
        if not row:
            return None
        value = row.get("page_ir")
        if isinstance(value, str):
            value = json.loads(value)
        if not isinstance(value, dict):
            raise RuntimeError("persisted parsing page cache is malformed")
        page = PageIR.from_dict(value)
        if page.page_number != int(row.get("page_number") or 0):
            raise RuntimeError("persisted parsing page cache identity mismatch")
        return page

    async def put(self, key: str, page: PageIR) -> None:
        content_hash = self._page_content_hashes.get(page.page_number)
        if not content_hash:
            raise RuntimeError("page cache write has no source content hash")
        stored = await self._database.store_parsing_page_cache(
            tenant_id=self._tenant_id,
            dataset_id=self._dataset_id,
            document_id=self._document_id,
            generation_key=self._generation_key,
            cache_key=key,
            content_hash=content_hash,
            page_number=page.page_number,
            backend=page.parser,
            backend_version=page.parser_version,
            parser_config_hash=self._parser_config_hash,
            page_ir=page.to_dict(),
            confidence=page.confidence,
            hard_page=page.hard_page,
        )
        if not stored:
            raise RuntimeError("parsing page cache lost document ownership")
