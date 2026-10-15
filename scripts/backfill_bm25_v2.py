#!/usr/bin/env python3
"""Deterministic, resumable Qdrant-native BM25 v2 shadow backfill.

The command is read-only unless ``run --apply`` is explicitly selected. It
uses PostgreSQL ``segments.vector_id`` as the authoritative base-text point
manifest, then reconciles that manifest with Qdrant. Mutations are limited to
the named ``bm25_v2`` vector, the reserved per-point ``_lexical`` receipt, and—
after exact verification—the collection-level
``knowledge_bm25_v2_backfill`` completion receipt.

Typical flow (stdout from ``plan`` is the manifest):

    uv run --package knowledge-service python scripts/backfill_bm25_v2.py plan \
      --collection kb_example_1024 --tenant-id TENANT --dataset-id DATASET \
      > bm25-v2-manifest.json
    uv run --package knowledge-service python scripts/backfill_bm25_v2.py run \
      --manifest bm25-v2-manifest.json --dry-run
    uv run --package knowledge-service python scripts/backfill_bm25_v2.py run \
      --manifest bm25-v2-manifest.json --apply \
      --confirm-manifest-sha256 sha256:...

``run`` defaults to a true remote dry-run: it performs reads and inference
probes but invokes no Qdrant mutation method.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import re
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol
from urllib.parse import urlsplit

import asyncpg
from knowledge_service.persistence.database import dataset_index_deletion_fence
from knowledge_service.services.knowledge.lexical_config import (
    BM25_V2_AUTHORITY_KIND,
    BM25_V2_FIELD,
    BM25_V2_MODEL,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1,
    REQUIRED_FILTER_PAYLOAD_INDEXES,
    LexicalConfig,
    LexicalConfigError,
)
from qdrant_client import AsyncQdrantClient
from qdrant_client.http import models as qmodels

MANIFEST_SCHEMA_VERSION = 2
MANIFEST_KIND = "knowledge-bm25-v2-shadow-backfill"
AUTHORITY_KIND = BM25_V2_AUTHORITY_KIND
RECEIPT_METADATA_KEY = "knowledge_bm25_v2_backfill"
POINT_ID_ALGORITHM = "sha256(sorted-point-id-newline-v1)"
SOURCE_TEXT_ALGORITHM = "sha256(sorted-point-id-text-sha256-null-newline-v1)"
_SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_HEX_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class BackfillError(RuntimeError):
    """Raised when a backfill cannot prove that proceeding is safe."""


class QdrantBackfillClient(Protocol):
    async def get_collection(self, collection_name: str) -> Any: ...

    async def count(self, **kwargs: Any) -> Any: ...

    async def scroll(self, **kwargs: Any) -> tuple[list[Any], Any]: ...

    async def query_points(self, **kwargs: Any) -> Any: ...

    async def update_vectors(self, **kwargs: Any) -> Any: ...

    async def set_payload(self, **kwargs: Any) -> Any: ...

    async def update_collection(self, **kwargs: Any) -> Any: ...


@dataclass(frozen=True, slots=True)
class AuthoritySnapshot:
    """Immutable PostgreSQL authority for one base text collection."""

    collection_name: str
    tenant_id: str
    dataset_id: str
    content_revision: int
    point_count: int
    point_ids_sha256: str
    source_text_sha256: str
    kind: str = AUTHORITY_KIND

    def __post_init__(self) -> None:
        for name in ("collection_name", "tenant_id", "dataset_id"):
            if getattr(self, name) != _require_nonempty_string(
                getattr(self, name), name=name
            ):
                raise BackfillError(f"authority {name} contains surrounding whitespace")
        if self.kind != AUTHORITY_KIND:
            raise BackfillError("unsupported backfill authority kind")
        for name in ("content_revision", "point_count"):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                raise BackfillError(f"authority {name} must be a non-negative integer")
        if not _HEX_SHA256_RE.fullmatch(self.point_ids_sha256):
            raise BackfillError("authority point_ids_sha256 must be lowercase sha256 hex")
        if not _HEX_SHA256_RE.fullmatch(self.source_text_sha256):
            raise BackfillError("authority source_text_sha256 must be lowercase sha256 hex")


class BackfillAuthority(Protocol):
    async def snapshot(
        self,
        *,
        collection_name: str,
        tenant_id: str,
        dataset_id: str,
    ) -> AuthoritySnapshot: ...


def _postgres_authority_row_is_active(row: dict[str, Any]) -> bool:
    """Defense-in-depth mirror of the SQL lifecycle predicates."""

    return bool(
        row.get("segment_status") == "completed"
        and row.get("document_status") == "completed"
        and row.get("segment_enabled") is not False
        and row.get("document_enabled") is not False
        and row.get("document_archived") is not True
        and row.get("document_lifecycle_pending") is not True
    )


class PostgresBackfillAuthority:
    """Read-only PostgreSQL source of expected base text vector IDs."""

    def __init__(self, dsn: str) -> None:
        self._dsn = _require_nonempty_string(dsn, name="database_dsn")

    async def snapshot(
        self,
        *,
        collection_name: str,
        tenant_id: str,
        dataset_id: str,
    ) -> AuthoritySnapshot:
        connection: Any = None
        try:
            connection = await asyncpg.connect(self._dsn)
            async with connection.transaction(isolation="repeatable_read", readonly=True):
                dataset = await connection.fetchrow(
                    """
                    SELECT dataset_id, tenant_id, collection_name, content_revision,
                           index_config
                    FROM datasets
                    WHERE dataset_id = $1
                      AND is_deleted = FALSE
                    """,
                    dataset_id,
                )
                if dataset is None:
                    raise BackfillError(
                        "authoritative dataset does not exist or is deleted"
                    )
                observed_tenant = str(dataset["tenant_id"] or "")
                observed_collection = str(dataset["collection_name"] or "")
                if observed_tenant != tenant_id:
                    raise BackfillError("authoritative dataset tenant_id does not match request")
                if observed_collection != collection_name:
                    raise BackfillError(
                        "authoritative dataset base collection does not match request"
                    )
                try:
                    deletion_fence = dataset_index_deletion_fence(dict(dataset))
                except RuntimeError as exc:
                    raise BackfillError(
                        "authoritative dataset deletion fence is malformed"
                    ) from exc
                if deletion_fence is not None:
                    raise BackfillError(
                        "authoritative dataset index deletion is pending"
                    )
                rows = await connection.fetch(
                    """
                    SELECT s.vector_id::text AS point_id, s.text,
                           s.enabled AS segment_enabled,
                           s.status AS segment_status,
                           d.enabled AS document_enabled,
                           d.archived AS document_archived,
                           d.status AS document_status,
                           (
                               COALESCE(d.metadata, '{}'::jsonb)
                               ? '_document_lifecycle_reindex'
                           ) AS document_lifecycle_pending
                    FROM segments AS s
                    JOIN documents AS d
                      ON d.document_id = s.document_id
                     AND d.dataset_id = s.dataset_id
                    JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                    WHERE s.dataset_id = $1
                      AND ds.tenant_id = $2
                      AND ds.collection_name = $3
                      AND ds.is_deleted = FALSE
                      AND NOT (
                          COALESCE(ds.index_config -> 'retrieval', '{}'::jsonb)
                          ? '_index_deletion_fence'
                      )
                      AND s.vector_id IS NOT NULL
                      AND s.vector_id <> ''
                      AND COALESCE(s.enabled, TRUE) = TRUE
                      AND s.status = 'completed'
                      AND COALESCE(s.level, 3) = 3
                      AND COALESCE(s.content_type, 'text') = 'text'
                      AND COALESCE(d.enabled, TRUE) = TRUE
                      AND COALESCE(d.archived, FALSE) = FALSE
                      AND d.status = 'completed'
                      AND NOT (
                          COALESCE(d.metadata, '{}'::jsonb)
                          ? '_document_lifecycle_reindex'
                      )
                    ORDER BY s.vector_id::text
                    """,
                    dataset_id,
                    tenant_id,
                    collection_name,
                )
                active_rows: list[dict[str, Any]] = []
                for raw_row in rows:
                    row = dict(raw_row)
                    if _postgres_authority_row_is_active(row):
                        active_rows.append(row)
                rows = active_rows
                point_ids = [str(row["point_id"]) for row in rows]
                if len(point_ids) != len(set(point_ids)):
                    raise BackfillError(
                        "authoritative segments contain duplicate vector_id values"
                    )
                source_entries = [
                    (str(row["point_id"]), str(row["text"] or ""))
                    for row in rows
                ]
                return AuthoritySnapshot(
                    collection_name=collection_name,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    content_revision=int(dataset["content_revision"] or 0),
                    point_count=len(point_ids),
                    point_ids_sha256=point_ids_sha256(point_ids),
                    source_text_sha256=source_text_sha256(source_entries),
                )
        except BackfillError:
            raise
        except Exception as exc:
            # Do not include the DSN or driver message; either can contain
            # credentials or internal network details.
            raise BackfillError(
                f"PostgreSQL authority query failed ({type(exc).__name__})"
            ) from exc
        finally:
            if connection is not None:
                await connection.close()


def _canonical_sha256(value: Any, *, prefix: bool = True) -> str:
    encoded = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    digest = hashlib.sha256(encoded).hexdigest()
    return f"sha256:{digest}" if prefix else digest


def _require_nonempty_string(value: Any, *, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BackfillError(f"{name} must be a non-empty string")
    return value.strip()


@dataclass(frozen=True, slots=True)
class BackfillManifest:
    """Immutable operator approval boundary for exactly one tenant/dataset scope."""

    collection_name: str
    tenant_id: str
    dataset_id: str
    bm25_v2_schema_fingerprint: str
    filtering_profile_fingerprint: str
    lexical_metadata_sha256: str
    authority_kind: str
    authority_content_revision: int
    point_count: int
    point_ids_sha256: str
    source_text_sha256: str
    manifest_sha256: str = ""
    schema_version: int = MANIFEST_SCHEMA_VERSION
    kind: str = MANIFEST_KIND
    manifest_algorithm: str = POINT_ID_ALGORITHM
    source_text_algorithm: str = SOURCE_TEXT_ALGORITHM

    def __post_init__(self) -> None:
        for name in ("collection_name", "tenant_id", "dataset_id"):
            value = getattr(self, name)
            if value != _require_nonempty_string(value, name=name):
                raise BackfillError(f"{name} must not contain surrounding whitespace")
        for name in (
            "bm25_v2_schema_fingerprint",
            "filtering_profile_fingerprint",
            "lexical_metadata_sha256",
        ):
            if not _SHA256_RE.fullmatch(getattr(self, name)):
                raise BackfillError(f"{name} must be a sha256:<64 lowercase hex> fingerprint")
        if isinstance(self.point_count, bool) or not isinstance(self.point_count, int):
            raise BackfillError("point_count must be a non-negative integer")
        if self.point_count < 0:
            raise BackfillError("point_count must be a non-negative integer")
        for name in ("point_ids_sha256", "source_text_sha256"):
            if not _HEX_SHA256_RE.fullmatch(getattr(self, name)):
                raise BackfillError(f"{name} must be a 64-character lowercase hex digest")
        if self.schema_version != MANIFEST_SCHEMA_VERSION or self.kind != MANIFEST_KIND:
            raise BackfillError("unsupported BM25 v2 backfill manifest schema")
        if self.authority_kind != AUTHORITY_KIND:
            raise BackfillError(f"authority_kind must be {AUTHORITY_KIND}")
        if (
            isinstance(self.authority_content_revision, bool)
            or not isinstance(self.authority_content_revision, int)
            or self.authority_content_revision < 0
        ):
            raise BackfillError("authority_content_revision must be non-negative")
        if self.manifest_algorithm != POINT_ID_ALGORITHM:
            raise BackfillError(f"manifest_algorithm must be {POINT_ID_ALGORITHM}")
        if self.source_text_algorithm != SOURCE_TEXT_ALGORITHM:
            raise BackfillError(f"source_text_algorithm must be {SOURCE_TEXT_ALGORITHM}")
        if self.manifest_sha256 and not _SHA256_RE.fullmatch(self.manifest_sha256):
            raise BackfillError("manifest_sha256 must be a sha256:<64 lowercase hex> fingerprint")

    def unsigned_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "kind": self.kind,
            "collection_name": self.collection_name,
            "tenant_id": self.tenant_id,
            "dataset_id": self.dataset_id,
            "bm25_v2_schema_fingerprint": self.bm25_v2_schema_fingerprint,
            "filtering_profile_fingerprint": self.filtering_profile_fingerprint,
            "lexical_metadata_sha256": self.lexical_metadata_sha256,
            "authority_kind": self.authority_kind,
            "authority_content_revision": self.authority_content_revision,
            "point_count": self.point_count,
            "point_ids_sha256": self.point_ids_sha256,
            "manifest_algorithm": self.manifest_algorithm,
            "source_text_sha256": self.source_text_sha256,
            "source_text_algorithm": self.source_text_algorithm,
        }

    @property
    def expected_manifest_sha256(self) -> str:
        return _canonical_sha256(self.unsigned_dict())

    def signed(self) -> BackfillManifest:
        return BackfillManifest(
            **self.unsigned_dict(), manifest_sha256=self.expected_manifest_sha256
        )

    def to_dict(self) -> dict[str, Any]:
        if self.manifest_sha256 != self.expected_manifest_sha256:
            raise BackfillError("manifest_sha256 does not match the canonical manifest")
        return {**self.unsigned_dict(), "manifest_sha256": self.manifest_sha256}

    @classmethod
    def from_dict(cls, value: Any) -> BackfillManifest:
        if not isinstance(value, dict):
            raise BackfillError("manifest must be a JSON object")
        expected_keys = {
            "schema_version",
            "kind",
            "collection_name",
            "tenant_id",
            "dataset_id",
            "bm25_v2_schema_fingerprint",
            "filtering_profile_fingerprint",
            "lexical_metadata_sha256",
            "authority_kind",
            "authority_content_revision",
            "point_count",
            "point_ids_sha256",
            "manifest_algorithm",
            "source_text_sha256",
            "source_text_algorithm",
            "manifest_sha256",
        }
        if set(value) != expected_keys:
            missing = sorted(expected_keys - set(value))
            unknown = sorted(set(value) - expected_keys)
            details = []
            if missing:
                details.append("missing=" + ",".join(missing))
            if unknown:
                details.append("unknown=" + ",".join(unknown))
            raise BackfillError("manifest fields do not match schema (" + "; ".join(details) + ")")
        manifest = cls(**value)
        if manifest.manifest_sha256 != manifest.expected_manifest_sha256:
            raise BackfillError("manifest_sha256 does not match the canonical manifest")
        return manifest


@dataclass(frozen=True, slots=True)
class ScopeScan:
    point_count: int
    point_ids_sha256: str
    source_text_sha256: str
    complete_points: int
    pending_points: int


def _scope_filter(*, tenant_id: str, dataset_id: str) -> qmodels.Filter:
    return qmodels.Filter(
        must=[
            qmodels.FieldCondition(
                key="tenant_id",
                match=qmodels.MatchValue(value=tenant_id),
            ),
            qmodels.FieldCondition(
                key="dataset_id",
                match=qmodels.MatchValue(value=dataset_id),
            ),
        ]
    )


def _lexical_scope_filter(*, tenant_id: str, dataset_id: str) -> qmodels.Filter:
    """Qdrant half of the enabled-L3-text authority contract.

    Legacy text payloads did not persist ``content_type``, ``level`` or
    ``enabled``. Missing values therefore mean text/L3/enabled, matching the
    PostgreSQL COALESCE defaults. Explicit image, hierarchy and disabled
    points are excluded from BM25 v2 receipts while collection ownership is
    still checked across every point separately.
    """

    scoped = _scope_filter(tenant_id=tenant_id, dataset_id=dataset_id)
    return qmodels.Filter(
        must=[
            *(scoped.must or []),
            qmodels.Filter(
                should=[
                    qmodels.FieldCondition(
                        key="content_type",
                        match=qmodels.MatchValue(value="text"),
                    ),
                    qmodels.IsEmptyCondition(
                        is_empty=qmodels.PayloadField(key="content_type")
                    ),
                ]
            ),
            qmodels.Filter(
                should=[
                    qmodels.FieldCondition(
                        key="level",
                        match=qmodels.MatchValue(value=3),
                    ),
                    qmodels.IsEmptyCondition(
                        is_empty=qmodels.PayloadField(key="level")
                    ),
                ]
            ),
            qmodels.Filter(
                should=[
                    qmodels.FieldCondition(
                        key="enabled",
                        match=qmodels.MatchValue(value=True),
                    ),
                    qmodels.IsEmptyCondition(
                        is_empty=qmodels.PayloadField(key="enabled")
                    ),
                ]
            ),
        ],
    )


def _dataset_filter(dataset_id: str) -> qmodels.Filter:
    return qmodels.Filter(
        must=[
            qmodels.FieldCondition(
                key="dataset_id",
                match=qmodels.MatchValue(value=dataset_id),
            )
        ]
    )


def _count_value(result: Any) -> int:
    raw = getattr(result, "count", result)
    if isinstance(raw, bool) or not isinstance(raw, int) or raw < 0:
        raise BackfillError("Qdrant returned an invalid exact count")
    return raw


def _metadata(info: Any) -> dict[str, Any]:
    raw = getattr(getattr(info, "config", None), "metadata", None)
    if not isinstance(raw, dict):
        raise BackfillError("collection is missing Qdrant metadata")
    return dict(raw)


def _keyword_index_ready(
    info: Any,
    field_name: str,
    *,
    require_tenant_partition: bool = False,
) -> bool:
    payload_schema = getattr(info, "payload_schema", None) or {}
    schema = payload_schema.get(field_name)
    data_type = getattr(schema, "data_type", schema)
    keyword = (
        getattr(data_type, "value", data_type)
        == qmodels.PayloadSchemaType.KEYWORD.value
    )
    if not keyword or not require_tenant_partition:
        return keyword
    return getattr(getattr(schema, "params", None), "is_tenant", None) is True


def _modifier_is_idf(params: Any) -> bool:
    modifier = getattr(params, "modifier", None)
    return getattr(modifier, "value", modifier) == qmodels.Modifier.IDF.value


def _validate_collection_profile(
    info: Any,
    *,
    tenant_id: str,
    dataset_id: str,
    expected: BackfillManifest | None = None,
) -> LexicalConfig:
    metadata = _metadata(info)
    expected_scope = {
        "schema_version": 1,
        "tenant_id": _require_nonempty_string(tenant_id, name="tenant_id"),
        "dataset_id": _require_nonempty_string(dataset_id, name="dataset_id"),
    }
    if metadata.get(COLLECTION_SCOPE_METADATA_KEY) != expected_scope:
        raise BackfillError(
            "collection immutable knowledge_scope does not exactly match "
            "the requested tenant_id and dataset_id"
        )
    try:
        config = LexicalConfig.from_collection_metadata(metadata)
    except (LexicalConfigError, TypeError, ValueError) as exc:
        raise BackfillError(f"invalid collection lexical metadata: {exc}") from exc
    if config is None:
        raise BackfillError("collection is missing versioned BM25 v2 metadata")
    if config.active_version != LEXICAL_V1:
        raise BackfillError("shadow backfill requires active_version=lexical_v1")
    if not config.writes_bm25_v2:
        raise BackfillError("shadow backfill requires bm25_v2.shadow_write_enabled=true")

    sparse = getattr(getattr(getattr(info, "config", None), "params", None), "sparse_vectors", None)
    sparse = sparse or {}
    if BM25_V2_FIELD not in sparse or not _modifier_is_idf(sparse[BM25_V2_FIELD]):
        raise BackfillError("collection bm25_v2 sparse field is missing or does not use IDF")
    missing_indexes = [
        field
        for field in REQUIRED_FILTER_PAYLOAD_INDEXES
        if not _keyword_index_ready(
            info,
            field,
            require_tenant_partition=field == "tenant_id",
        )
    ]
    if missing_indexes:
        raise BackfillError(
            "collection is missing required keyword payload indexes: " + ", ".join(missing_indexes)
        )

    if expected is not None:
        comparisons = {
            "bm25_v2_schema_fingerprint": (
                expected.bm25_v2_schema_fingerprint,
                config.bm25_v2.fingerprint,
            ),
            "filtering_profile_fingerprint": (
                expected.filtering_profile_fingerprint,
                config.filtering.fingerprint,
            ),
            "lexical_metadata_sha256": (
                expected.lexical_metadata_sha256,
                _canonical_sha256(metadata["knowledge_lexical"]),
            ),
        }
        mismatches = [name for name, (wanted, actual) in comparisons.items() if wanted != actual]
        if mismatches:
            raise BackfillError(
                "collection profile no longer matches manifest: " + ", ".join(mismatches)
            )
    return config


def _bm25_document(text: str, config: LexicalConfig) -> qmodels.Document:
    if not text.strip():
        raise BackfillError("bm25_v2 cannot encode empty point text")
    try:
        options = qmodels.Bm25Config(
            k=config.bm25_v2.k,
            b=config.bm25_v2.b,
            avg_len=config.bm25_v2.avg_len,
            tokenizer=qmodels.TokenizerType(config.bm25_v2.tokenizer),
            language=config.bm25_v2.language,
            lowercase=config.bm25_v2.lowercase,
            ascii_folding=config.bm25_v2.ascii_folding,
            stopwords=None,
            stemmer=None,
            min_token_len=config.bm25_v2.min_token_len,
            max_token_len=config.bm25_v2.max_token_len,
        )
        return qmodels.Document(text=text, model=BM25_V2_MODEL, options=options)
    except Exception as exc:
        raise BackfillError(f"invalid Qdrant-native BM25 v2 profile: {exc}") from exc


async def _probe_native_bm25(
    client: QdrantBackfillClient,
    *,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
    config: LexicalConfig,
) -> None:
    try:
        await client.query_points(
            collection_name=collection_name,
            query=_bm25_document("bm25 v2 read-only capability probe", config),
            using=BM25_V2_FIELD,
            query_filter=_scope_filter(tenant_id=tenant_id, dataset_id=dataset_id),
            limit=1,
            with_payload=False,
        )
    except Exception as exc:
        raise BackfillError(f"Qdrant-native BM25 capability probe failed: {exc}") from exc


def _point_state(
    record: Any,
    *,
    tenant_id: str,
    dataset_id: str,
    config: LexicalConfig,
) -> tuple[str, bool]:
    payload = getattr(record, "payload", None)
    if not isinstance(payload, dict):
        raise BackfillError(f"point {record.id!s} has no object payload")
    if payload.get("tenant_id") != tenant_id or payload.get("dataset_id") != dataset_id:
        raise BackfillError(f"point {record.id!s} escaped the requested tenant/dataset scope")
    raw_content_type = payload.get("content_type")
    content_type = "text" if raw_content_type is None else raw_content_type
    if content_type != "text":
        raise BackfillError(f"point {record.id!s} escaped the enabled L3 text scope")
    raw_level = payload.get("level")
    level = 3 if raw_level is None else raw_level
    if isinstance(level, bool) or not isinstance(level, int):
        raise BackfillError(f"point {record.id!s} has an invalid hierarchy level")
    raw_enabled = payload.get("enabled")
    enabled = True if raw_enabled is None else raw_enabled
    if level != 3 or not isinstance(enabled, bool) or not enabled:
        raise BackfillError(f"point {record.id!s} escaped the enabled L3 text scope")
    text = payload.get("text")
    if not isinstance(text, str) or not text.strip():
        raise BackfillError(f"point {record.id!s} has empty or non-string text")

    lexical = payload.get("_lexical")
    lexical = lexical if isinstance(lexical, dict) else {}
    observed_schema = lexical.get("bm25_v2_schema_fingerprint")
    observed_filtering = lexical.get("filtering_profile_fingerprint")
    observed_source_text = lexical.get("source_text_sha256")
    expected_source_text = hashlib.sha256(text.encode("utf-8")).hexdigest()
    if observed_schema is not None and observed_schema != config.bm25_v2.fingerprint:
        raise BackfillError(f"point {record.id!s} has a conflicting bm25_v2 schema fingerprint")
    if observed_filtering is not None and observed_filtering != config.filtering.fingerprint:
        raise BackfillError(f"point {record.id!s} has a conflicting filtering fingerprint")

    vectors = getattr(record, "vector", None)
    has_vector = isinstance(vectors, dict) and BM25_V2_FIELD in vectors
    versions = lexical.get("versions")
    versioned = isinstance(versions, list) and {"lexical_v1", "bm25_v2"}.issubset(versions)
    complete = bool(
        has_vector
        and observed_schema == config.bm25_v2.fingerprint
        and observed_filtering == config.filtering.fingerprint
        and observed_source_text == expected_source_text
        and versioned
    )
    return text, complete


def point_ids_sha256(point_ids: Sequence[str]) -> str:
    encoded = "".join(f"{point_id}\n" for point_id in sorted(map(str, point_ids)))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def source_text_sha256(entries: Sequence[tuple[str, str]]) -> str:
    lines = []
    for point_id, text in sorted(entries, key=lambda item: item[0]):
        text_digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
        lines.append(f"{point_id}\0{text_digest}\n")
    return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()


async def _authority_snapshot(
    authority: BackfillAuthority,
    *,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
) -> AuthoritySnapshot:
    snapshot = await authority.snapshot(
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
    )
    if not isinstance(snapshot, AuthoritySnapshot):
        raise BackfillError("authority returned an invalid snapshot")
    expected_identity = (collection_name, tenant_id, dataset_id)
    observed_identity = (
        snapshot.collection_name,
        snapshot.tenant_id,
        snapshot.dataset_id,
    )
    if observed_identity != expected_identity:
        raise BackfillError("authority snapshot identity does not match request")
    return snapshot


def _assert_authority_stable(
    before: AuthoritySnapshot,
    after: AuthoritySnapshot,
) -> None:
    if before != after:
        raise BackfillError(
            "authoritative PostgreSQL segment manifest changed during verification"
        )


def _assert_scan_matches_authority(
    scan: ScopeScan,
    authority: AuthoritySnapshot,
) -> None:
    mismatches = []
    if scan.point_count != authority.point_count:
        mismatches.append("point_count")
    if scan.point_ids_sha256 != authority.point_ids_sha256:
        mismatches.append("point_ids_sha256")
    if scan.source_text_sha256 != authority.source_text_sha256:
        mismatches.append("source_text_sha256")
    if mismatches:
        raise BackfillError(
            "Qdrant does not match authoritative PostgreSQL segments: "
            + ", ".join(mismatches)
        )


def _assert_authority_matches_manifest(
    authority: AuthoritySnapshot,
    manifest: BackfillManifest,
) -> None:
    expected = {
        "kind": manifest.authority_kind,
        "content_revision": manifest.authority_content_revision,
        "point_count": manifest.point_count,
        "point_ids_sha256": manifest.point_ids_sha256,
        "source_text_sha256": manifest.source_text_sha256,
        "collection_name": manifest.collection_name,
        "tenant_id": manifest.tenant_id,
        "dataset_id": manifest.dataset_id,
    }
    observed = {
        "kind": authority.kind,
        "content_revision": authority.content_revision,
        "point_count": authority.point_count,
        "point_ids_sha256": authority.point_ids_sha256,
        "source_text_sha256": authority.source_text_sha256,
        "collection_name": authority.collection_name,
        "tenant_id": authority.tenant_id,
        "dataset_id": authority.dataset_id,
    }
    mismatches = sorted(key for key, value in expected.items() if observed[key] != value)
    if mismatches:
        raise BackfillError(
            "authoritative PostgreSQL manifest no longer matches approval: "
            + ", ".join(mismatches)
        )


async def _scan_scope(
    client: QdrantBackfillClient,
    *,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
    config: LexicalConfig,
    batch_size: int,
) -> ScopeScan:
    offset: Any = None
    point_ids: list[str] = []
    source_entries: list[tuple[str, str]] = []
    seen: set[tuple[type[Any], str]] = set()
    complete = 0

    while True:
        records, next_offset = await client.scroll(
            collection_name=collection_name,
            scroll_filter=_lexical_scope_filter(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            ),
            limit=batch_size,
            offset=offset,
            with_payload=[
                "tenant_id",
                "dataset_id",
                "content_type",
                "level",
                "enabled",
                "text",
                "_lexical",
            ],
            with_vectors=[BM25_V2_FIELD],
        )
        for record in records:
            raw_id = getattr(record, "id", None)
            if raw_id is None:
                raise BackfillError("Qdrant returned a point without an id")
            identity = (type(raw_id), str(raw_id))
            if identity in seen:
                raise BackfillError(f"Qdrant scroll returned duplicate point id {raw_id!s}")
            seen.add(identity)
            text, is_complete = _point_state(
                record,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                config=config,
            )
            point_ids.append(str(raw_id))
            source_entries.append((str(raw_id), text))
            complete += int(is_complete)
        if next_offset is None:
            break
        if not records or next_offset == offset:
            raise BackfillError("Qdrant scroll pagination did not make progress")
        offset = next_offset

    count = len(point_ids)
    return ScopeScan(
        point_count=count,
        point_ids_sha256=point_ids_sha256(point_ids),
        source_text_sha256=source_text_sha256(source_entries),
        complete_points=complete,
        pending_points=count - complete,
    )


async def _validate_exact_scope_counts(
    client: QdrantBackfillClient,
    *,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
    scanned_count: int,
) -> None:
    dataset_count = _count_value(
        await client.count(
            collection_name=collection_name,
            count_filter=_dataset_filter(dataset_id),
            exact=True,
        )
    )
    scoped_count = _count_value(
        await client.count(
            collection_name=collection_name,
            count_filter=_scope_filter(tenant_id=tenant_id, dataset_id=dataset_id),
            exact=True,
        )
    )
    lexical_scoped_count = _count_value(
        await client.count(
            collection_name=collection_name,
            count_filter=_lexical_scope_filter(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            ),
            exact=True,
        )
    )
    collection_count = _count_value(
        await client.count(
            collection_name=collection_name,
            exact=True,
        )
    )
    if collection_count != dataset_count:
        raise BackfillError(
            "collection contains points outside dataset_id or without dataset_id; "
            "refusing a collection-level receipt "
            f"(collection={collection_count}, dataset={dataset_count})"
        )
    if dataset_count != scoped_count:
        raise BackfillError(
            "dataset points are missing or conflict with tenant_id; refusing partial scope "
            f"(dataset={dataset_count}, tenant+dataset={scoped_count})"
        )
    if lexical_scoped_count != scanned_count:
        raise BackfillError(
            "exact enabled-L3-text count changed during scan "
            f"(count={lexical_scoped_count}, scanned={scanned_count})"
        )


async def prepare_manifest(
    client: QdrantBackfillClient,
    *,
    authority: BackfillAuthority,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
    batch_size: int = 256,
) -> BackfillManifest:
    _validate_batch_size(batch_size)
    collection_name = _require_nonempty_string(collection_name, name="collection_name")
    tenant_id = _require_nonempty_string(tenant_id, name="tenant_id")
    dataset_id = _require_nonempty_string(dataset_id, name="dataset_id")
    authority_before = await _authority_snapshot(
        authority,
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
    )
    info = await client.get_collection(collection_name)
    config = _validate_collection_profile(
        info,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
    )
    await _probe_native_bm25(
        client,
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
        config=config,
    )
    scan = await _scan_scope(
        client,
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
        config=config,
        batch_size=batch_size,
    )
    await _validate_exact_scope_counts(
        client,
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
        scanned_count=scan.point_count,
    )
    _assert_scan_matches_authority(scan, authority_before)
    authority_after = await _authority_snapshot(
        authority,
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
    )
    _assert_authority_stable(authority_before, authority_after)
    lexical_metadata = _metadata(info)["knowledge_lexical"]
    return BackfillManifest(
        collection_name=collection_name,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
        bm25_v2_schema_fingerprint=config.bm25_v2.fingerprint,
        filtering_profile_fingerprint=config.filtering.fingerprint,
        lexical_metadata_sha256=_canonical_sha256(lexical_metadata),
        authority_kind=authority_before.kind,
        authority_content_revision=authority_before.content_revision,
        point_count=authority_before.point_count,
        point_ids_sha256=authority_before.point_ids_sha256,
        source_text_sha256=authority_before.source_text_sha256,
    ).signed()


def _assert_scan_matches_manifest(scan: ScopeScan, manifest: BackfillManifest) -> None:
    mismatches = []
    if scan.point_count != manifest.point_count:
        mismatches.append("point_count")
    if scan.point_ids_sha256 != manifest.point_ids_sha256:
        mismatches.append("point_ids_sha256")
    if scan.source_text_sha256 != manifest.source_text_sha256:
        mismatches.append("source_text_sha256")
    if mismatches:
        raise BackfillError(
            "collection contents no longer match manifest: " + ", ".join(mismatches)
        )


def _write_completed(result: Any, *, operation: str) -> None:
    if result is True:
        return
    status = getattr(result, "status", None)
    status_value = getattr(status, "value", status)
    if str(status_value).lower() != "completed":
        raise BackfillError(f"Qdrant {operation} did not complete (status={status_value!s})")


async def _apply_missing_points(
    client: QdrantBackfillClient,
    *,
    manifest: BackfillManifest,
    config: LexicalConfig,
    batch_size: int,
) -> int:
    offset: Any = None
    written = 0
    while True:
        records, next_offset = await client.scroll(
            collection_name=manifest.collection_name,
            scroll_filter=_lexical_scope_filter(
                tenant_id=manifest.tenant_id,
                dataset_id=manifest.dataset_id,
            ),
            limit=batch_size,
            offset=offset,
            with_payload=[
                "tenant_id",
                "dataset_id",
                "content_type",
                "level",
                "enabled",
                "text",
                "_lexical",
            ],
            with_vectors=[BM25_V2_FIELD],
        )
        pending: list[tuple[Any, str, dict[str, Any]]] = []
        for record in records:
            text, complete = _point_state(
                record,
                tenant_id=manifest.tenant_id,
                dataset_id=manifest.dataset_id,
                config=config,
            )
            if not complete:
                payload = getattr(record, "payload", None) or {}
                existing_lexical = payload.get("_lexical")
                pending.append(
                    (
                        record.id,
                        text,
                        dict(existing_lexical)
                        if isinstance(existing_lexical, dict)
                        else {},
                    )
                )
        if pending:
            vector_result = await client.update_vectors(
                collection_name=manifest.collection_name,
                points=[
                    qmodels.PointVectors(
                        id=point_id,
                        vector={BM25_V2_FIELD: _bm25_document(text, config)},
                    )
                    for point_id, text, _existing_lexical in pending
                ],
                wait=True,
            )
            _write_completed(vector_result, operation="bm25_v2 vector update")
            for point_id, _text, existing_lexical in pending:
                versions = existing_lexical.get("versions")
                preserved_versions = (
                    [str(value) for value in versions]
                    if isinstance(versions, list)
                    else []
                )
                marker_result = await client.set_payload(
                    collection_name=manifest.collection_name,
                    payload={
                        "_lexical": {
                            **existing_lexical,
                            "versions": list(
                                dict.fromkeys(
                                    [*preserved_versions, "lexical_v1", "bm25_v2"]
                                )
                            ),
                            "bm25_v2_schema_fingerprint": (
                                config.bm25_v2.fingerprint
                            ),
                            "filtering_profile_fingerprint": (
                                config.filtering.fingerprint
                            ),
                            "source_text_sha256": hashlib.sha256(
                                _text.encode("utf-8")
                            ).hexdigest(),
                        }
                    },
                    points=[point_id],
                    wait=True,
                )
                _write_completed(
                    marker_result,
                    operation="bm25_v2 point receipt update",
                )
            written += len(pending)
        if next_offset is None:
            break
        if not records or next_offset == offset:
            raise BackfillError("Qdrant scroll pagination did not make progress during apply")
        offset = next_offset
    return written


def _completion_receipt(manifest: BackfillManifest) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "status": "complete",
        "collection_name": manifest.collection_name,
        "bm25_v2_schema_fingerprint": manifest.bm25_v2_schema_fingerprint,
        "filtering_profile_fingerprint": manifest.filtering_profile_fingerprint,
        "dataset_id": manifest.dataset_id,
        "tenant_id": manifest.tenant_id,
        "point_count": manifest.point_count,
        "point_ids_sha256": manifest.point_ids_sha256,
        "manifest_algorithm": POINT_ID_ALGORITHM,
        "source_text_sha256": manifest.source_text_sha256,
        "source_text_algorithm": SOURCE_TEXT_ALGORITHM,
        "authority_kind": manifest.authority_kind,
        "authority_content_revision": manifest.authority_content_revision,
    }


async def _publish_completion_receipt(
    client: QdrantBackfillClient,
    *,
    manifest: BackfillManifest,
) -> dict[str, Any]:
    info = await client.get_collection(manifest.collection_name)
    _validate_collection_profile(
        info,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        expected=manifest,
    )
    receipt = _completion_receipt(manifest)
    updated = await client.update_collection(
        collection_name=manifest.collection_name,
        metadata={RECEIPT_METADATA_KEY: receipt},
    )
    if updated is not True:
        raise BackfillError("Qdrant rejected the BM25 v2 completion receipt")
    refreshed = await client.get_collection(manifest.collection_name)
    _validate_collection_profile(
        refreshed,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        expected=manifest,
    )
    verified = _metadata(refreshed).get(RECEIPT_METADATA_KEY)
    if verified != receipt:
        raise BackfillError("BM25 v2 completion receipt did not converge")
    return receipt


async def _clear_completion_receipt(
    client: QdrantBackfillClient,
    *,
    collection_name: str,
    tenant_id: str,
    dataset_id: str,
) -> None:
    info = await client.get_collection(collection_name)
    _validate_collection_profile(
        info,
        tenant_id=tenant_id,
        dataset_id=dataset_id,
    )
    metadata = _metadata(info)
    receipt = metadata.get(RECEIPT_METADATA_KEY)
    if not isinstance(receipt, dict) or receipt.get("status") != "complete":
        return
    invalidated = {
        "schema_version": 1,
        "status": "invalidated",
        "reason": "backfill_apply",
    }
    updated = await client.update_collection(
        collection_name=collection_name,
        metadata={RECEIPT_METADATA_KEY: invalidated},
    )
    if updated is not True:
        raise BackfillError("failed to clear an invalid BM25 v2 completion receipt")
    observed = _metadata(await client.get_collection(collection_name)).get(
        RECEIPT_METADATA_KEY
    )
    if observed != invalidated:
        raise BackfillError("BM25 v2 completion receipt invalidation did not converge")


async def run_backfill(
    client: QdrantBackfillClient,
    *,
    authority: BackfillAuthority,
    manifest: BackfillManifest,
    apply: bool = False,
    batch_size: int = 256,
) -> dict[str, Any]:
    """Validate or apply one manifest; dry-run performs zero remote writes.

    Active BM25 v2 serving remains hard-disabled. The exact pre/post authority
    snapshots and completion receipt make this command suitable for shadow
    evaluation, but they are not a cross-store writer-exclusion protocol.
    """

    _validate_batch_size(batch_size)
    if manifest.manifest_sha256 != manifest.expected_manifest_sha256:
        raise BackfillError("manifest_sha256 does not match the canonical manifest")
    authority_before = await _authority_snapshot(
        authority,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
    )
    _assert_authority_matches_manifest(authority_before, manifest)
    info = await client.get_collection(manifest.collection_name)
    config = _validate_collection_profile(
        info,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        expected=manifest,
    )
    await _probe_native_bm25(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        config=config,
    )
    before = await _scan_scope(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        config=config,
        batch_size=batch_size,
    )
    await _validate_exact_scope_counts(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        scanned_count=before.point_count,
    )
    _assert_scan_matches_manifest(before, manifest)
    _assert_scan_matches_authority(before, authority_before)
    authority_after_scan = await _authority_snapshot(
        authority,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
    )
    _assert_authority_stable(authority_before, authority_after_scan)

    if not apply:
        return {
            "schema_version": 1,
            "status": "dry_run",
            "writes_performed": 0,
            "collection_name": manifest.collection_name,
            "tenant_id": manifest.tenant_id,
            "dataset_id": manifest.dataset_id,
            "point_count": before.point_count,
            "complete_points": before.complete_points,
            "pending_points": before.pending_points,
            "manifest_sha256": manifest.manifest_sha256,
        }

    # Invalidate a prior receipt before the first mutation. If this run fails,
    # active queries remain blocked until an exact rerun publishes a new signed
    # receipt instead of trusting stale collection metadata.
    await _clear_completion_receipt(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
    )
    written = await _apply_missing_points(
        client,
        manifest=manifest,
        config=config,
        batch_size=batch_size,
    )

    # Verify all point state and source identity before publishing the gate used
    # by runtime cutover. This deliberately does not treat a count estimate or
    # a successful write acknowledgement as completion.
    after = await _scan_scope(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        config=config,
        batch_size=batch_size,
    )
    await _validate_exact_scope_counts(
        client,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
        scanned_count=after.point_count,
    )
    _assert_scan_matches_manifest(after, manifest)
    _assert_scan_matches_authority(after, authority_before)
    authority_after_apply = await _authority_snapshot(
        authority,
        collection_name=manifest.collection_name,
        tenant_id=manifest.tenant_id,
        dataset_id=manifest.dataset_id,
    )
    _assert_authority_stable(authority_before, authority_after_apply)
    if after.complete_points != manifest.point_count or after.pending_points != 0:
        raise BackfillError(
            "BM25 v2 exact coverage verification failed "
            f"({after.complete_points}/{manifest.point_count})"
        )
    # Close the narrow verification/publication race as far as the Qdrant API
    # allows. Runtime writers independently replace this receipt with a
    # fail-closed sentinel before every later central write/delete.
    try:
        receipt = await _publish_completion_receipt(client, manifest=manifest)
        post_receipt = await _scan_scope(
            client,
            collection_name=manifest.collection_name,
            tenant_id=manifest.tenant_id,
            dataset_id=manifest.dataset_id,
            config=config,
            batch_size=batch_size,
        )
        await _validate_exact_scope_counts(
            client,
            collection_name=manifest.collection_name,
            tenant_id=manifest.tenant_id,
            dataset_id=manifest.dataset_id,
            scanned_count=post_receipt.point_count,
        )
        _assert_scan_matches_manifest(post_receipt, manifest)
        _assert_scan_matches_authority(post_receipt, authority_before)
        authority_after_receipt = await _authority_snapshot(
            authority,
            collection_name=manifest.collection_name,
            tenant_id=manifest.tenant_id,
            dataset_id=manifest.dataset_id,
        )
        _assert_authority_stable(authority_before, authority_after_receipt)
        if post_receipt.complete_points != manifest.point_count:
            raise BackfillError("BM25 v2 coverage changed after receipt publication")
        current_receipt = _metadata(await client.get_collection(manifest.collection_name)).get(
            RECEIPT_METADATA_KEY
        )
        if current_receipt != receipt:
            raise BackfillError("BM25 v2 receipt changed during final verification")
    except Exception:
        await _clear_completion_receipt(
            client,
            collection_name=manifest.collection_name,
            tenant_id=manifest.tenant_id,
            dataset_id=manifest.dataset_id,
        )
        raise

    return {
        **receipt,
        "exact": True,
        "coverage_percent": 100.0,
        "points_written_this_run": written,
        "manifest_sha256": manifest.manifest_sha256,
    }


def load_manifest(path: str | Path) -> BackfillManifest:
    try:
        raw = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BackfillError(f"could not read manifest: {exc}") from exc
    return BackfillManifest.from_dict(raw)


def _validate_batch_size(value: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 1000:
        raise BackfillError("batch_size must be between 1 and 1000")


def _safe_qdrant_url(value: str) -> str:
    url = _require_nonempty_string(value, name="qdrant_url")
    parsed = urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise BackfillError("qdrant_url must be an absolute http(s) URL")
    if parsed.username or parsed.password:
        raise BackfillError("qdrant_url must not contain embedded credentials")
    return url


def _client_from_args(args: argparse.Namespace) -> AsyncQdrantClient:
    url = _safe_qdrant_url(args.qdrant_url)
    api_key = os.getenv(args.api_key_env) or None
    return AsyncQdrantClient(
        url=url,
        api_key=api_key,
        timeout=args.timeout,
        cloud_inference=True,
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Plan or execute a fail-closed Qdrant-native BM25 v2 shadow backfill"
    )
    parser.add_argument(
        "--qdrant-url",
        default=os.getenv("QDRANT_URL", "http://localhost:6333"),
        help="Qdrant URL (default: QDRANT_URL or localhost; credentials are rejected)",
    )
    parser.add_argument(
        "--api-key-env",
        default="QDRANT_API_KEY",
        help="environment variable holding the Qdrant API key (value is never printed)",
    )
    parser.add_argument(
        "--database-dsn-env",
        default="KNOWLEDGE_DATABASE__DSN",
        help=(
            "environment variable holding the PostgreSQL DSN used for the "
            "authoritative segment manifest (value is never printed)"
        ),
    )
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--batch-size", type=int, default=256)
    subparsers = parser.add_subparsers(dest="command", required=True)

    plan = subparsers.add_parser("plan", help="read-only scan and print a signed manifest")
    plan.add_argument("--collection", required=True)
    plan.add_argument("--tenant-id", required=True)
    plan.add_argument("--dataset-id", required=True)

    run = subparsers.add_parser("run", help="validate a manifest; dry-run is the default")
    run.add_argument("--manifest", required=True)
    mode = run.add_mutually_exclusive_group()
    mode.add_argument(
        "--dry-run",
        action="store_true",
        help="perform the default read-only validation explicitly",
    )
    mode.add_argument(
        "--apply",
        action="store_true",
        help="perform idempotent bm25_v2 writes and publish an exact completion receipt",
    )
    run.add_argument(
        "--confirm-manifest-sha256",
        help="required with --apply; must exactly match manifest_sha256",
    )
    return parser


async def _async_main(args: argparse.Namespace) -> dict[str, Any]:
    _validate_batch_size(args.batch_size)
    if args.timeout < 1 or args.timeout > 3600:
        raise BackfillError("timeout must be between 1 and 3600 seconds")
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args.api_key_env):
        raise BackfillError("api-key-env must be an environment variable name")
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args.database_dsn_env):
        raise BackfillError("database-dsn-env must be an environment variable name")
    database_dsn = os.getenv(args.database_dsn_env)
    if not database_dsn:
        raise BackfillError(
            f"PostgreSQL authority requires the {args.database_dsn_env} environment variable"
        )
    authority = PostgresBackfillAuthority(database_dsn)
    client = _client_from_args(args)
    try:
        if args.command == "plan":
            manifest = await prepare_manifest(
                client,
                authority=authority,
                collection_name=args.collection,
                tenant_id=args.tenant_id,
                dataset_id=args.dataset_id,
                batch_size=args.batch_size,
            )
            return manifest.to_dict()

        manifest = load_manifest(args.manifest)
        if args.apply and args.confirm_manifest_sha256 != manifest.manifest_sha256:
            raise BackfillError(
                "--apply requires --confirm-manifest-sha256 equal to the signed manifest"
            )
        if not args.apply and args.confirm_manifest_sha256:
            raise BackfillError("--confirm-manifest-sha256 is only valid with --apply")
        return await run_backfill(
            client,
            authority=authority,
            manifest=manifest,
            apply=args.apply,
            batch_size=args.batch_size,
        )
    finally:
        await client.close()


def main() -> int:
    try:
        result = asyncio.run(_async_main(_parser().parse_args()))
    except (BackfillError, OSError) as exc:
        print(json.dumps({"status": "failed", "error": str(exc)}, sort_keys=True), file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
