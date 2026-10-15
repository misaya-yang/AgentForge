from __future__ import annotations

import hashlib
import json
import os
import uuid
from copy import deepcopy
from types import SimpleNamespace
from typing import Any

import asyncpg
import pytest
from knowledge_service.persistence.database import DOCUMENT_LIFECYCLE_REINDEX_KEY
from knowledge_service.services.knowledge.lexical_config import (
    BM25_V2_FIELD,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1,
    LexicalConfig,
)
from qdrant_client.http import models as qmodels

from scripts.backfill_bm25_v2 import (
    AUTHORITY_KIND,
    POINT_ID_ALGORITHM,
    RECEIPT_METADATA_KEY,
    SOURCE_TEXT_ALGORITHM,
    AuthoritySnapshot,
    BackfillError,
    BackfillManifest,
    PostgresBackfillAuthority,
    point_ids_sha256,
    prepare_manifest,
    run_backfill,
    source_text_sha256,
)


def _lexical_config(*, k: float = 1.2) -> LexicalConfig:
    return LexicalConfig.from_index_config(
        {
            "retrieval": {
                "lexical": {
                    "active_version": LEXICAL_V1,
                    "bm25_v2": {
                        "shadow_write_enabled": True,
                        "field": BM25_V2_FIELD,
                        "model": "qdrant/bm25",
                        "k": k,
                        "b": 0.75,
                        "avg_len": 256,
                        "tokenizer": "multilingual",
                        "language": "none",
                        "lowercase": True,
                        "ascii_folding": False,
                        "filtering": {
                            "required_payload_indexes": ["tenant_id", "dataset_id"],
                            "strict_unindexed_filtering": False,
                        },
                    },
                }
            }
        }
    )


def _point(
    point_id: str,
    text: str,
    *,
    tenant_id: str = "tenant-a",
    dataset_id: str = "dataset-a",
    complete: bool = False,
    config: LexicalConfig | None = None,
    content_type: str | None = None,
    level: int | None = None,
    enabled: bool | None = None,
) -> dict[str, Any]:
    selected = config or _lexical_config()
    payload: dict[str, Any] = {
        "tenant_id": tenant_id,
        "dataset_id": dataset_id,
        "text": text,
        "unrelated": "preserve-me",
    }
    if content_type is not None:
        payload["content_type"] = content_type
    if level is not None:
        payload["level"] = level
    if enabled is not None:
        payload["enabled"] = enabled
    vectors: dict[str, Any] = {"": [float(len(text)), 1.0], "bm25": "legacy"}
    if complete:
        vectors[BM25_V2_FIELD] = "already-encoded"
        payload["_lexical"] = {
            "versions": ["lexical_v1", "bm25_v2"],
            "bm25_v2_schema_fingerprint": selected.bm25_v2.fingerprint,
            "filtering_profile_fingerprint": selected.filtering.fingerprint,
            "source_text_sha256": hashlib.sha256(text.encode()).hexdigest(),
        }
    return {"id": point_id, "payload": payload, "vector": vectors}


