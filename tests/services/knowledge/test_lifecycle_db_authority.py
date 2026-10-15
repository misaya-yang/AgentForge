from __future__ import annotations

import json
from contextlib import AbstractAsyncContextManager
from typing import Any

import pytest
from knowledge_service.persistence.database import (
    CONFLUENCE_SYNC_GENERATION_KEY,
    DOCUMENT_INGEST_ACTION_KEY,
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DOCUMENT_PENDING_RESTORE_VERSION_KEY,
    DOCUMENT_PIPELINE_EXECUTION_KEY,
    DOCUMENT_RECOVER_STAGE_KEY,
    DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
    DOCUMENT_UPLOAD_GENERATION_KEY,
    SOURCE_OWNED_DOCUMENT_METADATA_KEYS,
    DatabaseStorage,
)


class AsyncContext(AbstractAsyncContextManager[Any]):
    def __init__(self, value: Any) -> None:
        self.value = value

    async def __aenter__(self) -> Any:
        return self.value

    async def __aexit__(self, *_args: Any) -> None:
        return None


class RecordingConnection:
    def __init__(self) -> None:
        self.fetch_behaviors: list[list[dict[str, Any]] | Exception] = []
        self.fetchrow_behaviors: list[dict[str, Any] | None | Exception] = []
        self.fetchval_behaviors: list[Any | Exception] = []
        self.execute_behaviors: list[str | Exception] = []
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.fetchrow_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.fetchval_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.execute_calls: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> AsyncContext:
        return AsyncContext(self)

    @staticmethod
    def _resolve(behaviors: list[Any], default: Any) -> Any:
        result = behaviors.pop(0) if behaviors else default
        if isinstance(result, Exception):
            raise result
        return result

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        return self._resolve(self.fetch_behaviors, [])

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        self.fetchrow_calls.append((query, args))
        return self._resolve(self.fetchrow_behaviors, None)

    async def fetchval(self, query: str, *args: Any) -> Any:
        self.fetchval_calls.append((query, args))
        return self._resolve(self.fetchval_behaviors, None)

    async def execute(self, query: str, *args: Any) -> str:
        self.execute_calls.append((query, args))
        return self._resolve(self.execute_behaviors, "UPDATE 1")


class RecordingPool:
    def __init__(self, connection: RecordingConnection) -> None:
        self.connection = connection
        self.acquire_calls = 0

    def acquire(self) -> AsyncContext:
        self.acquire_calls += 1
        return AsyncContext(self.connection)


def make_database() -> tuple[DatabaseStorage, RecordingConnection, RecordingPool]:
    connection = RecordingConnection()
    pool = RecordingPool(connection)
    database = DatabaseStorage()
    database._pool = pool  # type: ignore[assignment]
    return database, connection, pool


def normalized(query: str) -> str:
    return " ".join(query.split())


@pytest.mark.asyncio
async def test_fetchval_uses_authoritative_pool() -> None:
    database, connection, pool = make_database()
    connection.fetchval_behaviors = [1]

    assert await database.fetchval("SELECT 1") == 1
    assert pool.acquire_calls == 1
    assert connection.fetchval_calls == [("SELECT 1", ())]


ACTIVE_SQL_FRAGMENTS = (
    "JOIN documents AS d",
    "JOIN datasets AS ds",
    "s.dataset_id = $1",
    "ds.tenant_id = $2",
    "COALESCE(s.enabled, TRUE) = TRUE",
    "s.status = 'completed'",
    "COALESCE(d.enabled, TRUE) = TRUE",
    "COALESCE(d.archived, FALSE) = FALSE",
    "? '_document_lifecycle_reindex'",
)

# Zero-window fence (PRD T1): serving gates must decide per segment, never
# per document lifecycle status. Gating on d.status='completed' blacks out
# the previous generation for the whole re-ingest run.
FORBIDDEN_DOCUMENT_STATUS_GATE = "d.status = 'completed'"


@pytest.mark.asyncio
async def test_fts_requires_exact_tenant_dataset_and_active_document() -> None:
    database, connection, _pool = make_database()
    connection.fetch_behaviors = [[{"segment_id": "segment-a", "text": "needle"}]]

    rows = await database.search_segments_text(
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        terms=["needle"],
        limit=5,
    )

    assert [row["segment_id"] for row in rows] == ["segment-a"]
    query, args = connection.fetch_calls[0]
    compact = normalized(query)
    for fragment in ACTIVE_SQL_FRAGMENTS:
        assert fragment in compact
    assert FORBIDDEN_DOCUMENT_STATUS_GATE not in compact
    assert "s.text_search @@" in compact
    assert args[:2] == ("dataset-a", "tenant-a")


