"""PostgreSQL primitives for dataset metadata registries."""

from __future__ import annotations

import json
from typing import Any

REGISTRY_INDEX_CONFIG_KEY = "document_metadata_registry"


class MetadataRegistryRevisionConflict(RuntimeError):
    """The caller edited a stale metadata registry revision."""


def normalize_registry(value: Any) -> dict[str, Any]:
    if value is None:
        return {"version": 1, "revision": 0, "fields": []}
    if not isinstance(value, dict):
        raise ValueError("document metadata registry is malformed")
    version = value.get("version", 1)
    revision = value.get("revision", 0)
    fields = value.get("fields", [])
    if version != 1 or isinstance(revision, bool) or not isinstance(revision, int):
        raise ValueError("document metadata registry version is malformed")
    if revision < 0 or not isinstance(fields, list):
        raise ValueError("document metadata registry is malformed")
    return {"version": 1, "revision": revision, "fields": fields}


class DocumentMetadataStore:
    def __init__(self, pool: Any) -> None:
        if pool is None:
            raise RuntimeError("document metadata persistence requires PostgreSQL")
        self._pool = pool

    @staticmethod
    def registry_from_index_config(index_config: Any) -> dict[str, Any]:
        config = index_config if isinstance(index_config, dict) else {}
        return normalize_registry(config.get(REGISTRY_INDEX_CONFIG_KEY))

    async def get_registry(self, dataset_id: str) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT index_config
                FROM knowledge.datasets
                WHERE dataset_id = $1 AND is_deleted = FALSE
                """,
                dataset_id,
            )
        if row is None:
            return None
        return self.registry_from_index_config(row["index_config"])

    async def get_registry_locked(
        self,
        connection: Any,
        dataset_id: str,
        *,
        for_update: bool,
    ) -> dict[str, Any] | None:
        lock = "FOR UPDATE" if for_update else "FOR SHARE"
        row = await connection.fetchrow(
            """
            SELECT index_config
            FROM knowledge.datasets
            WHERE dataset_id = $1 AND is_deleted = FALSE
            """
            + f" {lock}",
            dataset_id,
        )
        if row is None:
            return None
        return self.registry_from_index_config(row["index_config"])

    async def update_registry(
        self,
        *,
        dataset_id: str,
        expected_revision: int,
        fields: list[dict[str, Any]],
    ) -> dict[str, Any]:
        async with self._pool.acquire() as conn, conn.transaction():
            current = await self.get_registry_locked(
                conn,
                dataset_id,
                for_update=True,
            )
            if current is None:
                raise ValueError("dataset not found")
            if current["revision"] != expected_revision:
                raise MetadataRegistryRevisionConflict("metadata schema changed; refresh and retry")
            updated = {
                "version": 1,
                "revision": expected_revision + 1,
                "fields": fields,
            }
            await conn.execute(
                """
                    UPDATE knowledge.datasets
                    SET index_config = jsonb_set(
                            CASE
                                WHEN jsonb_typeof(index_config) = 'object'
                                THEN index_config
                                ELSE '{}'::jsonb
                            END,
                            $2::text[],
                            $3::jsonb,
                            TRUE
                        ),
                        updated_at = NOW()
                    WHERE dataset_id = $1 AND is_deleted = FALSE
                    """,
                dataset_id,
                [REGISTRY_INDEX_CONFIG_KEY],
                json.dumps(updated, ensure_ascii=False, allow_nan=False),
            )
            return updated