class FakeQdrant:
    def __init__(
        self,
        points: list[dict[str, Any]],
        *,
        config: LexicalConfig | None = None,
        page_size: int = 2,
    ) -> None:
        self.lexical_config = config or _lexical_config()
        self.points = {point["id"]: deepcopy(point) for point in points}
        self.page_size = page_size
        self.metadata = {
            **deepcopy(self.lexical_config.to_collection_metadata()),
            COLLECTION_SCOPE_METADATA_KEY: {
                "schema_version": 1,
                "tenant_id": "tenant-a",
                "dataset_id": "dataset-a",
            },
        }
        self.mutations: list[tuple[str, Any]] = []
        self.queries: list[dict[str, Any]] = []
        self.vector_status: Any = SimpleNamespace(status="completed")
        self.payload_status: Any = SimpleNamespace(status="completed")

    def _info(self) -> SimpleNamespace:
        tenant_keyword = SimpleNamespace(
            data_type=qmodels.PayloadSchemaType.KEYWORD,
            params=SimpleNamespace(is_tenant=True),
        )
        keyword = SimpleNamespace(
            data_type=qmodels.PayloadSchemaType.KEYWORD,
            params=SimpleNamespace(is_tenant=False),
        )
        return SimpleNamespace(
            config=SimpleNamespace(
                metadata=deepcopy(self.metadata),
                params=SimpleNamespace(
                    sparse_vectors={
                        "bm25": qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF),
                        BM25_V2_FIELD: qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF),
                    }
                ),
            ),
            payload_schema={"tenant_id": tenant_keyword, "dataset_id": keyword},
        )

    @staticmethod
    def _matches(point: dict[str, Any], flt: qmodels.Filter | None) -> bool:
        if flt is None:
            return True

        def condition_matches(condition: Any) -> bool:
            if isinstance(condition, qmodels.Filter):
                return FakeQdrant._matches(point, condition)
            if isinstance(condition, qmodels.IsEmptyCondition):
                return point["payload"].get(condition.is_empty.key) is None
            payload_value = point["payload"].get(condition.key)
            match = condition.match
            if getattr(match, "any", None) is not None:
                return payload_value in match.any
            return payload_value == match.value

        must = flt.must if isinstance(flt.must, list) else [flt.must]
        for condition in must:
            if condition is None:
                continue
            if not condition_matches(condition):
                return False
        must_not = flt.must_not if isinstance(flt.must_not, list) else [flt.must_not]
        for condition in must_not:
            if condition is not None and condition_matches(condition):
                return False
        should = flt.should if isinstance(flt.should, list) else [flt.should]
        populated_should = [condition for condition in should if condition is not None]
        return not populated_should or any(
            condition_matches(condition) for condition in populated_should
        )

    async def get_collection(self, _collection_name: str) -> SimpleNamespace:
        return self._info()

    async def query_points(self, **kwargs: Any) -> SimpleNamespace:
        self.queries.append(kwargs)
        return SimpleNamespace(points=[])

    async def count(self, **kwargs: Any) -> SimpleNamespace:
        matched = [
            point
            for point in self.points.values()
            if self._matches(point, kwargs.get("count_filter"))
        ]
        return SimpleNamespace(count=len(matched))

    async def scroll(self, **kwargs: Any) -> tuple[list[SimpleNamespace], int | None]:
        matching = [
            point
            for point in sorted(self.points.values(), key=lambda item: item["id"])
            if self._matches(point, kwargs.get("scroll_filter"))
        ]
        start = int(kwargs.get("offset") or 0)
        size = min(int(kwargs["limit"]), self.page_size)
        selected = matching[start : start + size]
        next_offset = start + len(selected) if start + len(selected) < len(matching) else None
        return (
            [
                SimpleNamespace(
                    id=point["id"],
                    payload=deepcopy(point["payload"]),
                    vector=deepcopy(point["vector"]),
                )
                for point in selected
            ],
            next_offset,
        )

    async def update_vectors(self, **kwargs: Any) -> Any:
        self.mutations.append(("update_vectors", kwargs))
        if getattr(self.vector_status, "status", None) == "completed":
            for point_update in kwargs["points"]:
                self.points[str(point_update.id)]["vector"].update(point_update.vector)
        return self.vector_status

    async def set_payload(self, **kwargs: Any) -> Any:
        self.mutations.append(("set_payload", kwargs))
        if getattr(self.payload_status, "status", None) == "completed":
            for point_id in kwargs["points"]:
                self.points[str(point_id)]["payload"].update(deepcopy(kwargs["payload"]))
        return self.payload_status

    async def update_collection(self, **kwargs: Any) -> bool:
        self.mutations.append(("update_collection", kwargs))
        self.metadata.update(deepcopy(kwargs["metadata"]))
        return True