@pytest.mark.asyncio
async def test_ilike_fallback_preserves_active_scope_predicates() -> None:
    database, connection, _pool = make_database()

    class UndefinedTextSearchColumnError(RuntimeError):
        sqlstate = "42703"
        column_name = "text_search"

    connection.fetch_behaviors = [
        UndefinedTextSearchColumnError("column text_search does not exist"),
        [{"segment_id": "segment-a", "text": "needle"}],
    ]

    rows = await database.search_segments_text(
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        terms=["needle"],
        limit=5,
    )

    assert [row["segment_id"] for row in rows] == ["segment-a"]
    query, args = connection.fetch_calls[1]
    compact = normalized(query)
    for fragment in ACTIVE_SQL_FRAGMENTS:
        assert fragment in compact
    assert FORBIDDEN_DOCUMENT_STATUS_GATE not in compact
    assert "s.text ILIKE $3" in compact
    assert args == ("dataset-a", "tenant-a", "%needle%", 5)


@pytest.mark.asyncio
async def test_active_candidate_filters_are_tenant_scoped_and_deduplicated() -> None:
    database, connection, pool = make_database()
    connection.fetch_behaviors = [
        [{"segment_id": "segment-a"}, {"segment_id": "segment-a"}],
        [{"document_id": "document-a"}],
    ]

    segment_ids = await database.filter_active_segment_ids(
        "dataset-a",
        "tenant-a",
        ["segment-a", "segment-a", ""],
    )
    document_ids = await database.filter_active_document_ids(
        "dataset-a",
        "tenant-a",
        ["document-a", "document-a"],
    )

    assert segment_ids == {"segment-a"}
    assert document_ids == {"document-a"}
    segment_query, segment_args = connection.fetch_calls[0]
    document_query, document_args = connection.fetch_calls[1]
    assert "s.segment_id = ANY($3::text[])" in normalized(segment_query)
    assert "d.document_id = ANY($3::text[])" in normalized(document_query)
    assert FORBIDDEN_DOCUMENT_STATUS_GATE not in normalized(segment_query)
    assert FORBIDDEN_DOCUMENT_STATUS_GATE not in normalized(document_query)
    assert segment_args == ("dataset-a", "tenant-a", ["segment-a"])
    assert document_args == ("dataset-a", "tenant-a", ["document-a"])

    acquire_calls = pool.acquire_calls
    assert await database.filter_active_segment_ids("dataset-a", "tenant-a", []) == set()
    assert await database.filter_active_document_ids("dataset-a", "tenant-a", []) == set()
    assert pool.acquire_calls == acquire_calls


@pytest.mark.asyncio
async def test_scoped_context_reads_join_active_authority() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {"segment_id": "segment-a", "dataset_id": "dataset-a"},
        {"document_id": "document-a", "summary": "safe"},
    ]

    segment = await database.get_segment_scoped("segment-a", "dataset-a", "tenant-a")
    summary = await database.get_document_summary_scoped("document-a", "dataset-a", "tenant-a")

    assert segment is not None and segment["segment_id"] == "segment-a"
    assert summary is not None and summary["summary"] == "safe"
    for query, args in connection.fetchrow_calls:
        compact = normalized(query)
        assert "JOIN datasets AS ds" in compact
        assert "ds.tenant_id = $3" in compact
        assert "COALESCE(d.enabled, TRUE) = TRUE" in compact
        assert "COALESCE(d.archived, FALSE) = FALSE" in compact
        assert FORBIDDEN_DOCUMENT_STATUS_GATE not in compact
        assert args[1:] == ("dataset-a", "tenant-a")


@pytest.mark.asyncio
async def test_tenant_first_segment_lookup_has_no_cross_tenant_existence_oracle() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {"segment_id": "segment-a", "dataset_id": "dataset-a"},
        None,
    ]

    visible = await database.get_active_segment_by_tenant("segment-a", "tenant-a")
    hidden = await database.get_active_segment_by_tenant("segment-a", "tenant-b")

    assert visible is not None and visible["dataset_id"] == "dataset-a"
    assert hidden is None
    for query, args in connection.fetchrow_calls:
        compact = normalized(query)
        assert "ds.tenant_id = $2" in compact
        assert "s.status = 'completed'" in compact
        assert FORBIDDEN_DOCUMENT_STATUS_GATE not in compact
        assert "? '_document_lifecycle_reindex'" in compact
        assert args[0] == "segment-a"


