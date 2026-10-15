"""Durable T1 attachment receipts and T4 parsing artifacts.

The methods in this mixin are deliberately tenant/dataset/document scoped.
Callers never fetch a cache row by its digest alone: equal source bytes in two
tenants must not become a cross-tenant parsing side channel.
"""

from __future__ import annotations

import json
from typing import Any


def _required_identifier(value: Any, label: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise ValueError(f"{label} is required")
    return normalized


def _decode_json_object(value: Any, label: str) -> dict[str, Any]:
    if isinstance(value, str):
        value = json.loads(value)
    if not isinstance(value, dict):
        raise RuntimeError(f"persisted {label} is malformed")
    return value


class KnowledgeArtifactPersistenceMixin:
    """Asyncpg persistence mixed into ``DatabaseStorage``."""

    async def load_parsing_ir(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        generation_key: str,
        parser_bundle: str,
        parser_config_hash: str,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        if not self._pool:
            raise RuntimeError("database is not connected")
        values = tuple(
            _required_identifier(value, label)
            for value, label in (
                (tenant_id, "tenant_id"),
                (dataset_id, "dataset_id"),
                (document_id, "document_id"),
                (generation_key, "generation_key"),
                (parser_bundle, "parser_bundle"),
                (parser_config_hash, "parser_config_hash"),
            )
        )

        async def _load(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                SELECT ir_id, tenant_id, dataset_id, document_id,
                       generation_key, content_hash, schema_version,
                       parser_bundle, parser_config_hash, cascade_config,
                       ir, stats, created_at, updated_at
                FROM knowledge.kb_parsing_ir
                WHERE tenant_id = $1
                  AND dataset_id = $2
                  AND document_id = $3
                  AND generation_key = $4
                  AND parser_bundle = $5
                  AND parser_config_hash = $6
                """,
                *values,
            )

        if connection is not None:
            row = await _load(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _load(conn)
        if row is None:
            return None
        result = dict(row)
        for key in ("cascade_config", "ir", "stats"):
            result[key] = _decode_json_object(result.get(key), key)
        return result

    async def store_parsing_ir(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        generation_key: str,
        content_hash: str,
        schema_version: str,
        parser_bundle: str,
        parser_config_hash: str,
        cascade_config: dict[str, Any],
        ir: dict[str, Any],
        stats: dict[str, Any],
        connection: Any | None = None,
    ) -> bool:
        if not self._pool:
            raise RuntimeError("database is not connected")
        identifiers = tuple(
            _required_identifier(value, label)
            for value, label in (
                (tenant_id, "tenant_id"),
                (dataset_id, "dataset_id"),
                (document_id, "document_id"),
                (generation_key, "generation_key"),
                (content_hash, "content_hash"),
                (schema_version, "schema_version"),
                (parser_bundle, "parser_bundle"),
                (parser_config_hash, "parser_config_hash"),
            )
        )
        if not isinstance(cascade_config, dict):
            raise ValueError("cascade_config must be an object")
        if not isinstance(ir, dict) or not isinstance(stats, dict):
            raise ValueError("parsing IR and stats must be objects")

        async def _store(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                INSERT INTO knowledge.kb_parsing_ir (
                    tenant_id, dataset_id, document_id, generation_key,
                    content_hash, schema_version, parser_bundle,
                    parser_config_hash, cascade_config, ir, stats
                )
                SELECT $1::varchar, $2::varchar, $3::varchar, $4::varchar,
                       $5::varchar, $6::varchar, $7::varchar, $8::varchar,
                       $9::jsonb, $10::jsonb, $11::jsonb
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.document_id = $3::varchar
                  AND d.dataset_id = $2::varchar
                  AND ds.tenant_id = $1::varchar
                  AND ds.is_deleted = FALSE
                ON CONFLICT (
                    tenant_id, dataset_id, document_id, generation_key,
                    parser_bundle, parser_config_hash
                ) DO UPDATE SET
                    content_hash = EXCLUDED.content_hash,
                    schema_version = EXCLUDED.schema_version,
                    cascade_config = EXCLUDED.cascade_config,
                    ir = EXCLUDED.ir,
                    stats = EXCLUDED.stats,
                    updated_at = NOW()
                RETURNING ir_id
                """,
                *identifiers,
                json.dumps(cascade_config),
                json.dumps(ir),
                json.dumps(stats),
            )

        if connection is not None:
            row = await _store(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _store(conn)
        return row is not None

    async def load_parsing_page_cache(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        generation_key: str,
        cache_key: str,
        parser_config_hash: str,
        connection: Any | None = None,
    ) -> dict[str, Any] | None:
        if not self._pool:
            raise RuntimeError("database is not connected")
        values = tuple(
            _required_identifier(value, label)
            for value, label in (
                (tenant_id, "tenant_id"),
                (dataset_id, "dataset_id"),
                (document_id, "document_id"),
                (generation_key, "generation_key"),
                (cache_key, "cache_key"),
                (parser_config_hash, "parser_config_hash"),
            )
        )

        async def _load(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                SELECT page_ir, backend, backend_version, content_hash,
                       page_number, confidence, hard_page
                FROM knowledge.kb_parsing_page_cache
                WHERE tenant_id = $1
                  AND dataset_id = $2
                  AND document_id = $3
                  AND generation_key = $4
                  AND cache_key = $5
                  AND parser_config_hash = $6
                """,
                *values,
            )

        if connection is not None:
            row = await _load(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _load(conn)
        if row is None:
            return None
        result = dict(row)
        result["page_ir"] = _decode_json_object(
            result.get("page_ir"),
            "page_ir",
        )
        return result

    async def store_parsing_page_cache(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        generation_key: str,
        cache_key: str,
        content_hash: str,
        page_number: int,
        backend: str,
        backend_version: str,
        parser_config_hash: str,
        page_ir: dict[str, Any],
        confidence: float | None,
        hard_page: bool,
        connection: Any | None = None,
    ) -> bool:
        if not self._pool:
            raise RuntimeError("database is not connected")
        identifiers = tuple(
            _required_identifier(value, label)
            for value, label in (
                (tenant_id, "tenant_id"),
                (dataset_id, "dataset_id"),
                (document_id, "document_id"),
                (generation_key, "generation_key"),
                (cache_key, "cache_key"),
                (content_hash, "content_hash"),
                (backend, "backend"),
                (backend_version, "backend_version"),
                (parser_config_hash, "parser_config_hash"),
            )
        )
        normalized_page = int(page_number)
        if normalized_page < 1:
            raise ValueError("page_number must be positive")
        if not isinstance(page_ir, dict):
            raise ValueError("page_ir must be an object")

        async def _store(conn: Any) -> Any:
            return await conn.fetchrow(
                """
                INSERT INTO knowledge.kb_parsing_page_cache (
                    tenant_id, dataset_id, document_id, generation_key,
                    cache_key, content_hash, page_number, backend,
                    backend_version, parser_config_hash, page_ir,
                    confidence, hard_page
                )
                SELECT $1::varchar, $2::varchar, $3::varchar, $4::varchar,
                       $5::varchar, $6::varchar, $7, $8::varchar,
                       $9::varchar, $10::varchar,
                       $11::jsonb, $12, $13
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.document_id = $3::varchar
                  AND d.dataset_id = $2::varchar
                  AND ds.tenant_id = $1::varchar
                  AND ds.is_deleted = FALSE
                ON CONFLICT (tenant_id, dataset_id, document_id, cache_key)
                DO UPDATE SET
                    generation_key = EXCLUDED.generation_key,
                    content_hash = EXCLUDED.content_hash,
                    page_number = EXCLUDED.page_number,
                    backend = EXCLUDED.backend,
                    backend_version = EXCLUDED.backend_version,
                    parser_config_hash = EXCLUDED.parser_config_hash,
                    page_ir = EXCLUDED.page_ir,
                    confidence = EXCLUDED.confidence,
                    hard_page = EXCLUDED.hard_page,
                    updated_at = NOW()
                RETURNING cache_key
                """,
                *identifiers[:6],
                normalized_page,
                *identifiers[6:],
                json.dumps(page_ir),
                confidence,
                bool(hard_page),
            )

        if connection is not None:
            row = await _store(connection)
        else:
            async with self._pool.acquire() as conn:
                row = await _store(conn)
        return row is not None

    async def replace_document_attachment_bindings(
        self,
        document_id: str,
        dataset_id: str,
        *,
        tenant_id: str | None = None,
        connection: Any | None = None,
    ) -> tuple[int, int]:
        """Replace derived bindings from the committed ``segment_images`` set.

        The upsert and stale-row deletion share one transaction.  If either
        fails, the previous serving binding generation remains untouched.
        """

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_document = _required_identifier(document_id, "document_id")
        normalized_dataset = _required_identifier(dataset_id, "dataset_id")
        normalized_tenant = str(tenant_id or "").strip()

        async def _replace(conn: Any) -> tuple[int, int]:
            owner_tenant = normalized_tenant or str(
                await conn.fetchval(
                    """
                    SELECT ds.tenant_id
                    FROM documents AS d
                    JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                    WHERE d.document_id = $1
                      AND d.dataset_id = $2
                      AND ds.is_deleted = FALSE
                    """,
                    normalized_document,
                    normalized_dataset,
                )
                or ""
            ).strip()
            if not owner_tenant:
                raise RuntimeError("document attachment ownership is unavailable")
            if normalized_tenant and owner_tenant != normalized_tenant:
                raise RuntimeError("document attachment tenant ownership mismatch")
            row = await conn.fetchrow(
                """
                WITH desired AS MATERIALIZED (
                    SELECT $3::varchar AS tenant_id,
                           source_s.dataset_id,
                           source_s.document_id,
                           source_s.segment_id,
                           image_s.image_attachment_id AS attachment_id,
                           '["vision"]'::jsonb AS capabilities
                    FROM segment_images AS si
                    JOIN segments AS source_s
                      ON source_s.segment_id = si.segment_id
                    JOIN segments AS image_s
                      ON image_s.segment_id = si.image_segment_id
                     AND image_s.dataset_id = source_s.dataset_id
                     AND image_s.document_id = source_s.document_id
                    WHERE source_s.document_id = $1
                      AND source_s.dataset_id = $2
                      AND BTRIM(COALESCE(image_s.image_attachment_id, '')) <> ''
                ), upserted AS (
                    INSERT INTO knowledge.kb_segment_attachment_bindings (
                        tenant_id, dataset_id, document_id, segment_id,
                        attachment_id, capabilities
                    )
                    SELECT tenant_id, dataset_id, document_id, segment_id,
                           attachment_id, capabilities
                    FROM desired
                    ON CONFLICT (
                        tenant_id, dataset_id, document_id, segment_id,
                        attachment_id
                    ) DO UPDATE SET
                        capabilities = EXCLUDED.capabilities,
                        updated_at = NOW()
                    RETURNING 1
                ), deleted AS (
                    DELETE FROM knowledge.kb_segment_attachment_bindings AS binding
                    WHERE binding.tenant_id = $3
                      AND binding.dataset_id = $2
                      AND binding.document_id = $1
                      AND NOT EXISTS (
                          SELECT 1 FROM desired
                          WHERE desired.segment_id = binding.segment_id
                            AND desired.attachment_id = binding.attachment_id
                      )
                    RETURNING 1
                )
                SELECT (SELECT COUNT(*) FROM upserted) AS upserted_count,
                       (SELECT COUNT(*) FROM deleted) AS deleted_count
                """,
                normalized_document,
                normalized_dataset,
                owner_tenant,
            )
            return int(row["upserted_count"]), int(row["deleted_count"])

        if connection is not None:
            return await _replace(connection)
        async with self._pool.acquire() as conn, conn.transaction():
            return await _replace(conn)

    async def replace_document_image_associations(
        self,
        document_id: str,
        dataset_id: str,
        tenant_id: str,
        associations: list[dict[str, Any]],
        *,
        connection: Any | None = None,
    ) -> int:
        """Atomically replace one document's complete image-association set."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_document = _required_identifier(document_id, "document_id")
        normalized_dataset = _required_identifier(dataset_id, "dataset_id")
        normalized_tenant = _required_identifier(tenant_id, "tenant_id")
        if not isinstance(associations, list):
            raise ValueError("associations must be a list")

        desired_by_pair: dict[tuple[str, str], dict[str, Any]] = {}
        for value in associations:
            if not isinstance(value, dict):
                raise ValueError("association entries must be objects")
            source_id = _required_identifier(value.get("segment_id"), "segment_id")
            image_id = _required_identifier(
                value.get("image_segment_id"),
                "image_segment_id",
            )
            desired_by_pair[(source_id, image_id)] = {
                "segment_id": source_id,
                "image_segment_id": image_id,
                "position": int(value.get("position") or 0),
                "proximity_score": float(value.get("proximity_score") or 0.0),
                "char_offset": int(value.get("char_offset") or 0),
                "page_number": value.get("page_number"),
            }
        desired = list(desired_by_pair.values())
        payload = json.dumps(desired)

        async def _replace(conn: Any) -> int:
            owner = await conn.fetchval(
                """
                SELECT ds.tenant_id
                FROM documents AS d
                JOIN datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.document_id = $1
                  AND d.dataset_id = $2
                  AND ds.tenant_id = $3
                  AND ds.is_deleted = FALSE
                """,
                normalized_document,
                normalized_dataset,
                normalized_tenant,
            )
            if owner is None:
                raise RuntimeError("document image-association ownership mismatch")
            row = await conn.fetchrow(
                """
                WITH desired AS MATERIALIZED (
                    SELECT value.segment_id,
                           value.image_segment_id,
                           value.position,
                           value.proximity_score,
                           value.char_offset,
                           value.page_number
                    FROM jsonb_to_recordset($3::jsonb) AS value(
                        segment_id varchar,
                        image_segment_id varchar,
                        position integer,
                        proximity_score double precision,
                        char_offset integer,
                        page_number integer
                    )
                ), valid_desired AS MATERIALIZED (
                    SELECT desired.*
                    FROM desired
                    JOIN segments AS source_s
                      ON source_s.segment_id = desired.segment_id
                     AND source_s.document_id = $1
                     AND source_s.dataset_id = $2
                    JOIN segments AS image_s
                      ON image_s.segment_id = desired.image_segment_id
                     AND image_s.document_id = source_s.document_id
                     AND image_s.dataset_id = source_s.dataset_id
                ), upserted AS (
                    INSERT INTO segment_images (
                        segment_id, image_segment_id, position,
                        proximity_score, char_offset, page_number
                    )
                    SELECT segment_id, image_segment_id, position,
                           proximity_score, char_offset, page_number
                    FROM valid_desired
                    ON CONFLICT (segment_id, image_segment_id) DO UPDATE SET
                        position = EXCLUDED.position,
                        proximity_score = EXCLUDED.proximity_score,
                        char_offset = EXCLUDED.char_offset,
                        page_number = EXCLUDED.page_number
                    RETURNING 1
                ), deleted AS (
                    DELETE FROM segment_images AS existing
                    USING segments AS source_s
                    WHERE existing.segment_id = source_s.segment_id
                      AND source_s.document_id = $1
                      AND source_s.dataset_id = $2
                      AND NOT EXISTS (
                          SELECT 1 FROM valid_desired
                          WHERE valid_desired.segment_id = existing.segment_id
                            AND valid_desired.image_segment_id = existing.image_segment_id
                      )
                    RETURNING 1
                )
                SELECT (SELECT COUNT(*) FROM desired) AS desired_count,
                       (SELECT COUNT(*) FROM valid_desired) AS valid_count,
                       (SELECT COUNT(*) FROM upserted) AS upserted_count
                """,
                normalized_document,
                normalized_dataset,
                payload,
            )
            if int(row["desired_count"]) != int(row["valid_count"]):
                raise RuntimeError(
                    "one or more image associations crossed document ownership"
                )
            await conn.execute(
                """
                UPDATE segments AS source_s
                SET has_images = EXISTS (
                        SELECT 1 FROM segment_images AS si
                        WHERE si.segment_id = source_s.segment_id
                    ),
                    image_count = (
                        SELECT COUNT(*) FROM segment_images AS si
                        WHERE si.segment_id = source_s.segment_id
                    ),
                    updated_at = NOW()
                WHERE source_s.document_id = $1
                  AND source_s.dataset_id = $2
                  AND source_s.content_type = 'text'
                """,
                normalized_document,
                normalized_dataset,
            )
            return int(row["upserted_count"])

        if connection is not None:
            return await _replace(connection)
        async with self._pool.acquire() as conn, conn.transaction():
            return await _replace(conn)

    async def store_image_segments(
        self,
        segments: list[dict[str, Any]],
        *,
        connection: Any | None = None,
    ) -> None:
        """Upsert a complete prepared image batch in one PostgreSQL transaction."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        if not segments:
            return

        async def _store(conn: Any) -> None:
            for segment in segments:
                row = await conn.fetchrow(
                    """
                    INSERT INTO segments (
                        segment_id, dataset_id, document_id, position,
                        text, token_count, vector_id, metadata,
                        content_type, image_url, image_attachment_id,
                        image_filename, image_media_type, image_file_size,
                        enabled, status
                    )
                    SELECT $1::varchar, $2::varchar, $3::varchar, $4,
                           $5::text, $6, $7::varchar, $8::jsonb,
                           'image', $9::text, $10::varchar, $11::varchar,
                           $12::varchar, $13, TRUE, 'completed'
                    FROM documents AS d
                    WHERE d.document_id = $3::varchar
                      AND d.dataset_id = $2::varchar
                    ON CONFLICT (segment_id) DO UPDATE SET
                        dataset_id = EXCLUDED.dataset_id,
                        document_id = EXCLUDED.document_id,
                        position = EXCLUDED.position,
                        text = EXCLUDED.text,
                        token_count = EXCLUDED.token_count,
                        vector_id = EXCLUDED.vector_id,
                        metadata = EXCLUDED.metadata,
                        content_type = EXCLUDED.content_type,
                        image_url = EXCLUDED.image_url,
                        image_attachment_id = EXCLUDED.image_attachment_id,
                        image_filename = EXCLUDED.image_filename,
                        image_media_type = EXCLUDED.image_media_type,
                        image_file_size = EXCLUDED.image_file_size,
                        enabled = TRUE,
                        status = 'completed',
                        error = NULL,
                        updated_at = NOW()
                    RETURNING segment_id
                    """,
                    _required_identifier(segment.get("segment_id"), "segment_id"),
                    _required_identifier(segment.get("dataset_id"), "dataset_id"),
                    _required_identifier(segment.get("document_id"), "document_id"),
                    int(segment.get("position") or 0),
                    str(segment.get("text") or ""),
                    int(segment.get("token_count") or 0),
                    str(segment.get("vector_id") or "").strip() or None,
                    json.dumps(segment.get("metadata") or {}),
                    segment.get("image_url"),
                    _required_identifier(
                        segment.get("image_attachment_id"),
                        "image_attachment_id",
                    ),
                    segment.get("image_filename"),
                    segment.get("image_media_type"),
                    segment.get("image_file_size"),
                )
                if row is None:
                    raise RuntimeError("image segment lost document ownership")

        if connection is not None:
            await _store(connection)
            return
        async with self._pool.acquire() as conn, conn.transaction():
            await _store(conn)

    async def commit_image_segment_cleanup(
        self,
        *,
        dataset_id: str,
        document_id: str,
        stale_segment_ids: list[str],
        expected_ingestion_identity: str,
        connection: Any,
        finish_publication: bool = True,
    ) -> int:
        """Delete stale image rows and close the shared index publication."""

        normalized_stale = [
            _required_identifier(value, "stale_segment_id")
            for value in dict.fromkeys(stale_segment_ids)
        ]
        async with connection.transaction():
            await self._require_dataset_ingestion_identity(
                connection,
                dataset_id,
                expected_ingestion_identity,
            )
            result = await connection.execute(
                """
                DELETE FROM segments
                WHERE dataset_id = $1
                  AND document_id = $2
                  AND content_type = 'image'
                  AND segment_id = ANY($3::varchar[])
                """,
                dataset_id,
                document_id,
                normalized_stale,
            )
            if finish_publication:
                await self._finish_index_publication(connection, dataset_id)
        return int(result.rsplit(" ", 1)[-1])

    async def delete_document_attachment_bindings(
        self,
        document_id: str,
        dataset_id: str,
        *,
        connection: Any | None = None,
    ) -> int:
        """Explicit delayed-cleanup hook for document deletion/rebuild tools."""

        if not self._pool:
            raise RuntimeError("database is not connected")
        normalized_document = _required_identifier(document_id, "document_id")
        normalized_dataset = _required_identifier(dataset_id, "dataset_id")

        async def _delete(conn: Any) -> int:
            result = await conn.execute(
                """
                DELETE FROM knowledge.kb_segment_attachment_bindings
                WHERE document_id = $1 AND dataset_id = $2
                """,
                normalized_document,
                normalized_dataset,
            )
            return int(result.rsplit(" ", 1)[-1])

        if connection is not None:
            return await _delete(connection)
        async with self._pool.acquire() as conn:
            return await _delete(conn)