class FakeAuthority:
    def __init__(
        self,
        client: FakeQdrant,
        *,
        point_ids: list[str] | None = None,
        source_texts: dict[str, str] | None = None,
        content_revision: int = 11,
    ) -> None:
        self.client = client
        self.point_ids = point_ids
        self.source_texts = source_texts
        self.content_revision = content_revision

    async def snapshot(
        self,
        *,
        collection_name: str,
        tenant_id: str,
        dataset_id: str,
    ) -> AuthoritySnapshot:
        ids = self.point_ids
        if ids is None:
            ids = [
                str(point_id)
                for point_id, point in self.client.points.items()
                if point["payload"].get("dataset_id") == dataset_id
                and (
                    point["payload"].get("content_type") is None
                    or point["payload"].get("content_type") == "text"
                )
                and (
                    point["payload"].get("level") is None
                    or point["payload"].get("level") == 3
                )
                and (
                    point["payload"].get("enabled") is None
                    or point["payload"].get("enabled") is True
                )
            ]
        source_texts = self.source_texts
        if source_texts is None:
            source_texts = {
                str(point_id): str(point["payload"].get("text") or "")
                for point_id, point in self.client.points.items()
                if str(point_id) in ids
            }
        return AuthoritySnapshot(
            collection_name=collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            content_revision=self.content_revision,
            point_count=len(ids),
            point_ids_sha256=point_ids_sha256(ids),
            source_text_sha256=source_text_sha256(
                [(point_id, source_texts.get(point_id, "")) for point_id in ids]
            ),
        )


@pytest.mark.asyncio
async def test_prepare_manifest_is_read_only_deterministic_and_uses_native_document() -> None:
    client = FakeQdrant(
        [_point("c", "third"), _point("a", "first"), _point("b", "second")],
        page_size=1,
    )

    first = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        batch_size=3,
    )
    second = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        batch_size=2,
    )

    assert first == second
    assert first.point_count == 3
    assert first.point_ids_sha256 == hashlib.sha256(b"a\nb\nc\n").hexdigest()
    assert first.manifest_sha256 == first.expected_manifest_sha256
    assert client.mutations == []
    assert isinstance(client.queries[0]["query"], qmodels.Document)
    assert client.queries[0]["query"].model == "qdrant/bm25"
    assert client.queries[0]["query"].options.k == 1.2
    assert client.queries[0]["using"] == BM25_V2_FIELD


@pytest.mark.parametrize(
    "scope",
    [
        None,
        "malformed",
        {
            "schema_version": 0,
            "status": "adopting",
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
        },
        {
            "schema_version": 1,
            "tenant_id": "tenant-b",
            "dataset_id": "dataset-a",
        },
        {
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-b",
        },
    ],
)
@pytest.mark.asyncio
async def test_prepare_manifest_rejects_non_authoritative_collection_scope(
    scope: Any,
) -> None:
    client = FakeQdrant([_point("a", "text")])
    if scope is None:
        client.metadata.pop(COLLECTION_SCOPE_METADATA_KEY)
    else:
        client.metadata[COLLECTION_SCOPE_METADATA_KEY] = scope

    with pytest.raises(BackfillError, match="immutable knowledge_scope"):
        await prepare_manifest(
            client,
            authority=FakeAuthority(client),
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )

    assert client.mutations == []


@pytest.mark.asyncio
async def test_run_rechecks_collection_scope_before_receipt_invalidation_or_writes() -> None:
    client = FakeQdrant([_point("a", "text")])
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    prior_receipt = {
        "schema_version": 1,
        "status": "complete",
        "collection_name": manifest.collection_name,
    }
    client.metadata[RECEIPT_METADATA_KEY] = deepcopy(prior_receipt)
    client.metadata[COLLECTION_SCOPE_METADATA_KEY] = {
        "schema_version": 1,
        "tenant_id": "tenant-b",
        "dataset_id": "dataset-a",
    }
    client.mutations.clear()

    with pytest.raises(BackfillError, match="immutable knowledge_scope"):
        await run_backfill(
            client,
            authority=FakeAuthority(client),
            manifest=manifest,
            apply=True,
        )

    assert client.metadata[RECEIPT_METADATA_KEY] == prior_receipt
    assert client.mutations == []