@pytest.mark.asyncio
async def test_association_read_and_writers_validate_both_segment_scopes() -> None:
    database, connection, _pool = make_database()
    connection.fetch_behaviors = [[{"segment_id": "text-a", "image_segment_id": "image-a"}]]
    connection.execute_behaviors = ["INSERT 0 1", "INSERT 0 1"]

    associations = await database.get_segment_associations_batch(
        ["text-a"],
        dataset_id="dataset-a",
        tenant_id="tenant-a",
    )
    assert associations["text-a"][0]["image_segment_id"] == "image-a"
    read_query, read_args = connection.fetch_calls[0]
    compact_read = normalized(read_query)
    assert "image_s.dataset_id = source_s.dataset_id" in compact_read
    assert "source_s.dataset_id = $2" in compact_read
    assert "ds.tenant_id = $3" in compact_read
    assert read_args == (["text-a"], "dataset-a", "tenant-a")

    assert await database.add_segment_image_association(
        "text-a",
        "image-a",
        dataset_id="dataset-a",
        tenant_id="tenant-a",
    )
    assert (
        await database.add_segment_image_associations_batch(
            [{"segment_id": "text-a", "image_segment_id": "image-a"}],
            dataset_id="dataset-a",
            tenant_id="tenant-a",
        )
        == 1
    )
    for query, args in connection.execute_calls:
        compact_write = normalized(query)
        assert "image_s.dataset_id = source_s.dataset_id" in compact_write
        assert "source_s.dataset_id = $7" in compact_write
        assert "ds.tenant_id = $8" in compact_write
        assert args[-2:] == ("dataset-a", "tenant-a")


@pytest.mark.asyncio
async def test_completed_status_atomically_activates_pending_restore() -> None:
    database, connection, _pool = make_database()

    await database.update_document_status(
        "document-a",
        status="completed",
        progress=100,
    )

    query, args = connection.execute_calls[0]
    compact = normalized(query)
    marker_predicate = f"metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}' ->> 'status' = 'pending'"
    assert marker_predicate in compact
    assert f"metadata - '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'" in compact
    assert "enabled = CASE WHEN" in compact
    assert "archived = CASE WHEN" in compact
    assert args[0] == "completed"
    assert args[-1] == "document-a"
    # PRD T1 unified contract (§885-886): the activation statement itself
    # advances the dataset content revision the retrieval cache is keyed on,
    # gated on the identical pending-marker predicate as the CASE, so a
    # result cached while the document was hidden cannot survive activation.
    assert "WITH pending_restore AS" in compact
    assert "revision_bump AS" in compact
    assert "UPDATE datasets AS ds" in compact
    assert "content_revision = COALESCE(content_revision, 0) + 1" in compact
    assert "ds.is_deleted = FALSE" in compact
    assert "EXISTS (SELECT 1 FROM pending_restore)" in compact
    assert compact.index("UPDATE datasets AS ds") > compact.index(
        "WITH pending_restore AS"
    )


@pytest.mark.asyncio
async def test_non_completed_status_writes_carry_no_revision_bump() -> None:
    database, connection, _pool = make_database()

    await database.update_document_status(
        "document-a",
        status="error",
        error="boom",
    )
    await database.update_document_status("document-a", status="parsing", progress=5)

    for query, _args in connection.execute_calls:
        compact = normalized(query)
        # An error-terminal restore stays hidden and must not invalidate the
        # retrieval cache; stage writes never change visible content either.
        assert "UPDATE datasets" not in compact
        assert "revision_bump" not in compact