@pytest.mark.asyncio
async def test_mixed_base_collection_mutates_only_enabled_l3_text_points() -> None:
    """Same-dimension image/hierarchy/disabled points are outside BM25 scope."""

    client = FakeQdrant(
        [
            _point("text", "eligible text"),
            _point("image", "image caption", content_type="image"),
            _point("page-image", "page image", content_type="page_image"),
            _point("mixed", "mixed content", content_type="mixed"),
            _point("empty-type", "explicit empty type", content_type=""),
            _point("section", "section summary", content_type="text", level=2),
            _point("level-zero", "zero level", content_type="text", level=0),
            _point("level-four", "fourth level", content_type="text", level=4),
            _point("string-level", "string level", content_type="text", level="3"),
            _point("disabled", "disabled text", enabled=False),
        ]
    )
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )

    assert manifest.point_count == 1
    assert manifest.point_ids_sha256 == point_ids_sha256(["text"])
    receipt = await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=True,
    )

    assert receipt["point_count"] == 1
    assert BM25_V2_FIELD in client.points["text"]["vector"]
    for point_id in (
        "image",
        "page-image",
        "mixed",
        "empty-type",
        "section",
        "level-zero",
        "level-four",
        "string-level",
        "disabled",
    ):
        assert BM25_V2_FIELD not in client.points[point_id]["vector"]
        assert "_lexical" not in client.points[point_id]["payload"]
    vector_ids = {
        str(point.id)
        for name, kwargs in client.mutations
        if name == "update_vectors"
        for point in kwargs["points"]
    }
    assert vector_ids == {"text"}


@pytest.mark.asyncio
async def test_postgres_authority_matches_full_active_retrieval_scope(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    class Transaction:
        async def __aenter__(self) -> None:
            return None

        async def __aexit__(self, *_args: Any) -> None:
            return None

    class Connection:
        def transaction(self, **_kwargs: Any) -> Transaction:
            return Transaction()

        async def fetchrow(self, query: str, *args: Any) -> dict[str, Any]:
            captured["dataset_query"] = query
            captured["dataset_args"] = args
            return {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "collection_name": "kb_dataset-a_2",
                "content_revision": 11,
                "index_config": {},
            }

        async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
            captured["segment_query"] = query
            captured["segment_args"] = args
            active = {
                "point_id": "enabled-text",
                "text": "authoritative",
                "segment_enabled": True,
                "segment_status": "completed",
                "document_enabled": True,
                "document_archived": False,
                "document_status": "completed",
                "document_lifecycle_pending": False,
            }
            return [
                active,
                {**active, "point_id": "segment-disabled", "segment_enabled": False},
                {**active, "point_id": "segment-error", "segment_status": "error"},
                {**active, "point_id": "document-disabled", "document_enabled": False},
                {**active, "point_id": "document-archived", "document_archived": True},
                {**active, "point_id": "document-processing", "document_status": "processing"},
                {
                    **active,
                    "point_id": "document-lifecycle-pending",
                    "document_lifecycle_pending": True,
                },
            ]

        async def close(self) -> None:
            return None

    async def connect(_dsn: str) -> Connection:
        return Connection()

    monkeypatch.setattr("scripts.backfill_bm25_v2.asyncpg.connect", connect)
    snapshot = await PostgresBackfillAuthority("postgresql://redacted").snapshot(
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )

    normalized_dataset_query = " ".join(captured["dataset_query"].split())
    normalized_segment_query = " ".join(captured["segment_query"].split())
    assert "is_deleted = FALSE" in normalized_dataset_query
    assert captured["dataset_args"] == ("dataset-a",)
    assert "JOIN documents AS d" in normalized_segment_query
    assert "JOIN datasets AS ds" in normalized_segment_query
    assert "ds.tenant_id = $2" in normalized_segment_query
    assert "ds.collection_name = $3" in normalized_segment_query
    assert "ds.is_deleted = FALSE" in normalized_segment_query
    assert "'_index_deletion_fence'" in normalized_segment_query
    assert "COALESCE(s.enabled, TRUE) = TRUE" in normalized_segment_query
    assert "s.status = 'completed'" in normalized_segment_query
    assert "COALESCE(s.level, 3) = 3" in normalized_segment_query
    assert "COALESCE(s.content_type, 'text') = 'text'" in normalized_segment_query
    assert "COALESCE(d.enabled, TRUE) = TRUE" in normalized_segment_query
    assert "COALESCE(d.archived, FALSE) = FALSE" in normalized_segment_query
    assert "d.status = 'completed'" in normalized_segment_query
    assert "'_document_lifecycle_reindex'" in normalized_segment_query
    assert captured["segment_args"] == (
        "dataset-a",
        "tenant-a",
        "kb_dataset-a_2",
    )
    assert snapshot.point_count == 1
    assert snapshot.point_ids_sha256 == point_ids_sha256(["enabled-text"])


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("mode", "match"),
    [
        ("deleted", "does not exist or is deleted"),
        ("pending", "index deletion is pending"),
        ("malformed", "deletion fence is malformed"),
    ],
)
async def test_postgres_authority_rejects_inactive_or_fenced_dataset(
    monkeypatch: pytest.MonkeyPatch,
    mode: str,
    match: str,
) -> None:
    class Transaction:
        async def __aenter__(self) -> None:
            return None

        async def __aexit__(self, *_args: Any) -> None:
            return None

    class Connection:
        def transaction(self, **_kwargs: Any) -> Transaction:
            return Transaction()

        async def fetchrow(self, _query: str, *_args: Any) -> dict[str, Any] | None:
            if mode == "deleted":
                return None
            marker: Any = {
                "operation": "dataset_delete",
                "target_id": "dataset-a",
                "status": "pending",
                "version": 1,
            }
            if mode == "malformed":
                marker = "invalid"
            return {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "collection_name": "kb_dataset-a_2",
                "content_revision": 11,
                "index_config": {
                    "retrieval": {"_index_deletion_fence": marker}
                },
            }

        async def fetch(self, *_args: Any, **_kwargs: Any) -> list[Any]:
            pytest.fail("inactive dataset must fail before segment authority scan")

        async def close(self) -> None:
            return None

    async def connect(_dsn: str) -> Connection:
        return Connection()

    monkeypatch.setattr("scripts.backfill_bm25_v2.asyncpg.connect", connect)

    with pytest.raises(BackfillError, match=match):
        await PostgresBackfillAuthority("postgresql://redacted").snapshot(
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )


@pytest.mark.integration
@pytest.mark.asyncio
async def test_real_postgres_authority_excludes_inactive_segment_and_document_rows() -> None:
    dsn = os.getenv("KB_BACKFILL_POSTGRES_TEST_DSN", "").strip()
    if not dsn:
        pytest.skip("KB_BACKFILL_POSTGRES_TEST_DSN is not configured")

    probe_id = uuid.uuid4().hex
    dataset_id = f"bm25-authority-dataset-{probe_id}"
    tenant_id = f"bm25-authority-tenant-{probe_id}"
    collection_name = f"kb_bm25_authority_{probe_id}"
    document_ids = {
        state: f"bm25-authority-document-{state}-{probe_id}"
        for state in ("active", "disabled", "archived", "processing", "lifecycle")
    }
    segment_ids = {
        state: f"bm25-authority-segment-{state}-{probe_id}"
        for state in (
            "active",
            "disabled",
            "error",
            "non-l3",
            "non-text",
            "disabled-document",
            "archived-document",
            "processing-document",
            "lifecycle-document",
        )
    }
    active_text = "only this active PostgreSQL row belongs in the authority snapshot"
    connection = await asyncpg.connect(dsn)

    try:
        await connection.execute(
            """
            INSERT INTO datasets (
                dataset_id, name, tenant_id, collection_name, index_config
            ) VALUES ($1, $2, $3, $4, '{}'::jsonb)
            """,
            dataset_id,
            f"BM25 authority probe {probe_id}",
            tenant_id,
            collection_name,
        )
        await connection.executemany(
            """
            INSERT INTO documents (
                document_id, dataset_id, title, status, enabled, archived, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
            """,
            [
                (
                    document_ids["active"],
                    dataset_id,
                    "active",
                    "completed",
                    True,
                    False,
                    "{}",
                ),
                (
                    document_ids["disabled"],
                    dataset_id,
                    "disabled",
                    "completed",
                    False,
                    False,
                    "{}",
                ),
                (
                    document_ids["archived"],
                    dataset_id,
                    "archived",
                    "completed",
                    True,
                    True,
                    "{}",
                ),
                (
                    document_ids["processing"],
                    dataset_id,
                    "processing",
                    "processing",
                    True,
                    False,
                    "{}",
                ),
                (
                    document_ids["lifecycle"],
                    dataset_id,
                    "lifecycle pending",
                    "completed",
                    True,
                    False,
                    json.dumps(
                        {
                            DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                                "status": "pending",
                                "version": 1,
                            }
                        }
                    ),
                ),
            ],
        )
        await connection.executemany(
            """
            INSERT INTO segments (
                segment_id, dataset_id, document_id, position, text, vector_id,
                content_type, enabled, status, level
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            """,
            [
                (
                    segment_ids["active"],
                    dataset_id,
                    document_ids["active"],
                    0,
                    active_text,
                    segment_ids["active"],
                    "text",
                    True,
                    "completed",
                    3,
                ),
                (
                    segment_ids["disabled"],
                    dataset_id,
                    document_ids["active"],
                    1,
                    "disabled segment",
                    segment_ids["disabled"],
                    "text",
                    False,
                    "completed",
                    3,
                ),
                (
                    segment_ids["error"],
                    dataset_id,
                    document_ids["active"],
                    2,
                    "error segment",
                    segment_ids["error"],
                    "text",
                    True,
                    "error",
                    3,
                ),
                (
                    segment_ids["non-l3"],
                    dataset_id,
                    document_ids["active"],
                    3,
                    "non-L3 segment",
                    segment_ids["non-l3"],
                    "text",
                    True,
                    "completed",
                    2,
                ),
                (
                    segment_ids["non-text"],
                    dataset_id,
                    document_ids["active"],
                    4,
                    "non-text segment",
                    segment_ids["non-text"],
                    "image",
                    True,
                    "completed",
                    3,
                ),
                (
                    segment_ids["disabled-document"],
                    dataset_id,
                    document_ids["disabled"],
                    0,
                    "segment from disabled document",
                    segment_ids["disabled-document"],
                    "text",
                    True,
                    "completed",
                    3,
                ),
                (
                    segment_ids["archived-document"],
                    dataset_id,
                    document_ids["archived"],
                    0,
                    "segment from archived document",
                    segment_ids["archived-document"],
                    "text",
                    True,
                    "completed",
                    3,
                ),
                (
                    segment_ids["processing-document"],
                    dataset_id,
                    document_ids["processing"],
                    0,
                    "segment from processing document",
                    segment_ids["processing-document"],
                    "text",
                    True,
                    "completed",
                    3,
                ),
                (
                    segment_ids["lifecycle-document"],
                    dataset_id,
                    document_ids["lifecycle"],
                    0,
                    "segment from lifecycle-pending document",
                    segment_ids["lifecycle-document"],
                    "text",
                    True,
                    "completed",
                    3,
                ),
            ],
        )

        authority = PostgresBackfillAuthority(dsn)
        snapshot = await authority.snapshot(
            collection_name=collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )

        assert snapshot.point_count == 1
        assert snapshot.point_ids_sha256 == point_ids_sha256([segment_ids["active"]])
        assert snapshot.source_text_sha256 == source_text_sha256(
            [(segment_ids["active"], active_text)]
        )

        with pytest.raises(
            BackfillError,
            match="authoritative dataset tenant_id does not match request",
        ):
            await authority.snapshot(
                collection_name=collection_name,
                tenant_id=f"wrong-{tenant_id}",
                dataset_id=dataset_id,
            )
        with pytest.raises(
            BackfillError,
            match="authoritative dataset base collection does not match request",
        ):
            await authority.snapshot(
                collection_name=f"wrong-{collection_name}",
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
    finally:
        try:
            await connection.execute(
                """
                DELETE FROM segments
                WHERE segment_id = ANY($1::varchar[]) AND dataset_id = $2
                """,
                list(segment_ids.values()),
                dataset_id,
            )
            await connection.execute(
                """
                DELETE FROM documents
                WHERE document_id = ANY($1::varchar[]) AND dataset_id = $2
                """,
                list(document_ids.values()),
                dataset_id,
            )
            await connection.execute(
                "DELETE FROM datasets WHERE dataset_id = $1",
                dataset_id,
            )
        finally:
            await connection.close()


@pytest.mark.asyncio
async def test_dry_run_performs_zero_writes_and_reports_resumable_progress() -> None:
    config = _lexical_config()
    client = FakeQdrant(
        [
            _point("a", "already complete", complete=True, config=config),
            _point("b", "still pending", config=config),
        ],
        config=config,
    )
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    receipt = await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=False,
    )

    assert receipt["status"] == "dry_run"
    assert receipt["writes_performed"] == 0
    assert receipt["complete_points"] == 1
    assert receipt["pending_points"] == 1
    assert client.mutations == []