@pytest.mark.asyncio
async def test_bump_dataset_content_revision_contract() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {"dataset_id": "dataset-a"},
        {"dataset_id": "dataset-a"},
    ]

    assert await database.bump_dataset_content_revision("dataset-a") is True

    query, args = connection.fetchrow_calls[0]
    compact = normalized(query)
    assert "UPDATE datasets" in compact
    assert "content_revision = COALESCE(content_revision, 0) + 1" in compact
    assert "is_deleted = FALSE" in compact
    assert args == ("dataset-a",)

    # Works on a supplied lifecycle-lease connection without touching the pool.
    assert (
        await database.bump_dataset_content_revision(
            "dataset-a",
            connection=connection,
        )
        is True
    )
    assert connection.fetchrow_calls[1][1] == ("dataset-a",)

    with pytest.raises(ValueError):
        await database.bump_dataset_content_revision("  ")
    # A missing or soft-deleted dataset reports False rather than raising.
    connection.fetchrow_behaviors = [None]
    assert await database.bump_dataset_content_revision("dataset-gone") is False


@pytest.mark.asyncio
async def test_index_write_lease_rejects_inactive_document_without_pending_marker() -> None:
    database, connection, _pool = make_database()
    connection.fetchval_behaviors = [True, 0]
    connection.fetchrow_behaviors = [
        {
            "tenant_id": "tenant-a",
            "collection_name": "base",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 384,
            "embedding_config": {},
            "index_config": {},
        }
    ]

    with pytest.raises(RuntimeError, match="missing or inactive"):
        async with database.dataset_index_write_lease(
            "dataset-a",
            ["document-a"],
        ):
            raise AssertionError("inactive lease must not yield")

    count_query, count_args = connection.fetchval_calls[1]
    compact = normalized(count_query)
    assert "COALESCE(enabled, TRUE) = TRUE" in compact
    assert "COALESCE(archived, FALSE) = FALSE" in compact
    assert f"metadata -> '{DOCUMENT_LIFECYCLE_REINDEX_KEY}'" in compact
    assert count_args == ("dataset-a", ["document-a"])


@pytest.mark.asyncio
async def test_document_enqueue_claim_uses_dataset_then_document_lock_and_active_authority() -> None:
    database, connection, _pool = make_database()
    connection.fetchval_behaviors = [True, True, True, True]
    connection.fetchrow_behaviors = [
        {
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 384,
            "embedding_config": {},
            "index_config": {},
        },
        {"document_id": "document-a"},
    ]

    assert await database.claim_document_for_enqueue("dataset-a", "document-a") is True

    lock_queries = [normalized(query) for query, _args in connection.fetchval_calls]
    assert "pg_try_advisory_lock_shared" in lock_queries[0]
    assert connection.fetchval_calls[0][1][0] == "knowledge-dataset-index:dataset-a"
    assert "pg_try_advisory_lock" in lock_queries[1]
    assert connection.fetchval_calls[1][1][0] == "knowledge-document-index:dataset-a:document-a"
    update_query, update_args = connection.fetchrow_calls[1]
    compact = normalized(update_query)
    assert "SET status = 'waiting'" in compact
    assert "started_at = NULL" in compact
    assert "completed_at = NULL" in compact
    assert "parsing_started_at = NULL" in compact
    assert "splitting_started_at = NULL" in compact
    assert "indexing_started_at = NULL" in compact
    assert "status IN ('waiting', 'completed', 'error')" in compact
    assert "COALESCE(enabled, TRUE) = TRUE" in compact
    assert "COALESCE(archived, FALSE) = FALSE" in compact
    assert "desired_enabled' = 'true'" in compact
    assert "desired_archived' = 'false'" in compact
    # Dual-verb contract: the claim binds a third metadata-patch parameter;
    # for the default ingest it is NULL, which strips any stale verb markers.
    # The fourth parameter carries the requested verb (empty for ingest);
    # the fifth and sixth bind the replay-rule CAS and execution owner.
    assert update_args == ("document-a", "dataset-a", None, "", False, None)
    assert "WHEN $3::jsonb IS NULL THEN" in compact
    assert f"- '{DOCUMENT_INGEST_ACTION_KEY}'" in compact
    assert f"- '{DOCUMENT_RECOVER_STAGE_KEY}'" in compact
    assert f"- '{DOCUMENT_PIPELINE_EXECUTION_KEY}'" in compact
    assert "status <> 'waiting'" in compact
    assert "= $4" in compact
    assert "pg_advisory_unlock" in lock_queries[-2]
    assert "pg_advisory_unlock_shared" in lock_queries[-1]