@pytest.mark.asyncio
async def test_apply_updates_only_bm25_v2_and_publishes_exact_receipt_after_verification() -> None:
    config = _lexical_config()
    client = FakeQdrant(
        [
            _point("b", "pending text", config=config),
            _point("a", "complete text", complete=True, config=config),
        ],
        config=config,
    )
    original_dense = deepcopy(client.points["b"]["vector"][""])
    original_legacy = client.points["b"]["vector"]["bm25"]
    client.points["b"]["payload"]["_lexical"] = {"preserve": "nested-marker"}
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )

    receipt = await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=True,
    )

    assert receipt["status"] == "complete"
    assert receipt["exact"] is True
    assert receipt["coverage_percent"] == 100.0
    assert receipt["points_written_this_run"] == 1
    assert client.points["b"]["vector"][""] == original_dense
    assert client.points["b"]["vector"]["bm25"] == original_legacy
    native = client.points["b"]["vector"][BM25_V2_FIELD]
    assert isinstance(native, qmodels.Document)
    assert native.text == "pending text"
    assert client.points["b"]["payload"]["unrelated"] == "preserve-me"
    assert (
        client.points["b"]["payload"]["_lexical"]["bm25_v2_schema_fingerprint"]
        == config.bm25_v2.fingerprint
    )
    assert client.points["b"]["payload"]["_lexical"]["preserve"] == "nested-marker"

    stored = client.metadata[RECEIPT_METADATA_KEY]
    assert stored == {
        "schema_version": 1,
        "status": "complete",
        "collection_name": "kb_dataset-a_2",
        "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
        "filtering_profile_fingerprint": config.filtering.fingerprint,
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "point_count": 2,
        "point_ids_sha256": point_ids_sha256(["a", "b"]),
        "manifest_algorithm": POINT_ID_ALGORITHM,
        "source_text_sha256": source_text_sha256(
            [("a", "complete text"), ("b", "pending text")]
        ),
        "source_text_algorithm": SOURCE_TEXT_ALGORITHM,
        "authority_kind": AUTHORITY_KIND,
        "authority_content_revision": 11,
    }
    vector_call = next(kwargs for name, kwargs in client.mutations if name == "update_vectors")
    assert set(vector_call["points"][0].vector) == {BM25_V2_FIELD}

    client.mutations.clear()
    rerun = await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=True,
    )
    assert rerun["points_written_this_run"] == 0
    assert not any(name in {"update_vectors", "set_payload"} for name, _ in client.mutations)


@pytest.mark.asyncio
async def test_changed_text_with_old_vector_and_marker_is_reencoded() -> None:
    config = _lexical_config()
    client = FakeQdrant(
        [_point("a", "old source", complete=True, config=config)],
        config=config,
    )
    client.points["a"]["payload"]["text"] = "new source"
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )

    result = await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=True,
    )

    assert result["points_written_this_run"] == 1
    native = client.points["a"]["vector"][BM25_V2_FIELD]
    assert isinstance(native, qmodels.Document)
    assert native.text == "new source"
    assert client.points["a"]["payload"]["_lexical"]["source_text_sha256"] == (
        hashlib.sha256(b"new source").hexdigest()
    )


@pytest.mark.asyncio
async def test_manifest_content_or_profile_drift_fails_before_any_write() -> None:
    client = FakeQdrant([_point("a", "original")])
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    client.points["a"]["payload"]["text"] = "changed after approval"

    with pytest.raises(BackfillError, match="source_text_sha256"):
        await run_backfill(
            client,
            authority=FakeAuthority(client),
            manifest=manifest,
            apply=True,
        )

    assert client.mutations == []

    raw = manifest.to_dict()
    raw["point_count"] = 999
    with pytest.raises(BackfillError, match="manifest_sha256"):
        BackfillManifest.from_dict(raw)