@pytest.mark.asyncio
async def test_consumer_claim_rechecks_active_or_exact_restore_marker() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 384,
            "embedding_config": {},
            "index_config": {},
        },
        {"document_id": "document-a"},
    ]

    assert await database.claim_queued_document_for_processing(
        "dataset-a",
        "document-a",
        connection=connection,
    )

    compact = normalized(connection.fetchrow_calls[1][0])
    assert "status = 'waiting'" in compact
    assert "SET status = 'parsing'" in compact
    assert "parsing_started_at = COALESCE(parsing_started_at, NOW())" in compact
    assert "? '_document_lifecycle_reindex'" in compact
    assert "desired_enabled' = 'true'" in compact
    assert "desired_archived' = 'false'" in compact


@pytest.mark.asyncio
async def test_recovery_claim_is_queued_skip_locked_and_lease_aware_for_default_config() -> None:
    database, connection, _pool = make_database()
    connection.fetch_behaviors = [
        [
            {
                "document_id": "document-a",
                "dataset_id": "dataset-a",
                "status": "waiting",
                "old_status": "indexing",
            }
        ]
    ]

    rows = await database.claim_stuck_documents(15, limit=7)

    assert rows[0]["old_status"] == "indexing"
    query, args = connection.fetch_calls[0]
    compact = normalized(query)
    assert "FOR UPDATE OF d SKIP LOCKED" in compact
    assert "SET status = CASE" in compact
    assert "ELSE 'waiting'" in compact
    assert "d.status NOT IN ('completed', 'error')" in compact
    assert "COALESCE(d.enabled, TRUE) = TRUE" in compact
    assert "COALESCE(d.archived, FALSE) = FALSE" in compact
    assert "pg_try_advisory_xact_lock_shared" in compact
    assert "pg_try_advisory_xact_lock" in compact
    assert "AND NOT COALESCE(" in compact
    assert "? '_index_deletion_fence', FALSE" in compact
    assert "desired_enabled' = 'true'" in compact
    assert args == (15, 7, False)


@pytest.mark.asyncio
async def test_generic_metadata_preserves_marker_and_rejects_reserved_injection() -> None:
    database, connection, _pool = make_database()

    with pytest.raises(ValueError, match="reserved"):
        await database.update_document_fields(
            "document-a",
            {"metadata": {DOCUMENT_LIFECYCLE_REINDEX_KEY: {"status": "pending"}}},
        )
    with pytest.raises(ValueError, match="reserved"):
        await database.update_document_fields(
            "document-a",
            {"metadata": {DOCUMENT_PENDING_RESTORE_VERSION_KEY: 3}},
        )
    with pytest.raises(ValueError, match="reserved"):
        await database.update_document_fields(
            "document-a",
            {"metadata": {DOCUMENT_RESTORED_SOURCE_VERSION_KEY: 3}},
        )
    assert connection.execute_calls == []

    await database.update_document_fields(
        "document-a",
        {"metadata": {"user_key": "value"}},
    )
    compact = normalized(connection.execute_calls[0][0])
    assert "metadata = $1::jsonb ||" in compact
    assert "jsonb_build_object('_document_lifecycle_reindex'" in compact
    assert "jsonb_build_object('_document_pending_restore_version'" in compact
    assert "jsonb_build_object('_document_restored_source_version'" in compact


@pytest.mark.asyncio
async def test_internal_lifecycle_metadata_override_and_conditional_clear_are_explicit() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [{"document_id": "document-a"}]

    await database.update_document_fields(
        "document-a",
        {
            "metadata": {
                DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                    "status": "deactivating",
                    "desired_enabled": False,
                    "desired_archived": False,
                }
            }
        },
        allow_lifecycle_marker_update=True,
    )
    assert "metadata = $1::jsonb" in normalized(connection.execute_calls[0][0])

    assert await database.clear_document_lifecycle_marker(
        "document-a",
        expected_status="deactivating",
    )
    clear_query, clear_args = connection.fetchrow_calls[0]
    assert "metadata -> '_document_lifecycle_reindex' ->> 'status' = $2" in normalized(
        clear_query
    )
    assert clear_args == ("document-a", "deactivating")


@pytest.mark.asyncio
async def test_segment_index_state_never_overwrites_user_enabled_toggle() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [{"segment_id": "segment-a"}]

    await database.set_segment_index_state("segment-a", "pending")

    compact = normalized(connection.fetchrow_calls[0][0])
    assert "SET status = $2" in compact
    assert "enabled" not in compact


@pytest.mark.asyncio
async def test_segment_lease_order_is_dataset_document_segment_then_reverse_release() -> None:
    database, connection, _pool = make_database()
    connection.fetchval_behaviors = [True, True, True, True, True, True]

    async with database.segment_index_update_lease(
        "dataset-a",
        "document-a",
        "segment-a",
    ) as yielded:
        assert yielded is connection

    calls = connection.fetchval_calls
    assert calls[0][1] == ("knowledge-dataset-index:dataset-a",)
    assert calls[1][1] == ("knowledge-document-index:dataset-a:document-a",)
    assert calls[2][1] == ("knowledge-segment-index:dataset-a:segment-a",)
    assert "pg_advisory_unlock" in normalized(calls[3][0])
    assert calls[3][1] == ("knowledge-segment-index:dataset-a:segment-a",)
    assert calls[4][1] == ("knowledge-document-index:dataset-a:document-a",)
    assert "pg_advisory_unlock_shared" in normalized(calls[5][0])


@pytest.mark.asyncio
async def test_document_upload_insert_is_not_upsert_and_finalize_is_exact_cas() -> None:
    database, connection, _pool = make_database()
    dataset_row = {
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": {},
    }
    connection.fetchrow_behaviors = [dataset_row]
    document = {
        "document_id": "document-a",
        "dataset_id": "dataset-a",
        "status": "uploading",
        "metadata": {DOCUMENT_UPLOAD_GENERATION_KEY: "generation-a"},
    }

    await database.insert_document(document)

    insert_query = normalized(connection.execute_calls[0][0])
    assert "INSERT INTO documents" in insert_query
    assert "ON CONFLICT" not in insert_query

    connection.fetchrow_behaviors = [dataset_row, {"document_id": "document-a"}]
    finalized = {
        **document,
        "status": "waiting",
        "metadata": {
            DOCUMENT_UPLOAD_GENERATION_KEY: "generation-a",
            "original_file_key": "knowledge/documents/tenant-a/document-a/original/a.pdf",
        },
    }
    assert await database.finalize_document_upload(
        finalized,
        upload_generation="generation-a",
        connection=connection,
    )
    finalize_query, finalize_args = connection.fetchrow_calls[-1]
    compact = normalized(finalize_query)
    assert "UPDATE documents" in compact
    assert "INSERT INTO" not in compact
    assert "status IN ( 'uploading', 'waiting', 'uploading_images'" in compact
    assert "metadata ->> $15 = $16" in compact
    assert finalize_args[-1] == "generation-a"
    assert DOCUMENT_UPLOAD_GENERATION_KEY not in json.loads(finalize_args[11])


@pytest.mark.asyncio
async def test_worker_source_publications_are_single_row_authority_cas() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {"document_id": "document-a"},
        {"document_id": "document-a"},
    ]

    assert await database.compare_and_swap_document_processing_mode(
        "document-a",
        "dataset-a",
        expected_mode="auto",
        replacement_mode="multimodal",
        detection_result={"confidence": 0.9},
        connection=connection,
    )
    assert await database.publish_document_image_receipt(
        "document-a",
        "dataset-a",
        expected_original_file_key="owned-original",
        expected_processing_mode="multimodal",
        extracted_images=[
            {
                "image_id": "image-a",
                "storage_url": "signed-url",
                "storage_key": "knowledge/confluence/tenant-a/document-a/images/a.png",
            }
        ],
        connection=connection,
    )

    for query, _args in connection.fetchrow_calls:
        compact = normalized(query)
        assert "UPDATE documents" in compact
        assert "desired_enabled' = 'true'" in compact
        assert "desired_archived' = 'false'" in compact
        assert "? '_document_upload_generation'" in compact
        assert "? '_confluence_sync_generation'" in compact
    receipt_query, receipt_args = connection.fetchrow_calls[-1]
    assert "jsonb_array_length($5::jsonb)" in normalized(receipt_query)
    assert json.loads(receipt_args[-1])[0]["storage_key"].endswith("/a.png")