@pytest.mark.asyncio
async def test_tenant_dataset_scope_must_cover_every_dataset_point() -> None:
    client = FakeQdrant(
        [
            _point("a", "correct scope"),
            _point("b", "wrong tenant", tenant_id="tenant-b"),
        ]
    )

    with pytest.raises(BackfillError, match="refusing partial scope"):
        await prepare_manifest(
            client,
            authority=FakeAuthority(client),
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )

    assert client.mutations == []

    mixed_collection = FakeQdrant(
        [
            _point("a", "requested dataset"),
            _point("b", "other dataset", dataset_id="dataset-b"),
        ]
    )
    with pytest.raises(BackfillError, match="outside dataset_id"):
        await prepare_manifest(
            mixed_collection,
            authority=FakeAuthority(mixed_collection),
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    assert mixed_collection.mutations == []


@pytest.mark.asyncio
async def test_postgres_authority_rejects_missing_qdrant_segment_and_revision_drift() -> None:
    client = FakeQdrant([_point("a", "only indexed point")])
    with pytest.raises(BackfillError, match="authoritative PostgreSQL segments"):
        await prepare_manifest(
            client,
            authority=FakeAuthority(client, point_ids=["a", "missing-b"]),
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    assert client.mutations == []

    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client, content_revision=11),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    with pytest.raises(BackfillError, match="no longer matches approval"):
        await run_backfill(
            client,
            authority=FakeAuthority(client, content_revision=12),
            manifest=manifest,
            apply=True,
        )
    assert client.mutations == []


@pytest.mark.asyncio
async def test_conflicting_point_fingerprint_and_failed_write_never_publish_receipt() -> None:
    config = _lexical_config()
    conflicted = _point("a", "text", config=config)
    conflicted["payload"]["_lexical"] = {
        "bm25_v2_schema_fingerprint": "sha256:" + "0" * 64,
    }
    client = FakeQdrant([conflicted], config=config)
    with pytest.raises(BackfillError, match="conflicting bm25_v2 schema fingerprint"):
        await prepare_manifest(
            client,
            authority=FakeAuthority(client),
            collection_name="kb_dataset-a_2",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    assert client.mutations == []

    failing = FakeQdrant([_point("a", "text", config=config)], config=config)
    manifest = await prepare_manifest(
        failing,
        authority=FakeAuthority(failing),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    failing.vector_status = SimpleNamespace(status="failed")
    with pytest.raises(BackfillError, match="did not complete"):
        await run_backfill(
            failing,
            authority=FakeAuthority(failing),
            manifest=manifest,
            apply=True,
        )
    assert RECEIPT_METADATA_KEY not in failing.metadata
    assert not any(name == "update_collection" for name, _ in failing.mutations)


@pytest.mark.asyncio
async def test_failed_rerun_leaves_prior_receipt_explicitly_invalidated() -> None:
    config = _lexical_config()
    client = FakeQdrant([_point("a", "text", config=config)], config=config)
    manifest = await prepare_manifest(
        client,
        authority=FakeAuthority(client),
        collection_name="kb_dataset-a_2",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
    )
    await run_backfill(
        client,
        authority=FakeAuthority(client),
        manifest=manifest,
        apply=True,
    )

    client.points["a"]["vector"].pop(BM25_V2_FIELD)
    client.points["a"]["payload"].pop("_lexical")
    client.vector_status = SimpleNamespace(status="failed")
    with pytest.raises(BackfillError, match="did not complete"):
        await run_backfill(
            client,
            authority=FakeAuthority(client),
            manifest=manifest,
            apply=True,
        )

    assert client.metadata[RECEIPT_METADATA_KEY] == {
        "schema_version": 1,
        "status": "invalidated",
        "reason": "backfill_apply",
    }


def test_legacy_sparse_migration_is_explicitly_unsupported_for_v2() -> None:
    from scripts import migrate_sparse_vectors

    assert migrate_sparse_vectors.BM25_V2_SUPPORTED is False
    assert "unsafe for BM25 v2" in (migrate_sparse_vectors.__doc__ or "")