@pytest.mark.asyncio
async def test_generic_metadata_cannot_replace_source_owned_receipts() -> None:
    database, connection, _pool = make_database()

    await database.update_document_fields(
        "document-a",
        {
            "metadata": {
                "user_key": "new",
                "original_file_key": "attacker-key",
                "extracted_images": [{"storage_key": "foreign"}],
                "processing_mode": "multimodal",
            }
        },
    )

    query, args = connection.execute_calls[0]
    compact = normalized(query)
    assert json.loads(args[0]) == {"user_key": "new"}
    for key in SOURCE_OWNED_DOCUMENT_METADATA_KEYS:
        assert f"jsonb_build_object('{key}'" in compact


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "key", ["_special_publication_generation_id", "_special_source_manifest"],
)
async def test_generic_metadata_rejects_special_publication_owner_injection(key: str) -> None:
    database, connection, _pool = make_database()

    with pytest.raises(ValueError, match="reserved"):
        await database.update_document_fields("document-a", {"metadata": {key: "forged"}})

    assert connection.execute_calls == []


@pytest.mark.asyncio
async def test_confluence_owner_supersession_requires_stale_ttl_and_exact_abort() -> None:
    database, connection, _pool = make_database()
    connection.fetchrow_behaviors = [
        {"document_id": "document-a"},
        {"document_id": "document-a"},
    ]

    assert await database.begin_confluence_document_sync(
        "document-a",
        "dataset-a",
        generation="generation-a",
        source_metadata={"source_owner": {"kind": "binding", "id": "binding-a"}},
        connection=connection,
    )
    begin_query, begin_args = connection.fetchrow_calls[0]
    compact = normalized(begin_query)
    assert "started_at" in json.loads(begin_args[3])
    assert "make_interval(secs => $6)" in compact
    assert begin_args[5] == 3600

    assert await database.abort_confluence_document_sync(
        "document-a",
        "dataset-a",
        generation="generation-a",
        error="manifest changed",
        connection=connection,
    )
    abort_query, abort_args = connection.fetchrow_calls[1]
    compact_abort = normalized(abort_query)
    assert "status = 'error'" in compact_abort
    assert f"- '{CONFLUENCE_SYNC_GENERATION_KEY}'" in compact_abort
    assert "->> 'generation' = $3" in compact_abort
    assert abort_args[:3] == ("document-a", "dataset-a", "generation-a")


# ---------------------------------------------------------------------------
# D1/D4 (frontend handoff): the list page must carry the fields the UI
# derives badges and stage durations from, and pagination needs a companion
# count that shares the page's filter.
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_list_documents_select_carries_badge_and_stage_columns() -> None:
    database, connection, _pool = make_database()
    connection.fetch_behaviors = [[]]

    await database.list_documents("dataset-a")

    query, args = connection.fetch_calls[0]
    compact = normalized(query)
    for column in (
        "enabled",
        "archived",
        "archived_reason",
        "archived_at",
        "parsing_started_at",
        "splitting_started_at",
        "indexing_started_at",
    ):
        assert column in compact, f"list page SELECT is missing {column}"
    assert args[:1] == ("dataset-a",)


@pytest.mark.asyncio
async def test_list_documents_pagination_params_bind_after_filters() -> None:
    database, connection, _pool = make_database()
    connection.fetch_behaviors = [[]]

    await database.list_documents("dataset-a", status="completed", limit=25, offset=50)

    query, args = connection.fetch_calls[0]
    compact = normalized(query)
    assert "AND d.status = $2" in compact
    assert compact.endswith("LIMIT $3 OFFSET $4")
    assert args == ("dataset-a", "completed", 25, 50)


@pytest.mark.asyncio
async def test_count_documents_counts_dataset_rows() -> None:
    database, connection, _pool = make_database()
    connection.fetchval_behaviors = [7]

    assert await database.count_documents("dataset-a") == 7
    query, args = connection.fetchval_calls[0]
    assert normalized(query) == "SELECT COUNT(*) FROM documents AS d WHERE d.dataset_id = $1"
    assert args == ("dataset-a",)


@pytest.mark.asyncio
async def test_count_segments_mirrors_list_segment_filters() -> None:
    database, connection, _pool = make_database()
    connection.fetchval_behaviors = [3]

    assert (
        await database.count_segments(
            "dataset-a", document_id="document-a", query_text="合规"
        )
        == 3
    )
    query, args = connection.fetchval_calls[0]
    compact = normalized(query)
    assert "SELECT COUNT(*) FROM segments AS s WHERE s.dataset_id = $1" in compact
    assert "AND s.document_id = $2" in compact
    assert "AND s.text ILIKE $3" in compact
    assert args == ("dataset-a", "document-a", "%合规%")
