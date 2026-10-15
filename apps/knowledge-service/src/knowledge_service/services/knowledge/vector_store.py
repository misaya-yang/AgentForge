from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import logging
import math
import re
import time
import uuid
from collections.abc import Sequence
from contextvars import Token
from dataclasses import dataclass
from typing import Any

from .lexical_config import (
    BM25_V2_AUTHORITY_KIND,
    BM25_V2_BACKFILL_METADATA_KEY,
    BM25_V2_FIELD,
    BM25_V2_MODEL,
    COLLECTION_METADATA_KEY,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1,
    LEXICAL_V1_FIELD,
    QDRANT_CLIENT_VERSION,
    REQUIRED_FILTER_PAYLOAD_INDEXES,
    STRICT_FILTER_PAYLOAD_INDEXES,
    LexicalConfig,
    LexicalConfigError,
)
from .retrieval import text_to_sparse_vector
from .vector_store_contracts import _INTERACTIVE_DEADLINE
from .vector_store_contracts import CollectionReadAuthorityError as CollectionReadAuthorityError
from .vector_store_contracts import VectorStoreError as VectorStoreError
from .vector_store_contracts import (
    remaining_interactive_budget_seconds as remaining_interactive_budget_seconds,
)
from .vector_store_metrics import vector_store_metrics

logger = logging.getLogger(__name__)

_EMPTY_SPARSE_INDEX = 0

try:
    from qdrant_client.async_qdrant_client import AsyncQdrantClient
    from qdrant_client.http import models as qmodels

    HAS_QDRANT = True
except ImportError:  # pragma: no cover
    AsyncQdrantClient = None
    qmodels = None
    HAS_QDRANT = False


@dataclass(frozen=True)
class VectorSearchHit:
    point_id: str
    score: float
    payload: dict[str, Any]
    vector: list[float] | None = None


class VectorStoreConfig:
    """Configuration for VectorStore with adaptive batch sizes."""

    # Adaptive batch sizes based on document size
    SMALL_BATCH_THRESHOLD = 50  # chunks <= 50
    MEDIUM_BATCH_THRESHOLD = 200  # chunks <= 200
    LARGE_BATCH_THRESHOLD = 500  # chunks <= 500

    BATCH_SIZE_SMALL = 32
    BATCH_SIZE_MEDIUM = 16
    BATCH_SIZE_LARGE = 8
    BATCH_SIZE_XLARGE = 4

    @classmethod
    def get_batch_size(cls, total_chunks: int) -> int:
        """Get optimal batch size based on total chunks."""
        if total_chunks <= cls.SMALL_BATCH_THRESHOLD:
            return cls.BATCH_SIZE_SMALL
        elif total_chunks <= cls.MEDIUM_BATCH_THRESHOLD:
            return cls.BATCH_SIZE_MEDIUM
        elif total_chunks <= cls.LARGE_BATCH_THRESHOLD:
            return cls.BATCH_SIZE_LARGE
        else:
            return cls.BATCH_SIZE_XLARGE


def _sanitize_collection_name(name: str) -> str:
    # Qdrant allows letters/digits/_/-; keep it stable and readable.
    name = name.strip()
    name = re.sub(r"[^a-zA-Z0-9_-]+", "_", name)
    name = re.sub(r"_+", "_", name).strip("_")
    return name or "kb"


class VectorStore:
    def __init__(
        self,
        url: str,
        api_key: str | None = None,
        timeout_seconds: float = 30.0,
        prefer_grpc: bool = False,
        max_retries: int = 3,
        retry_base_delay: float = 0.5,
        bm25_v2_enabled: bool = True,
        bm25_v2_capability_ttl_seconds: float = 300.0,
        bm25_v2_readiness_ttl_seconds: float = 0.0,
        dataset_write_lease: Any | None = None,
        interactive_deadline_seconds: float = 3.0,
        interactive_max_retries: int = 2,
        health_receipt_ttl_seconds: float = 2.0,
    ):
        if not HAS_QDRANT:
            raise VectorStoreError("qdrant-client is not installed. Run: pip install qdrant-client")
        self.url = url
        self.api_key = api_key
        self.timeout_seconds = timeout_seconds
        self.interactive_deadline_seconds = max(
            float(interactive_deadline_seconds or 3.0), 0.1
        )
        self.interactive_max_retries = min(
            max(int(interactive_max_retries or 1), 1), 2
        )
        self.health_receipt_ttl_seconds = max(
            float(health_receipt_ttl_seconds or 0.0), 0.0
        )
        self._health_success_until = 0.0
        self._health_failure_until = 0.0
        self.prefer_grpc = prefer_grpc
        self.max_retries = max(1, int(max_retries or 1))
        self.retry_base_delay = float(retry_base_delay or 0.5)
        self.bm25_v2_enabled = bool(bm25_v2_enabled)
        self.bm25_v2_capability_ttl_seconds = max(
            float(bm25_v2_capability_ttl_seconds or 0.0), 0.0
        )
        self.bm25_v2_readiness_ttl_seconds = max(
            float(bm25_v2_readiness_ttl_seconds or 0.0),
            0.0,
        )
        self._dataset_write_lease = dataset_write_lease
        self._collection_dims: dict[str, int] = {}  # Cache: collection_name → dimension
        # SPO-04 / K1: short-TTL collection metadata cache with write
        # invalidation, so each retrieve pays at most ONE get_collection.
        self._collection_info_cache: dict[str, tuple[float, Any]] = {}
        self._collection_info_ttl_s = 30.0
        self._sparse_collections: set[str] = set()
        self._sparse_readiness: dict[str, bool] = {}
        self._bm25_v2_capability_receipts: dict[str, float] = {}
        self._bm25_v2_capability_lock = asyncio.Lock()
        self._bm25_v2_readiness_cache: dict[
            tuple[str, ...], tuple[float, dict[str, Any]]
        ] = {}
        self._bm25_v2_readiness_inflight: dict[
            tuple[str, ...], asyncio.Task[dict[str, Any]]
        ] = {}
        self._bm25_v2_readiness_lock = asyncio.Lock()
        self._bm25_v2_shadow_write_failures = 0
        self._bm25_v2_shadow_write_failure_points = 0
        self._client = AsyncQdrantClient(
            url=url,
            api_key=api_key,
            prefer_grpc=prefer_grpc,
            timeout=timeout_seconds,
            # qdrant/bm25 is executed by Qdrant. Keeping inference server-side
            # avoids an undeclared FastEmbed dependency and preserves one
            # authoritative implementation for document and query encoding.
            cloud_inference=True,
        )

    async def ping(self, timeout_seconds: float = 1.0) -> bool:
        """Best-effort health check (fail-fast, no retries)."""
        try:
            await asyncio.wait_for(self._client.get_collections(), timeout=float(timeout_seconds))
            return True
        except Exception:
            return False

    def _invalidate_collection_info(self, collection_name: str) -> None:
        """Drop cached collection metadata after a metadata-bearing write."""
        normalized = str(collection_name or "").strip()
        self._collection_info_cache.pop(normalized, None)
        self._collection_dims.pop(normalized, None)
        for key in [
            cache_key
            for cache_key in self._bm25_v2_readiness_cache
            if cache_key and cache_key[0] == normalized
        ]:
            self._bm25_v2_readiness_cache.pop(key, None)

    async def _cached_get_collection(
        self,
        collection_name: str,
        *,
        interactive: bool = False,
    ) -> Any:
        """get_collection with a short TTL cache (SPO-04 / K1).

        Collection scope metadata is immutable after creation; lexical
        serving mode changes go through ``update_collection`` write points,
        each of which invalidates this cache. A stale read can therefore only
        last up to the TTL for out-of-band metadata changes.
        """
        normalized = str(collection_name or "").strip()
        now = time.monotonic()
        cached = self._collection_info_cache.get(normalized)
        if cached is not None and cached[0] > now:
            return cached[1]
        info = await self._call(
            lambda: self._client.get_collection(normalized),
            interactive=interactive,
        )
        vector_store_metrics.get_collection_calls += 1
        self._collection_info_cache[normalized] = (
            now + self._collection_info_ttl_s,
            info,
        )
        return info

    @staticmethod
    def _is_transient_error(exc: BaseException) -> bool:
        if isinstance(exc, (asyncio.TimeoutError, TimeoutError, ConnectionError, OSError)):
            return True
        status_code = getattr(exc, "status_code", None)
        if status_code is None:
            status_code = getattr(getattr(exc, "response", None), "status_code", None)
        try:
            normalized_status = int(status_code)
        except (TypeError, ValueError):
            normalized_status = 0
        if normalized_status == 429 or 500 <= normalized_status < 600:
            return True
        name = type(exc).__name__.lower()
        return any(
            marker in name
            for marker in ("timeout", "transport", "connection", "network")
        )

    @staticmethod
    def _is_collection_missing_error(exc: BaseException) -> bool:
        """True when the underlying Qdrant failure is a 404 (collection gone).

        A 404 during dataset cleanup means a concurrent worker already deleted
        the collection — an idempotent success for removal paths, not a fault.
        """
        seen = 0
        while exc is not None and seen < 6:
            status_code = getattr(exc, "status_code", None)
            if status_code is None:
                status_code = getattr(getattr(exc, "response", None), "status_code", None)
            try:
                if int(status_code) == 404:
                    return True
            except (TypeError, ValueError):
                pass
            exc = exc.__cause__ if exc.__cause__ is not None else exc.__context__
            seen += 1
        return False

    def _record_interactive_health(self, *, success: bool) -> None:
        now = time.monotonic()
        if success:
            self._health_success_until = now + self.health_receipt_ttl_seconds
            self._health_failure_until = 0.0
        else:
            self._health_failure_until = now + self.health_receipt_ttl_seconds
            self._health_success_until = 0.0

    def begin_interactive_budget(self) -> Token[float | None]:
        """Start one absolute budget shared by every nested Qdrant read."""

        deadline = time.monotonic() + self.interactive_deadline_seconds
        inherited = _INTERACTIVE_DEADLINE.get()
        if inherited is not None:
            deadline = min(deadline, inherited)
        return _INTERACTIVE_DEADLINE.set(deadline)

    @staticmethod
    def end_interactive_budget(token: Token[float | None]) -> None:
        _INTERACTIVE_DEADLINE.reset(token)

    async def _call(self, coro_or_factory, *, interactive: bool = False):
        is_factory = callable(coro_or_factory)
        retries = self.max_retries if is_factory else 1
        deadline: float | None = None
        if interactive:
            now = time.monotonic()
            if self._health_failure_until > now:
                raise VectorStoreError(
                    f"Qdrant interactive circuit is open (url={self.url})"
                )
            retries = min(retries, self.interactive_max_retries)
            local_deadline = now + self.interactive_deadline_seconds
            request_deadline = _INTERACTIVE_DEADLINE.get()
            deadline = (
                min(local_deadline, request_deadline)
                if request_deadline is not None
                else local_deadline
            )
        last_exc: Exception | None = None
        for attempt in range(retries):
            timeout_seconds = float(self.timeout_seconds)
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    if last_exc is not None and self._is_transient_error(last_exc):
                        self._record_interactive_health(success=False)
                    raise VectorStoreError(
                        "Qdrant interactive request exceeded its total "
                        f"{self.interactive_deadline_seconds}s deadline (url={self.url})"
                    ) from last_exc
                timeout_seconds = min(timeout_seconds, remaining)
            try:
                coro = coro_or_factory() if is_factory else coro_or_factory
                result = await asyncio.wait_for(coro, timeout=timeout_seconds)
                if interactive:
                    self._record_interactive_health(success=True)
                return result
            except asyncio.TimeoutError as exc:
                last_exc = exc
                if attempt >= retries - 1:
                    if interactive:
                        self._record_interactive_health(success=False)
                    raise VectorStoreError(
                        f"Qdrant request timed out after {timeout_seconds}s (url={self.url})"
                    ) from exc
            except Exception as exc:
                last_exc = exc
                transient = self._is_transient_error(exc)
                if attempt >= retries - 1 or not transient:
                    if interactive and transient:
                        self._record_interactive_health(success=False)
                    raise VectorStoreError(
                        f"Qdrant request failed (url={self.url}): {exc}"
                    ) from exc

            # Exponential backoff before retry
            delay = self.retry_base_delay * (2**attempt)
            if deadline is not None:
                delay = min(delay, max(deadline - time.monotonic(), 0.0))
                if delay <= 0:
                    continue
            await asyncio.sleep(delay)

        raise VectorStoreError(f"Qdrant request failed (url={self.url}): {last_exc}") from last_exc

    async def close(self) -> None:
        await self._client.close()

    def make_collection_name(
        self, dataset_id: str, dimension: int, collection_name: str | None = None
    ) -> str:
        base = _sanitize_collection_name(dataset_id)
        return _sanitize_collection_name(collection_name or f"kb_{base}_{dimension}")

    def _require_bm25_v2_enabled(self) -> None:
        if not self.bm25_v2_enabled:
            raise VectorStoreError(
                "bm25_v2 is disabled by the service kill switch; "
                "set KNOWLEDGE_QDRANT__BM25_V2_ENABLED=true only for an approved rollout"
            )

    def _capability_receipt_key(self, config: LexicalConfig) -> str:
        payload = {
            "endpoint": self.url.rstrip("/"),
            "client": QDRANT_CLIENT_VERSION,
            "field": BM25_V2_FIELD,
            "model": BM25_V2_MODEL,
            "schema_fingerprint": config.bm25_v2.fingerprint,
        }
        canonical = json.dumps(payload, sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(canonical.encode("utf-8")).hexdigest()

    @staticmethod
    def _scope_from_metadata(metadata: dict[str, Any]) -> dict[str, str] | None:
        raw = metadata.get(COLLECTION_SCOPE_METADATA_KEY)
        if not isinstance(raw, dict):
            return None
        try:
            schema_version = int(raw.get("schema_version") or 0)
        except (TypeError, ValueError):
            return None
        if schema_version != 1:
            return None
        dataset_id = str(raw.get("dataset_id") or "").strip()
        tenant_id = str(raw.get("tenant_id") or "").strip()
        if not dataset_id or not tenant_id:
            return None
        return {"dataset_id": dataset_id, "tenant_id": tenant_id}

    @staticmethod
    def _scope_metadata(dataset_id: str, tenant_id: str) -> dict[str, Any]:
        normalized_dataset = str(dataset_id or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_dataset or not normalized_tenant:
            raise VectorStoreError(
                "collection ownership requires non-empty dataset_id and tenant_id scope"
            )
        return {
            COLLECTION_SCOPE_METADATA_KEY: {
                "schema_version": 1,
                "dataset_id": normalized_dataset,
                "tenant_id": normalized_tenant,
            }
        }

    async def require_collection_readable(
        self,
        collection_name: str,
        *,
        tenant_id: str | None,
        dataset_id: str | None,
        expected_active_v2: bool = False,
    ) -> dict[str, str]:
        """Return the collection's authoritative read scope or fail closed.

        Qdrant metadata is the last-mile authority for both immutable ownership
        and lexical serving mode.  Callers may use explicit scope for legacy
        collections, but a present metadata key must be valid; adoption and
        malformed markers are never treated as legacy.  An active ``bm25_v2``
        lexical read (``expected_active_v2=True`` from the PostgreSQL-derived
        dataset selection) requires the collection itself to be cut over —
        the reverse direction (v2 metadata, v1 selection) stays servable
        because cutover retains the ``lexical_v1`` field. Active callers still
        get per-query receipt recomputation on the query path; this check only
        proves the collection can serve the requested lexical mode.
        """

        normalized_collection = str(collection_name or "").strip()
        if not normalized_collection:
            raise CollectionReadAuthorityError(
                "collection_name is required for retrieval"
            )

        info = await self._cached_get_collection(
            normalized_collection,
            interactive=True,
        )
        metadata = self._collection_metadata(info)

        try:
            scope = self._scope_from_metadata(metadata)
        except (TypeError, ValueError) as exc:
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' has malformed immutable scope metadata"
            ) from exc
        if COLLECTION_SCOPE_METADATA_KEY in metadata and scope is None:
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' has malformed immutable scope metadata"
            )

        try:
            stored_lexical = LexicalConfig.from_collection_metadata(metadata)
        except (LexicalConfigError, TypeError, ValueError) as exc:
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' has invalid lexical metadata: {exc}"
            ) from exc
        if COLLECTION_METADATA_KEY in metadata and stored_lexical is None:
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' has invalid lexical metadata"
            )
        # A collection that says bm25_v2 while PostgreSQL still selects v1 is
        # safe for legacy reads: cutover retains the lexical_v1 field and the
        # v1 leg is served by it. Only an ACTIVE lexical read has to prove the
        # collection itself was cut over — never quietly downgrade or upgrade
        # the serving mode here.
        if expected_active_v2 and (
            stored_lexical is None or not stored_lexical.reads_bm25_v2
        ):
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' is not cut over to "
                "bm25_v2; refusing an active-lexical read"
            )

        supplied_tenant = str(tenant_id or "").strip()
        supplied_dataset = str(dataset_id or "").strip()
        if scope is not None:
            if supplied_tenant and supplied_tenant != scope["tenant_id"]:
                raise CollectionReadAuthorityError(
                    f"collection '{normalized_collection}' tenant scope mismatch"
                )
            if supplied_dataset and supplied_dataset != scope["dataset_id"]:
                raise CollectionReadAuthorityError(
                    f"collection '{normalized_collection}' dataset scope mismatch"
                )
            return scope

        if not supplied_tenant or not supplied_dataset:
            raise CollectionReadAuthorityError(
                f"collection '{normalized_collection}' requires non-empty "
                "tenant_id and dataset_id scope"
            )
        return {"tenant_id": supplied_tenant, "dataset_id": supplied_dataset}

    async def require_hierarchical_collections_readable(
        self,
        base_collection: str,
        *,
        tenant_id: str | None,
        dataset_id: str | None,
    ) -> None:
        """Preflight the base and every present hierarchical collection."""

        normalized_base = str(base_collection or "").strip()
        await self.require_collection_readable(
            normalized_base,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        response = await self._call(lambda: self._client.get_collections())
        existing = {
            str(getattr(item, "name", "") or "").strip()
            for item in (getattr(response, "collections", None) or [])
        }
        for suffix in ("_summary", "_sections"):
            collection_name = f"{normalized_base}{suffix}"
            if collection_name not in existing:
                continue
            await self.require_collection_readable(
                collection_name,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )

    @staticmethod
    def _payload_scope_filter(
        *,
        dataset_id: str,
        tenant_id: str,
        allow_missing_tenant: bool = False,
        document_id: str | None = None,
    ) -> Any:
        """Build an exact point-level ownership filter.

        Historical v1/image points may predate the ``tenant_id`` payload. They
        can only be considered during an already-authorized dataset/document
        deletion, where the globally unique dataset id remains mandatory.
        Collection adoption never uses this compatibility allowance.
        """

        conditions: list[Any] = [
            qmodels.FieldCondition(
                key="dataset_id",
                match=qmodels.MatchValue(value=dataset_id),
            )
        ]
        if document_id is not None:
            conditions.append(
                qmodels.FieldCondition(
                    key="document_id",
                    match=qmodels.MatchValue(value=document_id),
                )
            )
        tenant_condition: Any = qmodels.FieldCondition(
            key="tenant_id",
            match=qmodels.MatchValue(value=tenant_id),
        )
        if allow_missing_tenant:
            tenant_condition = qmodels.Filter(
                should=[
                    tenant_condition,
                    qmodels.IsEmptyCondition(
                        is_empty=qmodels.PayloadField(key="tenant_id")
                    ),
                ]
            )
        conditions.append(tenant_condition)
        return qmodels.Filter(must=conditions)

    @staticmethod
    def _enabled_payload_condition() -> Any:
        """Match legacy points without ``enabled`` plus explicitly enabled points."""

        return qmodels.Filter(
            should=[
                qmodels.FieldCondition(
                    key="enabled",
                    match=qmodels.MatchValue(value=True),
                ),
                qmodels.IsEmptyCondition(
                    is_empty=qmodels.PayloadField(key="enabled")
                ),
            ]
        )

    @staticmethod
    def _segment_identity_condition(segment_id: str) -> Any:
        """Match modern payload identity plus legacy point-id identity."""

        return qmodels.Filter(
            should=[
                qmodels.FieldCondition(
                    key="segment_id",
                    match=qmodels.MatchValue(value=segment_id),
                ),
                qmodels.HasIdCondition(has_id=[segment_id]),
            ]
        )

    async def _count_collection_points(
        self,
        collection_name: str,
        *,
        count_filter: Any | None = None,
    ) -> int:
        result = await self._call(
            lambda: self._client.count(
                collection_name=collection_name,
                count_filter=count_filter,
                exact=True,
            )
        )
        return int(getattr(result, "count", 0) or 0)

    async def _ensure_collection_scope(
        self,
        collection_name: str,
        info: Any,
        *,
        dataset_id: str,
        tenant_id: str,
    ) -> Any:
        """Verify immutable ownership or safely adopt a populated legacy collection.

        Adoption requires every existing point to carry the exact requested
        tenant and dataset. Empty or mixed collections provide no ownership
        proof and are never claimed. A second exact verification after metadata
        publication catches writes racing the adoption through this service.
        """

        expected_metadata = self._scope_metadata(dataset_id, tenant_id)
        expected_scope = self._scope_from_metadata(expected_metadata)
        assert expected_scope is not None
        metadata = self._collection_metadata(info)
        stored_scope = self._scope_from_metadata(metadata)
        if stored_scope is not None:
            if stored_scope != expected_scope:
                raise VectorStoreError(
                    f"collection '{collection_name}' immutable scope mismatch"
                )
            return info
        if COLLECTION_SCOPE_METADATA_KEY in metadata:
            raise VectorStoreError(
                f"collection '{collection_name}' has malformed immutable scope metadata"
            )

        total = await self._count_collection_points(collection_name)
        matching = await self._count_collection_points(
            collection_name,
            count_filter=self._payload_scope_filter(
                dataset_id=expected_scope["dataset_id"],
                tenant_id=expected_scope["tenant_id"],
            ),
        )
        if total <= 0 or matching != total:
            raise VectorStoreError(
                f"collection '{collection_name}' is unscoped and cannot be safely adopted "
                f"({matching}/{total} points match the requested ownership)"
            )

        adopting_scope = {
            COLLECTION_SCOPE_METADATA_KEY: {
                "schema_version": 0,
                "status": "adopting",
                "dataset_id": expected_scope["dataset_id"],
                "tenant_id": expected_scope["tenant_id"],
            }
        }
        updated = await self._call(
            lambda: self._client.update_collection(
                collection_name=collection_name,
                metadata=adopting_scope,
            )
        )
        self._invalidate_collection_info(collection_name)
        if updated is not True:
            raise VectorStoreError(
                f"collection '{collection_name}' ownership adoption was rejected"
            )
        refreshed = await self._call(
            lambda: self._client.get_collection(collection_name)
        )
        if self._collection_metadata(refreshed).get(COLLECTION_SCOPE_METADATA_KEY) != (
            adopting_scope[COLLECTION_SCOPE_METADATA_KEY]
        ):
            raise VectorStoreError(
                f"collection '{collection_name}' ownership adoption marker did not converge"
            )
        refreshed_total = await self._count_collection_points(collection_name)
        refreshed_matching = await self._count_collection_points(
            collection_name,
            count_filter=self._payload_scope_filter(
                dataset_id=expected_scope["dataset_id"],
                tenant_id=expected_scope["tenant_id"],
            ),
        )
        if refreshed_total != refreshed_matching:
            invalid_scope = {
                COLLECTION_SCOPE_METADATA_KEY: {
                    "schema_version": 0,
                    "adoption_failed": True,
                }
            }
            with contextlib.suppress(Exception):
                await self._call(
                    lambda: self._client.update_collection(
                        collection_name=collection_name,
                        metadata=invalid_scope,
                    )
                )
                self._invalidate_collection_info(collection_name)
            raise VectorStoreError(
                f"collection '{collection_name}' changed during ownership adoption"
            )
        finalized = await self._call(
            lambda: self._client.update_collection(
                collection_name=collection_name,
                metadata=expected_metadata,
            )
        )
        self._invalidate_collection_info(collection_name)
        if finalized is not True:
            raise VectorStoreError(
                f"collection '{collection_name}' ownership adoption finalization was rejected"
            )
        refreshed = await self._call(
            lambda: self._client.get_collection(collection_name)
        )
        if self._scope_from_metadata(self._collection_metadata(refreshed)) != expected_scope:
            raise VectorStoreError(
                f"collection '{collection_name}' ownership adoption did not converge"
            )
        return refreshed

    async def _invalidate_remote_bm25_v2_receipt(
        self,
        collection_name: str,
        metadata: dict[str, Any],
        *,
        reason: str,
    ) -> None:
        """Invalidate a published manifest before mutating collection points.

        Qdrant merges collection metadata updates, so omitting the receipt key
        does not delete it. Replace the receipt with a fail-closed sentinel and
        verify convergence; a later signed backfill may publish a fresh
        ``status=complete`` receipt.
        """

        raw_receipt = metadata.get(BM25_V2_BACKFILL_METADATA_KEY)
        if not isinstance(raw_receipt, dict) or raw_receipt.get("status") != "complete":
            return
        invalidated = {
            "schema_version": 1,
            "status": "invalidated",
            "reason": reason,
        }
        updated = await self._call(
            lambda: self._client.update_collection(
                collection_name=collection_name,
                metadata={BM25_V2_BACKFILL_METADATA_KEY: invalidated},
            )
        )
        self._invalidate_collection_info(collection_name)
        if updated is not True:
            raise VectorStoreError(
                f"collection '{collection_name}' BM25 v2 receipt invalidation was rejected"
            )
        refreshed = await self._call(
            lambda: self._client.get_collection(collection_name)
        )
        if self._collection_metadata(refreshed).get(BM25_V2_BACKFILL_METADATA_KEY) != invalidated:
            raise VectorStoreError(
                f"collection '{collection_name}' BM25 v2 receipt invalidation did not converge"
            )

    async def scan_embedding_migration_scope(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        embedding_model: str,
        embedding_model_version: str,
        embedding_dimension: int,
        batch_size: int = 256,
    ) -> dict[str, Any]:
        """Live point/source digest for a T3 shadow or serving generation.

        This is deliberately a full, uncached scroll. Verify/gate/cutover use
        it as cross-store evidence, so a count-only shortcut would miss a
        deleted/replaced point and stale content with the same cardinality.
        Every point must belong to the immutable dataset/tenant scope and
        carry the target model provenance. Vectors are deliberately omitted
        from the scroll: Qdrant enforces collection dimension on write, while
        downloading every dense vector would make an operator health check
        scale with embedding width as well as corpus size.
        """

        normalized_collection = str(collection_name or "").strip()
        normalized_tenant = str(tenant_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        expected_model = str(embedding_model or "").strip()
        expected_version = str(embedding_model_version or "").strip()
        expected_dimension = int(embedding_dimension or 0)
        if (
            not normalized_collection
            or not normalized_dataset
            or not normalized_tenant
            or not expected_model
            or expected_dimension <= 0
        ):
            raise VectorStoreError(
                "embedding migration scope needs collection, tenant, dataset, "
                "model, and a positive dimension"
            )

        offset: Any = None
        point_ids: list[str] = []
        source_entries: list[tuple[str, str]] = []
        seen: set[str] = set()
        while True:
            records, next_offset = await self._call(
                lambda scroll_offset=offset: self._client.scroll(
                    collection_name=normalized_collection,
                    limit=max(int(batch_size), 1),
                    offset=scroll_offset,
                    with_payload=[
                        "tenant_id",
                        "dataset_id",
                        "text",
                        "embedding_model",
                        "embedding_model_version",
                    ],
                    with_vectors=False,
                )
            )
            for record in records or []:
                raw_id = getattr(record, "id", None)
                if raw_id is None:
                    raise VectorStoreError("Qdrant returned a point without an id")
                point_id = str(raw_id)
                if point_id in seen:
                    raise VectorStoreError(
                        f"Qdrant scroll returned duplicate point id {point_id}"
                    )
                seen.add(point_id)
                payload = getattr(record, "payload", None)
                payload = payload if isinstance(payload, dict) else {}
                if (
                    str(payload.get("tenant_id") or "") != normalized_tenant
                    or str(payload.get("dataset_id") or "") != normalized_dataset
                ):
                    raise VectorStoreError(
                        f"point {point_id} escaped the embedding migration scope"
                    )
                if str(payload.get("embedding_model") or "") != expected_model or str(
                    payload.get("embedding_model_version") or ""
                ) != expected_version:
                    raise VectorStoreError(
                        f"point {point_id} has stale embedding provenance"
                    )

                text = payload.get("text")
                if not isinstance(text, str):
                    raise VectorStoreError(
                        f"point {point_id} has non-string source text"
                    )
                point_ids.append(point_id)
                source_entries.append((point_id, text))
            if next_offset is None:
                break
            if not records or next_offset == offset:
                raise VectorStoreError(
                    "Qdrant embedding-scope pagination did not make progress"
                )
            offset = next_offset

        return {
            "point_count": len(point_ids),
            "point_ids_sha256": self._lexical_point_ids_sha256(point_ids),
            "source_text_sha256": self._lexical_source_text_sha256(source_entries),
        }

    # ------------------------------------------------- T6 lifecycle surface

    @staticmethod
    def _lexical_point_ids_sha256(point_ids: Sequence[str]) -> str:
        """Sorted-ID digest; frozen contract shared with the backfill script."""

        encoded = "".join(f"{point_id}\n" for point_id in sorted(map(str, point_ids)))
        return hashlib.sha256(encoded.encode("utf-8")).hexdigest()

    @staticmethod
    def _lexical_source_text_sha256(entries: Sequence[tuple[str, str]]) -> str:
        """Sorted (id, sha256(text)) digest; frozen backfill contract."""

        lines: list[str] = []
        for point_id, text in sorted(entries, key=lambda item: str(item[0])):
            text_digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            lines.append(f"{point_id}\0{text_digest}\n")
        return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()

    async def get_bm25_v2_receipt(self, collection_name: str) -> dict[str, Any] | None:
        """Live (uncached) completion receipt persisted on the collection."""

        info = await self._call(lambda: self._client.get_collection(collection_name))
        raw = self._collection_metadata(info).get(BM25_V2_BACKFILL_METADATA_KEY)
        return dict(raw) if isinstance(raw, dict) else None

    async def get_live_lexical_profile(
        self, collection_name: str
    ) -> tuple[LexicalConfig | None, dict[str, Any] | None]:
        """Live (uncached) collection lexical profile + receipt for the protocol."""

        info = await self._call(lambda: self._client.get_collection(collection_name))
        metadata = self._collection_metadata(info)
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        raw = metadata.get(BM25_V2_BACKFILL_METADATA_KEY)
        return stored, dict(raw) if isinstance(raw, dict) else None

    async def invalidate_bm25_v2_receipt(self, collection_name: str, *, reason: str) -> None:
        """Public two-phase sentinel: swap a complete receipt for invalidated."""

        info = await self._call(lambda: self._client.get_collection(collection_name))
        return await self._invalidate_remote_bm25_v2_receipt(
            collection_name,
            self._collection_metadata(info),
            reason=reason,
        )

    async def _scan_bm25_v2_lexical_points(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        config: LexicalConfig,
        batch_size: int = 256,
    ) -> tuple[list[str], list[tuple[str, str]], int]:
        """Scroll the enabled-L3-text lexical scope; return ids, texts, complete count.

        The predicate is the same COALESCE-default scope PostgreSQL is
        authoritative for; a point that escaped the scope or carries a
        conflicting fingerprint fails loudly (addendum §T6.1: missing or
        corrupt index state must never degrade into an empty result).
        """

        must = [
            qmodels.FieldCondition(key="tenant_id", match=qmodels.MatchValue(value=tenant_id)),
            qmodels.FieldCondition(key="dataset_id", match=qmodels.MatchValue(value=dataset_id)),
        ]
        scroll_filter = self._bm25_v2_lexical_filter(must)
        offset: Any = None
        point_ids: list[str] = []
        source_entries: list[tuple[str, str]] = []
        complete = 0
        seen: set[str] = set()
        while True:
            records, next_offset = await self._call(
                # offset is rebound every loop; bind it as a default so the
                # retry wrapper cannot capture a drifted value (B023).
                lambda scroll_offset=offset: self._client.scroll(
                    collection_name=collection_name,
                    scroll_filter=scroll_filter,
                    limit=batch_size,
                    offset=scroll_offset,
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
                ),
                interactive=True,
            )
            for record in records or []:
                raw_id = getattr(record, "id", None)
                if raw_id is None:
                    raise VectorStoreError("Qdrant returned a point without an id")
                point_id = str(raw_id)
                if point_id in seen:
                    raise VectorStoreError(
                        f"Qdrant scroll returned duplicate point id {raw_id!s}"
                    )
                seen.add(point_id)
                payload = getattr(record, "payload", None)
                payload = payload if isinstance(payload, dict) else {}
                if (
                    payload.get("tenant_id") != tenant_id
                    or payload.get("dataset_id") != dataset_id
                    or not self._bm25_v2_point_is_eligible(payload)
                ):
                    raise VectorStoreError(
                        f"point {raw_id!s} escaped the requested lexical scope during verification"
                    )
                text = payload.get("text")
                if not isinstance(text, str) or not text.strip():
                    raise VectorStoreError(f"point {raw_id!s} has empty or non-string text")
                lexical = payload.get("_lexical")
                lexical = lexical if isinstance(lexical, dict) else {}
                observed_schema = lexical.get("bm25_v2_schema_fingerprint")
                observed_filtering = lexical.get("filtering_profile_fingerprint")
                observed_source = lexical.get("source_text_sha256")
                expected_source = hashlib.sha256(text.encode("utf-8")).hexdigest()
                if (
                    observed_schema is not None
                    and observed_schema != config.bm25_v2.fingerprint
                ) or (
                    observed_filtering is not None
                    and observed_filtering != config.filtering.fingerprint
                ):
                    raise VectorStoreError(
                        f"point {raw_id!s} has a conflicting bm25_v2 fingerprint"
                    )
                vectors = getattr(record, "vector", None)
                has_vector = isinstance(vectors, dict) and BM25_V2_FIELD in vectors
                versions = lexical.get("versions")
                versioned = (
                    isinstance(versions, list)
                    and {LEXICAL_V1, BM25_V2_FIELD}.issubset(versions)
                )
                if bool(
                    has_vector
                    and observed_schema == config.bm25_v2.fingerprint
                    and observed_filtering == config.filtering.fingerprint
                    and observed_source == expected_source
                    and versioned
                ):
                    complete += 1
                point_ids.append(point_id)
                source_entries.append((point_id, text))
            if next_offset is None:
                break
            if not records or next_offset == offset:
                raise VectorStoreError("Qdrant scroll pagination did not make progress")
            offset = next_offset
        return point_ids, source_entries, complete

    async def scan_bm25_v2_lexical_scope(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        config: LexicalConfig,
    ) -> dict[str, Any]:
        """Public protocol view of the live lexical scope digests."""

        point_ids, source_entries, complete = await self._scan_bm25_v2_lexical_points(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            config=config,
        )
        return {
            "point_count": len(point_ids),
            "complete_count": complete,
            "point_ids_sha256": self._lexical_point_ids_sha256(point_ids),
            "source_text_sha256": self._lexical_source_text_sha256(source_entries),
        }

    async def verify_bm25_v2_active_readiness(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        config: LexicalConfig | None = None,
    ) -> dict[str, Any]:
        """Verify active readiness with receipt-keyed TTL and singleflight.

        The cache key includes the certified authority revision, both frozen
        fingerprints, and the receipt digests.  Any runtime write invalidates
        the receipt before mutating points, so an old positive result cannot
        authorize a new generation.  TTL zero preserves full-scan behavior.
        """

        from ...core.observability.metrics import record_bm25_v2_readiness

        if self.bm25_v2_readiness_ttl_seconds <= 0:
            try:
                result = await self._verify_bm25_v2_active_readiness_uncached(
                    collection_name,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    config=config,
                )
            except Exception:
                record_bm25_v2_readiness("failure")
                raise
            record_bm25_v2_readiness("miss")
            return result

        self._require_bm25_v2_enabled()
        info = await self._call(
            lambda: self._client.get_collection(collection_name),
            interactive=True,
        )
        metadata = self._collection_metadata(info)
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        receipt = metadata.get(BM25_V2_BACKFILL_METADATA_KEY)
        if stored is None or not isinstance(receipt, dict):
            try:
                result = await self._verify_bm25_v2_active_readiness_uncached(
                    collection_name,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    config=config,
                    _info=info,
                )
            except Exception:
                record_bm25_v2_readiness("failure")
                raise
            record_bm25_v2_readiness("miss")
            return result
        requested_schema = config.bm25_v2.fingerprint if config is not None else ""
        requested_filtering = (
            config.filtering.fingerprint if config is not None else ""
        )
        receipt_fingerprint = hashlib.sha256(
            json.dumps(
                receipt,
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
                default=str,
            ).encode("utf-8")
        ).hexdigest()
        cache_key = (
            collection_name,
            tenant_id,
            dataset_id,
            stored.bm25_v2.fingerprint,
            stored.filtering.fingerprint,
            stored.active_version,
            str(stored.runtime_revision),
            receipt_fingerprint,
            str(receipt.get("authority_content_revision") or ""),
            str(receipt.get("point_count") or ""),
            str(receipt.get("point_ids_sha256") or ""),
            str(receipt.get("source_text_sha256") or ""),
            str(receipt.get("status") or ""),
            requested_schema,
            requested_filtering,
        )
        now = time.monotonic()
        owner = False
        async with self._bm25_v2_readiness_lock:
            cached = self._bm25_v2_readiness_cache.get(cache_key)
            if cached is not None and cached[0] > now:
                record_bm25_v2_readiness("hit")
                return dict(cached[1])
            task = self._bm25_v2_readiness_inflight.get(cache_key)
            if task is None:
                task = asyncio.create_task(
                    self._verify_bm25_v2_active_readiness_uncached(
                        collection_name,
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                        config=config,
                        _info=info,
                    )
                )
                self._bm25_v2_readiness_inflight[cache_key] = task
                task.add_done_callback(
                    lambda completed, key=cache_key: asyncio.create_task(
                        self._complete_bm25_v2_readiness_singleflight(key, completed)
                    )
                )
                owner = True
            else:
                record_bm25_v2_readiness("hit")
        try:
            result = await asyncio.shield(task)
        except Exception:
            record_bm25_v2_readiness("failure")
            raise
        if owner:
            record_bm25_v2_readiness("miss")
        return dict(result)

    async def _complete_bm25_v2_readiness_singleflight(
        self,
        cache_key: tuple[str, ...],
        task: asyncio.Task[dict[str, Any]],
    ) -> None:
        """Retire/cache a verification even if its first waiter is cancelled."""

        result: dict[str, Any] | None = None
        if not task.cancelled():
            try:
                result = dict(task.result())
            except BaseException:
                result = None
        async with self._bm25_v2_readiness_lock:
            if self._bm25_v2_readiness_inflight.get(cache_key) is task:
                self._bm25_v2_readiness_inflight.pop(cache_key, None)
                if result is not None:
                    self._bm25_v2_readiness_cache[cache_key] = (
                        time.monotonic() + self.bm25_v2_readiness_ttl_seconds,
                        result,
                    )

    async def _verify_bm25_v2_active_readiness_uncached(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        config: LexicalConfig | None = None,
        _info: Any | None = None,
    ) -> dict[str, Any]:
        """Recompute active-serving readiness from Qdrant-side evidence only.

        This uncached primitive re-verifies the filter profile and capability
        and recomputes
        the exact receipt point count, sorted-ID digest, and source-text
        digest over the lexical scope (bm25-v2 doc §Failure behavior). The
        PostgreSQL cross-authority side is verified by the T6 lifecycle
        service at cutover time and stored in the receipt's
        ``authority_content_revision``.
        """

        self._require_bm25_v2_enabled()
        info = _info or await self._call(
            lambda: self._client.get_collection(collection_name)
        )
        metadata = self._collection_metadata(info)
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        if stored is None or not stored.reads_bm25_v2:
            raise VectorStoreError(
                f"collection '{collection_name}' is not cut over to bm25_v2"
            )
        if config is not None and (
            config.bm25_v2.fingerprint != stored.bm25_v2.fingerprint
            or config.filtering.fingerprint != stored.filtering.fingerprint
        ):
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 fingerprints disagree "
                "with the PostgreSQL lexical selection"
            )
        scope = self._scope_from_metadata(metadata)
        if scope is None:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing immutable dataset/tenant scope"
            )
        if scope["tenant_id"] != tenant_id or scope["dataset_id"] != dataset_id:
            raise VectorStoreError(f"collection '{collection_name}' dataset scope mismatch")
        sparse_cfg = getattr(info.config.params, "sparse_vectors", None) or {}
        v2_params = sparse_cfg.get(BM25_V2_FIELD)
        modifier = getattr(v2_params, "modifier", None) if v2_params else None
        if getattr(modifier, "value", modifier) != qmodels.Modifier.IDF.value:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing the versioned bm25_v2 field"
            )
        await self._ensure_bm25_v2_capability(stored)
        await self._ensure_filtering_profile(
            collection_name,
            stored,
            info=info,
            allow_mutation=False,
        )
        receipt = metadata.get(BM25_V2_BACKFILL_METADATA_KEY)
        if not isinstance(receipt, dict) or receipt.get("status") != "complete":
            raise VectorStoreError(
                f"collection '{collection_name}' has no completed bm25_v2 receipt; "
                "refusing active reads with unproven coverage"
            )
        if (
            str(receipt.get("collection_name") or "") != collection_name
            or str(receipt.get("tenant_id") or "") != tenant_id
            or str(receipt.get("dataset_id") or "") != dataset_id
            or str(receipt.get("bm25_v2_schema_fingerprint") or "")
            != stored.bm25_v2.fingerprint
            or str(receipt.get("filtering_profile_fingerprint") or "")
            != stored.filtering.fingerprint
            or str(receipt.get("authority_kind") or "") != BM25_V2_AUTHORITY_KIND
        ):
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 receipt does not match "
                "the collection profile"
            )
        point_ids, source_entries, complete = await self._scan_bm25_v2_lexical_points(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            config=stored,
        )
        if complete != len(point_ids):
            raise VectorStoreError(
                f"collection '{collection_name}' has {len(point_ids) - complete} "
                "points without a complete bm25_v2 marker; backfill a fresh receipt"
            )
        observed_ids = self._lexical_point_ids_sha256(point_ids)
        observed_source = self._lexical_source_text_sha256(source_entries)
        if int(receipt.get("point_count") or -1) != len(point_ids):
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 point count drifted from its receipt"
            )
        if str(receipt.get("point_ids_sha256") or "") != observed_ids:
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 sorted point-ID digest "
                "drifted from its receipt"
            )
        if str(receipt.get("source_text_sha256") or "") != observed_source:
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 source-text digest drifted "
                "from its receipt"
            )
        return {
            "schema_version": 1,
            "status": "complete",
            "collection_name": collection_name,
            "bm25_v2_schema_fingerprint": stored.bm25_v2.fingerprint,
            "filtering_profile_fingerprint": stored.filtering.fingerprint,
            "dataset_id": dataset_id,
            "tenant_id": tenant_id,
            "point_count": len(point_ids),
            "point_ids_sha256": observed_ids,
            "manifest_algorithm": "sha256(sorted-point-id-newline-v1)",
            "source_text_sha256": observed_source,
            "source_text_algorithm": "sha256(sorted-point-id-text-sha256-null-newline-v1)",
            "authority_kind": BM25_V2_AUTHORITY_KIND,
            "authority_content_revision": receipt.get("authority_content_revision"),
            "certified_by": "active_readiness_recompute",
        }

    async def publish_bm25_v2_cutover_receipt(
        self,
        collection_name: str,
        *,
        receipt: dict[str, Any],
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any]:
        """Persist a cutover-certified completion receipt (T6 two-phase commit).

        Only the lifecycle protocol calls this, and only after it has
        recomputed agreement between PostgreSQL authority and the Qdrant
        lexical scope under the writer barrier. A malformed receipt is refused
        before any mutation; convergence is re-read from the server.
        """

        if not isinstance(receipt, dict) or receipt.get("status") != "complete":
            raise VectorStoreError("refusing to publish an incomplete bm25_v2 receipt")
        if (
            str(receipt.get("collection_name") or "") != collection_name
            or str(receipt.get("tenant_id") or "") != tenant_id
            or str(receipt.get("dataset_id") or "") != dataset_id
        ):
            raise VectorStoreError(
                f"refusing to publish a bm25_v2 receipt outside the "
                f"collection '{collection_name}' scope"
            )
        updated = await self._call(
            lambda: self._client.update_collection(
                collection_name=collection_name,
                metadata={BM25_V2_BACKFILL_METADATA_KEY: receipt},
            )
        )
        self._invalidate_collection_info(collection_name)
        if updated is not True:
            raise VectorStoreError(
                f"collection '{collection_name}' rejected the bm25_v2 cutover receipt"
            )
        refreshed = await self._call(lambda: self._client.get_collection(collection_name))
        observed = self._collection_metadata(refreshed).get(BM25_V2_BACKFILL_METADATA_KEY)
        if observed != receipt:
            raise VectorStoreError(
                f"collection '{collection_name}' bm25_v2 cutover receipt did not converge"
            )
        return dict(receipt)

    def bm25_v2_shadow_write_stats(self) -> dict[str, int]:
        """Expose process-local shadow failures for health/metrics adapters."""

        return {
            "failures": self._bm25_v2_shadow_write_failures,
            "failed_points": self._bm25_v2_shadow_write_failure_points,
        }

    @staticmethod
    def _bm25_v2_document(text: str, config: LexicalConfig) -> Any:
        if not text.strip():
            raise VectorStoreError("bm25_v2 requires non-empty text")
        required = ("Document", "Bm25Config", "TokenizerType")
        if any(getattr(qmodels, name, None) is None for name in required):
            raise VectorStoreError(
                "bm25_v2 requires qdrant-client with Document/Bm25Config support"
            )
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
            return qmodels.Document(
                text=text,
                model=BM25_V2_MODEL,
                options=options,
            )
        except Exception as exc:
            raise VectorStoreError(f"invalid bm25_v2 Qdrant configuration: {exc}") from exc

    @staticmethod
    def _collection_metadata(info: Any) -> dict[str, Any]:
        metadata = getattr(getattr(info, "config", None), "metadata", None)
        return dict(metadata) if isinstance(metadata, dict) else {}

    @staticmethod
    def _bm25_v2_scope_conditions() -> list[Any]:
        """Match exactly PostgreSQL COALESCE defaults for lexical authority."""

        return [
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
        ]

    @classmethod
    def _bm25_v2_lexical_filter(cls, must: Sequence[Any]) -> Any:
        return qmodels.Filter(
            must=[*must, *cls._bm25_v2_scope_conditions()],
        )

    @staticmethod
    def _bm25_v2_point_is_eligible(payload: dict[str, Any]) -> bool:
        raw_content_type = payload.get("content_type")
        content_type = "text" if raw_content_type is None else raw_content_type
        if content_type != "text":
            return False
        raw_level = payload.get("level")
        level = 3 if raw_level is None else raw_level
        if isinstance(level, bool) or not isinstance(level, int):
            return False
        if level != 3:
            return False
        raw_enabled = payload.get("enabled")
        enabled = True if raw_enabled is None else raw_enabled
        return isinstance(enabled, bool) and enabled

    @staticmethod
    def _payload_index_is_ready(
        info: Any,
        field_name: str,
        *,
        require_tenant_partition: bool = False,
    ) -> bool:
        payload_schema = getattr(info, "payload_schema", None) or {}
        schema_info = payload_schema.get(field_name)
        data_type = getattr(schema_info, "data_type", schema_info)
        expected_type = {
            "level": qmodels.PayloadSchemaType.INTEGER,
            "enabled": qmodels.PayloadSchemaType.BOOL,
        }.get(field_name, qmodels.PayloadSchemaType.KEYWORD)
        is_expected = (
            getattr(data_type, "value", data_type)
            == expected_type.value
        )
        if not is_expected or not require_tenant_partition:
            return is_expected
        params = getattr(schema_info, "params", None)
        return getattr(params, "is_tenant", None) is True

    @staticmethod
    def _strict_filtering_is_ready(info: Any) -> bool:
        strict = getattr(getattr(info, "config", None), "strict_mode_config", None)
        return bool(
            strict is not None
            and getattr(strict, "enabled", None) is True
            and getattr(strict, "unindexed_filtering_retrieve", None) is False
            and getattr(strict, "unindexed_filtering_update", None) is False
        )

    @staticmethod
    def _strict_filtering_is_disabled(info: Any) -> bool:
        strict = getattr(getattr(info, "config", None), "strict_mode_config", None)
        return strict is None or getattr(strict, "enabled", None) is False

    @staticmethod
    def _payload_index_exists(info: Any, field_name: str) -> bool:
        payload_schema = getattr(info, "payload_schema", None) or {}
        return field_name in payload_schema

    async def _validate_query_filter_indexes(
        self,
        collection_name: str,
        field_names: Sequence[str],
        *,
        custom_filter: Any | None = None,
    ) -> None:
        """Fail closed on dynamic filters when collection-wide strict mode is on."""

        info = await self._call(
            lambda: self._client.get_collection(collection_name),
            interactive=True,
        )
        if not self._strict_filtering_is_ready(info):
            return
        if custom_filter is not None:
            raise VectorStoreError(
                "custom query_filter is unsupported while strict unindexed filtering is enabled"
            )
        missing = sorted(
            {
                field_name
                for field_name in field_names
                if field_name and not self._payload_index_exists(info, field_name)
            }
        )
        if missing:
            raise VectorStoreError(
                f"collection '{collection_name}' strict filter indexes are missing: "
                + ", ".join(missing)
            )

    async def _ensure_filtering_profile(
        self,
        collection_name: str,
        config: LexicalConfig,
        *,
        info: Any | None = None,
        allow_mutation: bool,
        enforce_strict: bool | None = None,
        manage_strict: bool = True,
    ) -> Any:
        """Verify the v2 filter profile; optionally create missing indexes.

        Strict mode is never enabled by default. All fields used by built-in
        filters and readiness are indexed before strict mode can be enabled.
        The tenant index must be Qdrant's tenant-partition keyword index.
        """
        current = info or await self._call(lambda: self._client.get_collection(collection_name))
        strict_requested = (
            config.reads_bm25_v2
            if enforce_strict is None
            else bool(enforce_strict)
        ) and config.filtering.strict_unindexed_filtering
        required_indexes = (
            STRICT_FILTER_PAYLOAD_INDEXES
            if config.filtering.strict_unindexed_filtering
            else REQUIRED_FILTER_PAYLOAD_INDEXES
        )
        missing = [
            field_name
            for field_name in required_indexes
            if not self._payload_index_is_ready(
                current,
                field_name,
                require_tenant_partition=field_name == "tenant_id",
            )
        ]
        if missing and not allow_mutation:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing bm25_v2 filter indexes: "
                + ", ".join(missing)
            )
        for field_name in missing:
            schema: Any = {
                "level": qmodels.PayloadSchemaType.INTEGER,
                "enabled": qmodels.PayloadSchemaType.BOOL,
            }.get(field_name, qmodels.PayloadSchemaType.KEYWORD)
            if (
                field_name == "tenant_id"
                and getattr(qmodels, "KeywordIndexParams", None) is not None
            ):
                schema = qmodels.KeywordIndexParams(
                    type=qmodels.KeywordIndexType.KEYWORD,
                    is_tenant=True,
                )
            await self._call(
                lambda fn=field_name, fs=schema: self._client.create_payload_index(
                    collection_name=collection_name,
                    field_name=fn,
                    field_schema=fs,
                    wait=True,
                )
            )

        if missing:
            current = await self._call(lambda: self._client.get_collection(collection_name))
            still_missing = [
                field_name
                for field_name in required_indexes
                if not self._payload_index_is_ready(
                    current,
                    field_name,
                    require_tenant_partition=field_name == "tenant_id",
                )
            ]
            if still_missing:
                raise VectorStoreError(
                    f"collection '{collection_name}' rejected bm25_v2 filter indexes: "
                    + ", ".join(still_missing)
                )

        if not manage_strict:
            return current
        if strict_requested and not self._strict_filtering_is_ready(current):
            if not allow_mutation:
                raise VectorStoreError(
                    f"collection '{collection_name}' strict unindexed filtering is not ready"
                )
            updated = await self._call(
                lambda: self._client.update_collection(
                    collection_name=collection_name,
                    strict_mode_config=qmodels.StrictModeConfig(
                        enabled=True,
                        unindexed_filtering_retrieve=False,
                        unindexed_filtering_update=False,
                    ),
                )
            )
            self._invalidate_collection_info(collection_name)
            if updated is not True:
                raise VectorStoreError(
                    f"collection '{collection_name}' strict filtering update was rejected"
                )
            current = await self._call(lambda: self._client.get_collection(collection_name))
            if not self._strict_filtering_is_ready(current):
                raise VectorStoreError(
                    f"collection '{collection_name}' strict filtering did not converge"
                )
        elif not strict_requested and not self._strict_filtering_is_disabled(current):
            if not allow_mutation:
                raise VectorStoreError(
                    f"collection '{collection_name}' strict filtering remains enabled"
                )
            updated = await self._call(
                lambda: self._client.update_collection(
                    collection_name=collection_name,
                    strict_mode_config=qmodels.StrictModeConfig(enabled=False),
                )
            )
            self._invalidate_collection_info(collection_name)
            if updated is not True:
                raise VectorStoreError(
                    f"collection '{collection_name}' strict filtering rollback was rejected"
                )
            current = await self._call(lambda: self._client.get_collection(collection_name))
            if not self._strict_filtering_is_disabled(current):
                raise VectorStoreError(
                    f"collection '{collection_name}' strict filtering rollback did not converge"
                )
        return current

    async def _ensure_bm25_v2_capability(self, config: LexicalConfig) -> None:
        """Prove native BM25 with an isolated create/write/query/delete canary.

        A missing-collection 404 proves only routing, not inference. The receipt
        is therefore granted only after the configured profile ranks a repeated
        term above a length-diluted term on the actual endpoint. Receipts are
        keyed by endpoint, client, and full schema fingerprint and expire.
        """

        self._require_bm25_v2_enabled()
        receipt_key = self._capability_receipt_key(config)
        now = time.monotonic()
        if self._bm25_v2_capability_receipts.get(receipt_key, 0.0) > now:
            return

        async with self._bm25_v2_capability_lock:
            now = time.monotonic()
            if self._bm25_v2_capability_receipts.get(receipt_key, 0.0) > now:
                return

            canary = f"kb_bm25_v2_canary_{uuid.uuid4().hex}"
            repeated_id = str(uuid.uuid4())
            diluted_id = str(uuid.uuid4())
            unrelated_id = str(uuid.uuid4())
            created = False
            primary_error: Exception | None = None
            cleanup_error: Exception | None = None
            try:
                created_result = await asyncio.wait_for(
                    self._client.create_collection(
                        collection_name=canary,
                        vectors_config={},
                        sparse_vectors_config={
                            BM25_V2_FIELD: qmodels.SparseVectorParams(
                                modifier=qmodels.Modifier.IDF,
                            )
                        },
                    ),
                    timeout=min(float(self.timeout_seconds), 15.0),
                )
                if created_result is not True:
                    raise VectorStoreError("temporary BM25 canary collection was rejected")
                created = True
                await asyncio.wait_for(
                    self._client.upsert(
                        collection_name=canary,
                        wait=True,
                        points=[
                            qmodels.PointStruct(
                                id=repeated_id,
                                vector={
                                    BM25_V2_FIELD: self._bm25_v2_document(
                                        "alpha alpha alpha", config
                                    )
                                },
                            ),
                            qmodels.PointStruct(
                                id=diluted_id,
                                vector={
                                    BM25_V2_FIELD: self._bm25_v2_document(
                                        "alpha filler filler filler filler filler", config
                                    )
                                },
                            ),
                            qmodels.PointStruct(
                                id=unrelated_id,
                                vector={
                                    BM25_V2_FIELD: self._bm25_v2_document(
                                        "beta gamma", config
                                    )
                                },
                            ),
                        ],
                    ),
                    timeout=min(float(self.timeout_seconds), 15.0),
                )
                response = await asyncio.wait_for(
                    self._client.query_points(
                        collection_name=canary,
                        query=self._bm25_v2_document("alpha", config),
                        using=BM25_V2_FIELD,
                        limit=2,
                        with_payload=False,
                    ),
                    timeout=min(float(self.timeout_seconds), 15.0),
                )
                hits = list(getattr(response, "points", None) or [])
                if (
                    len(hits) < 2
                    or str(getattr(hits[0], "id", "")) != repeated_id
                    or float(getattr(hits[0], "score", 0.0) or 0.0) <= 0
                    or float(getattr(hits[0], "score", 0.0) or 0.0)
                    <= float(getattr(hits[1], "score", 0.0) or 0.0)
                    or {str(getattr(hit, "id", "")) for hit in hits}
                    != {repeated_id, diluted_id}
                ):
                    raise VectorStoreError(
                        "native BM25 canary returned unexpected TF/length ranking"
                    )
            except Exception as exc:
                primary_error = exc
            finally:
                if created:
                    try:
                        deleted = await asyncio.wait_for(
                            self._client.delete_collection(collection_name=canary),
                            timeout=min(float(self.timeout_seconds), 15.0),
                        )
                        if deleted is not True:
                            raise VectorStoreError(
                                "temporary BM25 canary collection cleanup was rejected"
                            )
                    except Exception as exc:
                        cleanup_error = exc

            if primary_error is not None:
                suffix = (
                    f"; cleanup also failed: {cleanup_error}"
                    if cleanup_error is not None
                    else ""
                )
                raise VectorStoreError(
                    "bm25_v2 native inference capability canary failed: "
                    f"{type(primary_error).__name__}: {primary_error}{suffix}"
                ) from primary_error
            if cleanup_error is not None:
                raise VectorStoreError(
                    "bm25_v2 native inference capability canary cleanup failed: "
                    f"{cleanup_error}"
                ) from cleanup_error

            ttl = self.bm25_v2_capability_ttl_seconds
            self._bm25_v2_capability_receipts[receipt_key] = time.monotonic() + ttl
            logger.info(
                "bm25_v2_capability_verified endpoint=%s profile=%s ttl_seconds=%s",
                self.url,
                config.bm25_v2.fingerprint,
                ttl,
            )

    async def delete_collection(self, collection_name: str) -> None:
        if not collection_name:
            return
        deleted = await self._call(
            lambda: self._client.delete_collection(collection_name=collection_name)
        )
        if deleted is True:
            self._invalidate_collection_info(collection_name)
        if deleted is not True:
            still_exists = bool(
                await self._call(
                    lambda: self._client.collection_exists(collection_name=collection_name)
                )
            )
            if still_exists:
                raise VectorStoreError(f"collection '{collection_name}' could not be deleted")
        self._collection_dims.pop(collection_name, None)
        self._sparse_collections.discard(collection_name)
        self._sparse_readiness.pop(collection_name, None)

    async def collection_exists(self, collection_name: str) -> bool:
        """True when the named collection is present in the vector store.

        Read-only probe (no lease needed): blue-green migration uses it to
        detect hierarchical auxiliary siblings (``_summary``/``_sections``)
        before opening a migration that cannot enumerate them.
        """
        if not collection_name:
            return False
        return bool(
            await self._call(
                lambda: self._client.collection_exists(collection_name=collection_name)
            )
        )

    async def ensure_collection(
        self,
        dataset_id: str,
        dimension: int,
        collection_name: str | None = None,
        distance: str = "cosine",
        allow_existing: bool = True,
        lexical_config: LexicalConfig | None = None,
        tenant_id: str | None = None,
        allow_lexical_transition: bool = False,
        authority_content_revision: int | None = None,
        *,
        bootstrap_unbound_dataset: bool = False,
        lifecycle_lease_held: bool = False,
    ) -> str:
        """Ensure one collection while fenced against dataset deletion.

        Dataset creation is the sole exception: its collection must exist before
        the PostgreSQL row can be inserted, so that internal caller opts into an
        explicit unbound bootstrap path. Every existing-dataset schema/create
        operation takes the same shared lifecycle lease as point upserts.
        """

        kwargs = {
            "dataset_id": dataset_id,
            "dimension": dimension,
            "collection_name": collection_name,
            "distance": distance,
            "allow_existing": allow_existing,
            "lexical_config": lexical_config,
            "tenant_id": tenant_id,
            "allow_lexical_transition": allow_lexical_transition,
            "authority_content_revision": authority_content_revision,
        }
        if bootstrap_unbound_dataset or lifecycle_lease_held:
            return await self._ensure_collection_unfenced(**kwargs)
        if not callable(self._dataset_write_lease):
            raise VectorStoreError(
                "existing-dataset collection writes require a dataset lifecycle lease"
            )
        async with self._dataset_write_lease(dataset_id, []):
            return await self._ensure_collection_unfenced(**kwargs)

    async def _ensure_collection_unfenced(
        self,
        dataset_id: str,
        dimension: int,
        collection_name: str | None = None,
        distance: str = "cosine",
        allow_existing: bool = True,
        lexical_config: LexicalConfig | None = None,
        tenant_id: str | None = None,
        allow_lexical_transition: bool = False,
        authority_content_revision: int | None = None,
    ) -> str:
        """Implement collection setup after the lifecycle decision is made.

        Set ``allow_existing=False`` when creating a dataset so a caller can
        atomically claim a new collection instead of attaching to another
        dataset's existing collection.

        Returns the actual collection name to use.
        """
        desired = self.make_collection_name(
            dataset_id=dataset_id, dimension=dimension, collection_name=collection_name
        )

        exists = bool(
            await self._call(lambda: self._client.collection_exists(collection_name=desired))
        )
        info = await self._call(lambda: self._client.get_collection(desired)) if exists else None

        if info is not None:
            if not allow_existing:
                raise VectorStoreError(f"collection '{desired}' already exists")
            current_size = int(info.config.params.vectors.size)  # type: ignore[attr-defined]
            if current_size == int(dimension):
                info = await self._ensure_collection_scope(
                    desired,
                    info,
                    dataset_id=dataset_id,
                    tenant_id=str(tenant_id or ""),
                )
                await self.ensure_sparse_vectors(desired)
                await self.ensure_lexical_config(
                    desired,
                    lexical_config,
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    allow_runtime_transition=allow_lexical_transition,
                    authority_content_revision=authority_content_revision,
                )
                return desired

        # If collection does not exist or dimension mismatch, create a correct one.
        # If the desired name exists with wrong size, suffix with a version.
        actual = desired
        if info is not None:
            actual = _sanitize_collection_name(f"{desired}_d{dimension}")

        dist = qmodels.Distance.COSINE
        if distance.lower() in {"dot", "dotproduct"}:
            dist = qmodels.Distance.DOT
        elif distance.lower() in {"euclid", "l2"}:
            dist = qmodels.Distance.EUCLID

        requested_lexical = lexical_config or LexicalConfig()
        sparse_vectors_config = {
            LEXICAL_V1_FIELD: qmodels.SparseVectorParams(
                modifier=qmodels.Modifier.IDF,
            ),
        }
        collection_metadata = self._scope_metadata(dataset_id, str(tenant_id or ""))
        if requested_lexical.writes_bm25_v2:
            if requested_lexical.reads_bm25_v2:
                raise VectorStoreError(
                    "bm25_v2 active mode requires a completed shadow backfill receipt"
                )
            await self._ensure_bm25_v2_capability(requested_lexical)
            sparse_vectors_config[BM25_V2_FIELD] = qmodels.SparseVectorParams(
                modifier=qmodels.Modifier.IDF,
            )
            collection_metadata = {
                **collection_metadata,
                **requested_lexical.to_collection_metadata(),
            }

        create_kwargs: dict[str, Any] = {
            "collection_name": actual,
            "vectors_config": qmodels.VectorParams(size=int(dimension), distance=dist),
            "sparse_vectors_config": sparse_vectors_config,
            "hnsw_config": qmodels.HnswConfigDiff(
                m=16,
                ef_construct=200,
                full_scan_threshold=10000,
            ),
            "optimizers_config": qmodels.OptimizersConfigDiff(
                indexing_threshold=20000,
            ),
        }
        create_kwargs["metadata"] = collection_metadata

        created = await self._call(
            lambda: self._client.create_collection(
                **create_kwargs,
            )
        )
        self._invalidate_collection_info(collection_name)
        if created is not True:
            if allow_existing:
                # Preserve the idempotent ensure contract for ingestion and
                # reindex callers when another request creates the same
                # dimension-compatible collection first.
                existing = await self._call(lambda: self._client.get_collection(actual))
                existing_size = int(existing.config.params.vectors.size)  # type: ignore[attr-defined]
                if existing_size == int(dimension):
                    existing = await self._ensure_collection_scope(
                        actual,
                        existing,
                        dataset_id=dataset_id,
                        tenant_id=str(tenant_id or ""),
                    )
                    await self.ensure_sparse_vectors(actual)
                    await self.ensure_lexical_config(
                        actual,
                        lexical_config,
                        dataset_id=dataset_id,
                        tenant_id=tenant_id,
                        allow_runtime_transition=allow_lexical_transition,
                        authority_content_revision=authority_content_revision,
                    )
                    return actual
            raise VectorStoreError(f"collection '{actual}' could not be claimed")
        try:
            self._sparse_collections.add(actual)
            self._sparse_readiness[actual] = True

            # Payload indexes for fast filtering.
            payload_indexes: tuple[tuple[str, qmodels.PayloadSchemaType], ...] = (
                ("tenant_id", qmodels.PayloadSchemaType.KEYWORD),
                ("dataset_id", qmodels.PayloadSchemaType.KEYWORD),
                ("document_id", qmodels.PayloadSchemaType.KEYWORD),
                ("segment_id", qmodels.PayloadSchemaType.KEYWORD),
                ("source_type", qmodels.PayloadSchemaType.KEYWORD),
                ("language", qmodels.PayloadSchemaType.KEYWORD),
                ("madhab", qmodels.PayloadSchemaType.KEYWORD),
                ("authority_rank", qmodels.PayloadSchemaType.INTEGER),
                ("section_title", qmodels.PayloadSchemaType.KEYWORD),
                # P1: Generic LLM metadata indexes
                ("metadata.topic", qmodels.PayloadSchemaType.KEYWORD),
                ("metadata.keywords", qmodels.PayloadSchemaType.KEYWORD),
            )
            for field_name, field_schema in payload_indexes:
                effective_schema: Any = field_schema
                if (
                    requested_lexical.writes_bm25_v2
                    and field_name == "tenant_id"
                    and getattr(qmodels, "KeywordIndexParams", None) is not None
                ):
                    effective_schema = qmodels.KeywordIndexParams(
                        type=qmodels.KeywordIndexType.KEYWORD,
                        is_tenant=True,
                    )
                with contextlib.suppress(Exception):
                    await self._call(
                        lambda fn=field_name, fs=effective_schema: (
                            self._client.create_payload_index(
                                collection_name=actual,
                                field_name=fn,
                                field_schema=fs,
                            )
                        )
                    )

            if requested_lexical.writes_bm25_v2:
                await self._ensure_filtering_profile(
                    actual,
                    requested_lexical,
                    allow_mutation=True,
                    enforce_strict=False,
                )
        except Exception:
            # The collection was created by this call and has not yet been
            # returned/claimed by a dataset. Compensate post-create failures so
            # retries are not blocked by an orphaned partial schema.
            with contextlib.suppress(Exception):
                await self.delete_collection(actual)
            raise

        return actual

    async def ensure_sparse_vectors(self, collection_name: str) -> bool:
        """Add BM25 sparse vector config to an existing collection (migration)."""
        try:
            info = await self._call(lambda: self._client.get_collection(collection_name))
            sparse_cfg = getattr(info.config.params, "sparse_vectors", None) or {}
            if "bm25" in sparse_cfg:
                self._sparse_collections.add(collection_name)
                self._sparse_readiness.pop(collection_name, None)
                return False
            await self._call(
                lambda: self._client.update_collection(
                    collection_name=collection_name,
                    sparse_vectors_config={
                        "bm25": qmodels.SparseVectorParams(
                            modifier=qmodels.Modifier.IDF,
                        ),
                    },
                )
            )
            self._invalidate_collection_info(collection_name)
            self._sparse_collections.add(collection_name)
            self._sparse_readiness.pop(collection_name, None)
            logger.info("Added BM25 sparse vector config to collection %s", collection_name)
            return True
        except Exception as e:
            logger.warning("Failed to add sparse vector config to %s: %s", collection_name, e)
            return False

    async def ensure_lexical_config(
        self,
        collection_name: str,
        requested: LexicalConfig | None,
        *,
        dataset_id: str | None = None,
        tenant_id: str | None = None,
        allow_runtime_transition: bool = False,
        authority_content_revision: int | None = None,
        active_cutover_authorized: bool = False,
    ) -> bool:
        """Verify or transition the versioned lexical contract.

        Runtime selection changes are admin-only. Normal ingestion callers
        verify the persisted selection and cannot replay a stale dataset
        snapshot over a newer shadow enablement or rollback. An active
        bm25_v2 flip is only honored with ``active_cutover_authorized``,
        which the T6 lifecycle service sets exclusively while it holds the
        dataset write barrier and has verified cross-store agreement — it is
        not a configuration surface, so the production default stays
        shadow-only.
        """
        _ = authority_content_revision
        info = await self._call(lambda: self._client.get_collection(collection_name))
        sparse_cfg = getattr(info.config.params, "sparse_vectors", None) or {}
        metadata = self._collection_metadata(info)
        has_v2_field = BM25_V2_FIELD in sparse_cfg
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc

        if stored is not None and not has_v2_field:
            raise VectorStoreError(
                f"collection '{collection_name}' declares bm25_v2 metadata without its field"
            )

        requested_configured = requested is not None and requested.configured
        explicit_remove = (
            requested is not None
            and not requested.configured
            and allow_runtime_transition
            and stored is not None
        )
        config = stored or LexicalConfig()
        add_v2_field = False

        if has_v2_field:
            modifier = getattr(sparse_cfg[BM25_V2_FIELD], "modifier", None)
            modifier_value = getattr(modifier, "value", modifier)
            if modifier_value != qmodels.Modifier.IDF.value:
                raise VectorStoreError(
                    f"collection '{collection_name}' bm25_v2 field requires IDF modifier"
                )

            if stored is None and requested_configured and requested is not None:
                if not allow_runtime_transition:
                    raise VectorStoreError(
                        "bm25_v2 schema adoption requires an administrator transition"
                    )
                total = await self._call(
                    lambda: self._client.count(
                        collection_name=collection_name,
                        exact=True,
                    )
                )
                if int(getattr(total, "count", 0) or 0) > 0:
                    raise VectorStoreError(
                        f"collection '{collection_name}' has an unversioned bm25_v2 field; "
                        "refusing to mix unknown encodings"
                    )
                if requested.reads_bm25_v2:
                    raise VectorStoreError(
                        "bm25_v2 active mode requires a completed shadow phase"
                    )
                stored = requested.with_runtime_selection(
                    active_version=requested.active_version,
                    shadow_write_enabled=requested.bm25_v2_shadow_write_enabled,
                    filtering=requested.filtering,
                    runtime_revision=1,
                )
                config = stored

            if stored is not None:
                if explicit_remove:
                    config = stored.with_runtime_selection(
                        active_version=LEXICAL_V1,
                        shadow_write_enabled=False,
                        filtering=stored.filtering,
                        runtime_revision=stored.runtime_revision + 1,
                    )
                elif not requested_configured or requested is None:
                    config = stored
                elif requested.bm25_v2.fingerprint != stored.bm25_v2.fingerprint:
                    raise VectorStoreError(
                        f"collection '{collection_name}' bm25_v2 encoding is immutable; "
                        "create a new shadow field/collection for changed parameters"
                    )
                else:
                    runtime_changed = any(
                        (
                            requested.active_version != stored.active_version,
                            requested.bm25_v2_shadow_write_enabled
                            != stored.bm25_v2_shadow_write_enabled,
                            requested.filtering.fingerprint
                            != stored.filtering.fingerprint,
                        )
                    )
                    if runtime_changed and allow_runtime_transition:
                        config = stored.with_runtime_selection(
                            active_version=requested.active_version,
                            shadow_write_enabled=(
                                requested.bm25_v2_shadow_write_enabled
                            ),
                            filtering=requested.filtering,
                            runtime_revision=stored.runtime_revision + 1,
                        )
                    else:
                        # A normal writer may carry a stale DB snapshot. It can
                        # verify the immutable schema but never change the
                        # collection's persisted runtime selection.
                        config = stored
        elif requested_configured and requested is not None and requested.writes_bm25_v2:
            if not allow_runtime_transition:
                raise VectorStoreError(
                    "enabling bm25_v2 shadow writes requires an administrator transition"
                )
            if requested.reads_bm25_v2:
                raise VectorStoreError(
                    "bm25_v2 active mode requires a completed shadow phase"
                )
            add_v2_field = True
            config = requested.with_runtime_selection(
                active_version=requested.active_version,
                shadow_write_enabled=requested.bm25_v2_shadow_write_enabled,
                filtering=requested.filtering,
                runtime_revision=1,
            )

        if not has_v2_field and not add_v2_field:
            return False

        if config.reads_bm25_v2:
            if not active_cutover_authorized:
                raise VectorStoreError(
                    "bm25_v2 active cutover is unavailable; this release is shadow-only"
                )
            # Proof of protocol, not configuration: only the T6 lifecycle
            # service passes this, and the service kill switch still has the
            # final say (release decision stays separate from the protocol).
            self._require_bm25_v2_enabled()

        if not self.bm25_v2_enabled:
            emergency_rollback = bool(
                allow_runtime_transition
                and stored is not None
                and not config.reads_bm25_v2
                and not config.writes_bm25_v2
            )
            if not emergency_rollback and (
                config.reads_bm25_v2
                or (
                allow_runtime_transition
                and requested_configured
                and requested is not None
                and (requested.writes_bm25_v2 or requested.reads_bm25_v2)
                )
            ):
                self._require_bm25_v2_enabled()
            if not emergency_rollback:
                # Emergency shadow kill: keep the persisted profile untouched
                # and let central upsert continue dense+lexical_v1 only. An
                # explicit v1 + shadow-off rollback is allowed to continue so
                # it can publish safe metadata and disable strict mode.
                return False

        supplied_dataset = str(dataset_id or "").strip()
        supplied_tenant = str(tenant_id or "").strip()
        scope = self._scope_from_metadata(metadata)
        if scope is not None:
            if supplied_dataset and supplied_dataset != scope["dataset_id"]:
                raise VectorStoreError(
                    f"collection '{collection_name}' dataset scope mismatch"
                )
            if supplied_tenant and supplied_tenant != scope["tenant_id"]:
                raise VectorStoreError(
                    f"collection '{collection_name}' tenant scope mismatch"
                )
        elif allow_runtime_transition and supplied_dataset and supplied_tenant:
            info = await self._ensure_collection_scope(
                collection_name,
                info,
                dataset_id=supplied_dataset,
                tenant_id=supplied_tenant,
            )
            metadata = self._collection_metadata(info)
            scope = self._scope_from_metadata(metadata)
        else:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing immutable dataset/tenant scope"
            )

        if config.writes_bm25_v2 or config.reads_bm25_v2:
            legacy_params = sparse_cfg.get(LEXICAL_V1_FIELD)
            legacy_modifier = getattr(legacy_params, "modifier", None)
            if getattr(legacy_modifier, "value", legacy_modifier) != qmodels.Modifier.IDF.value:
                raise VectorStoreError(
                    f"collection '{collection_name}' must retain lexical_v1 with IDF"
                )
            await self._ensure_bm25_v2_capability(config)
            info = await self._ensure_filtering_profile(
                collection_name,
                config,
                info=info,
                allow_mutation=True,
                manage_strict=False,
            )

        expected_metadata = {**metadata, **config.to_collection_metadata()}
        metadata_changed = any(
            self._collection_metadata(info).get(key) != value
            for key, value in expected_metadata.items()
        )

        # Rollback relaxes physical strict mode before publishing v1 metadata;
        # an in-flight active query then fails closed on its strict-profile check.
        if stored is not None and stored.reads_bm25_v2 and not config.reads_bm25_v2:
            info = await self._ensure_filtering_profile(
                collection_name,
                config,
                info=info,
                allow_mutation=True,
                enforce_strict=False,
            )

        if add_v2_field or metadata_changed:
            update_kwargs: dict[str, Any] = {
                "collection_name": collection_name,
                "metadata": config.to_collection_metadata(),
            }
            if add_v2_field:
                update_kwargs["sparse_vectors_config"] = {
                    BM25_V2_FIELD: qmodels.SparseVectorParams(
                        modifier=qmodels.Modifier.IDF,
                    )
                }
            try:
                updated = await self._call(lambda: self._client.update_collection(**update_kwargs))
            except Exception as exc:
                raise VectorStoreError(
                    "bm25_v2 requires Qdrant client/server support for Document, "
                    "BM25 configuration, collection metadata, and sparse schema updates"
                ) from exc
            if updated is not True:
                raise VectorStoreError(
                    f"collection '{collection_name}' bm25_v2 schema update was rejected"
                )
            self._invalidate_collection_info(collection_name)

        refreshed: Any | None = None
        if config.writes_bm25_v2 or stored is not None:
            refreshed = await self._call(
                lambda: self._client.get_collection(collection_name)
            )
            await self._ensure_filtering_profile(
                collection_name,
                config,
                info=refreshed,
                allow_mutation=True,
                enforce_strict=False,
            )
        return add_v2_field or metadata_changed

    async def hybrid_search_native(
        self,
        collection_name: str,
        query_vector: list[float],
        sparse_indices: list[int],
        sparse_values: list[float],
        top_k: int = 10,
        dense_limit: int = 100,
        sparse_limit: int = 100,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        document_id: str | None = None,
        source_type: str | None = None,
        language: str | None = None,
        metadata_filter: dict[str, Any] | None = None,
        with_payload: bool = True,
        rrf_k: int = 60,
        dense_weight: float | None = None,
        sparse_weight: float | None = None,
        query_text: str | None = None,
        lexical_config: LexicalConfig | None = None,
        authority_content_revision: int | None = None,
    ) -> list[VectorSearchHit]:
        """Native Qdrant hybrid search: Prefetch(dense+BM25) → RRF fusion."""
        result_sets = await self.hybrid_search_multi_native(
            collection_name=collection_name,
            routes=[
                {
                    "query_vector": query_vector,
                    "sparse_indices": sparse_indices,
                    "sparse_values": sparse_values,
                    "dense_limit": dense_limit,
                    "sparse_limit": sparse_limit,
                    "document_id": document_id,
                    "source_type": source_type,
                    "language": language,
                    "metadata_filter": metadata_filter,
                    "query_text": query_text,
                }
            ],
            top_k=top_k,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            with_payload=with_payload,
            rrf_k=rrf_k,
            dense_weight=dense_weight,
            sparse_weight=sparse_weight,
            lexical_config=lexical_config,
            authority_content_revision=authority_content_revision,
        )
        return result_sets[0] if result_sets else []

    async def hybrid_search_multi_native(
        self,
        collection_name: str,
        routes: list[dict[str, Any]],
        top_k: int,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        with_payload: bool = True,
        rrf_k: int = 60,
        dense_weight: float | None = None,
        sparse_weight: float | None = None,
        lexical_config: LexicalConfig | None = None,
        authority_content_revision: int | None = None,
    ) -> list[list[VectorSearchHit]]:
        """Run one weighted dense+sparse RRF query per route in one batch."""
        _ = authority_content_revision
        rrf_weights: list[float] | None = None
        if dense_weight is not None or sparse_weight is not None:
            if dense_weight is None or sparse_weight is None:
                raise VectorStoreError("dense and sparse RRF weights must be set together")
            rrf_weights = [float(dense_weight), float(sparse_weight)]
            if not all(math.isfinite(weight) and weight >= 0 for weight in rrf_weights):
                raise VectorStoreError("RRF weights must be finite and non-negative")
            if not any(rrf_weights):
                raise VectorStoreError("at least one RRF weight must be positive")

        selected_lexical = lexical_config or LexicalConfig()
        active_v2 = selected_lexical.reads_bm25_v2
        if active_v2 and not self.bm25_v2_enabled:
            raise VectorStoreError(
                "bm25_v2 active serving is unavailable; the service kill switch is off"
            )
        authoritative_scope = await self.require_collection_readable(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            expected_active_v2=active_v2,
        )
        tenant_id = authoritative_scope["tenant_id"]
        dataset_id = authoritative_scope["dataset_id"]
        sparse_field = BM25_V2_FIELD if active_v2 else LEXICAL_V1_FIELD
        if active_v2:
            # Per-query evidence, no positive cache: an active hybrid leg may
            # only run against a collection whose receipt still recomputes.
            await self.verify_bm25_v2_active_readiness(
                collection_name,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                config=selected_lexical,
            )

        requests = []
        for route in routes:
            prefetch = []
            conditions = []
            for key, value in (
                (
                    "tenant_id",
                    tenant_id,
                ),
                (
                    "dataset_id",
                    dataset_id,
                ),
                ("document_id", route.get("document_id")),
                ("source_type", route.get("source_type")),
                ("language", route.get("language")),
            ):
                if value:
                    conditions.append(
                        qmodels.FieldCondition(
                            key=key,
                            match=qmodels.MatchValue(value=value),
                        )
                    )
            for key, value in (route.get("metadata_filter") or {}).items():
                if isinstance(value, (str, int, bool)):
                    conditions.append(
                        qmodels.FieldCondition(
                            key=f"metadata.{key}",
                            match=qmodels.MatchValue(value=value),
                        )
                    )
            conditions.append(self._enabled_payload_condition())
            dense_filter = qmodels.Filter(must=conditions)

            query_vector = route.get("query_vector")
            if query_vector:
                prefetch.append(
                    qmodels.Prefetch(
                        query=query_vector,
                        limit=max(int(route.get("dense_limit") or 1), 1),
                        filter=dense_filter,
                    )
                )
            sparse_query: Any | None = None
            if active_v2:
                route_text = str(route.get("query_text") or "").strip()
                if not route_text:
                    raise VectorStoreError(
                        "bm25_v2 hybrid search requires non-empty query_text "
                        "for every route; refusing a silently-degraded leg"
                    )
                sparse_query = self._bm25_v2_document(route_text, selected_lexical)
            else:
                sparse_indices = list(route.get("sparse_indices") or [])
                if sparse_indices:
                    sparse_query = qmodels.SparseVector(
                        indices=sparse_indices,
                        values=list(route.get("sparse_values") or []),
                    )
            if sparse_query is not None:
                prefetch.append(
                    qmodels.Prefetch(
                        query=sparse_query,
                        using=sparse_field,
                        limit=max(int(route.get("sparse_limit") or 1), 1),
                        filter=dense_filter,
                    )
                )

            if len(prefetch) < 2:
                raise VectorStoreError(
                    "native RRF requires dense and sparse prefetches for every query"
                )
            requests.append(
                qmodels.QueryRequest(
                    prefetch=prefetch,
                    query=qmodels.RrfQuery(
                        rrf=qmodels.Rrf(
                            k=max(int(rrf_k), 1),
                            **({"weights": rrf_weights} if rrf_weights is not None else {}),
                        )
                    ),
                    limit=max(int(top_k), 1),
                    with_payload=with_payload,
                )
            )

        if not requests:
            return []
        if not active_v2 and not await self._is_sparse_ready(
            collection_name,
            interactive=True,
        ):
            raise VectorStoreError(
                f"collection '{collection_name}' requires sparse-vector backfill"
            )

        responses = await self._call(
            lambda: self._client.query_batch_points(
                collection_name=collection_name,
                requests=requests,
            ),
            interactive=True,
        )

        return [
            [
                VectorSearchHit(
                    point_id=str(point.id),
                    score=float(point.score),
                    payload=dict(point.payload or {}),
                )
                for point in (getattr(response, "points", None) or [])
            ]
            for response in responses
        ]

    async def _is_sparse_ready(
        self,
        collection_name: str,
        sparse_field: str = LEXICAL_V1_FIELD,
        *,
        config: LexicalConfig | None = None,
        info: Any | None = None,
        interactive: bool = False,
    ) -> bool:
        """Verify legacy sparse-vector readiness."""
        _ = (config, info)
        if sparse_field != LEXICAL_V1_FIELD:
            raise VectorStoreError(
                "bm25_v2 active readiness is unavailable; this release is shadow-only"
            )
        readiness_key = collection_name
        if sparse_field == LEXICAL_V1_FIELD and self._sparse_readiness.get(readiness_key) is True:
            return True

        total_result = await self._call(
            lambda: self._client.count(collection_name=collection_name, exact=True),
            interactive=interactive,
        )
        total_count = int(getattr(total_result, "count", 0) or 0)
        if total_count == 0:
            if sparse_field == LEXICAL_V1_FIELD:
                self._sparse_readiness[readiness_key] = True
            return True

        sparse_result = await self._call(
            lambda: self._client.count(
                collection_name=collection_name,
                count_filter=qmodels.Filter(
                    must=[qmodels.HasVectorCondition(has_vector=sparse_field)]
                ),
                exact=True,
            ),
            interactive=interactive,
        )
        is_ready = int(getattr(sparse_result, "count", 0) or 0) == total_count
        if is_ready and sparse_field == LEXICAL_V1_FIELD:
            self._sparse_readiness[readiness_key] = True
        else:
            self._sparse_readiness.pop(readiness_key, None)
        return is_ready

    async def sparse_search(
        self,
        collection_name: str,
        sparse_indices: list[int],
        sparse_values: list[float],
        top_k: int = 20,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        document_id: str | None = None,
        source_type: str | None = None,
        language: str | None = None,
        with_payload: bool = True,
        metadata_filter: dict[str, Any] | None = None,
        query_text: str | None = None,
        lexical_config: LexicalConfig | None = None,
        authority_content_revision: int | None = None,
    ) -> list[VectorSearchHit]:
        """Sparse-only (BM25) search via Qdrant native sparse vectors."""
        _ = authority_content_revision
        selected_lexical = lexical_config or LexicalConfig()
        active_v2 = selected_lexical.reads_bm25_v2
        if active_v2 and not self.bm25_v2_enabled:
            raise VectorStoreError(
                "bm25_v2 active serving is unavailable; the service kill switch is off"
            )
        authoritative_scope = await self.require_collection_readable(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            expected_active_v2=active_v2,
        )
        tenant_id = authoritative_scope["tenant_id"]
        dataset_id = authoritative_scope["dataset_id"]
        query: Any
        if active_v2:
            text = str(query_text or "").strip()
            if not text:
                raise VectorStoreError(
                    "bm25_v2 sparse search requires non-empty query_text; "
                    "refusing an empty-result fallback"
                )
            query = self._bm25_v2_document(text, selected_lexical)
        elif sparse_indices:
            query = qmodels.SparseVector(
                indices=sparse_indices,
                values=sparse_values,
            )
        else:
            return []

        conditions = []
        effective_tenant_id = tenant_id
        effective_dataset_id = dataset_id
        if effective_tenant_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="tenant_id",
                    match=qmodels.MatchValue(value=effective_tenant_id),
                )
            )
        if effective_dataset_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="dataset_id",
                    match=qmodels.MatchValue(value=effective_dataset_id),
                )
            )
        if document_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="document_id",
                    match=qmodels.MatchValue(value=document_id),
                )
            )
        if source_type:
            conditions.append(
                qmodels.FieldCondition(
                    key="source_type",
                    match=qmodels.MatchValue(value=source_type),
                )
            )
        if language:
            conditions.append(
                qmodels.FieldCondition(
                    key="language",
                    match=qmodels.MatchValue(value=language),
                )
            )
        for key, value in (metadata_filter or {}).items():
            if isinstance(value, (str, int, bool)):
                conditions.append(
                    qmodels.FieldCondition(
                        key=f"metadata.{key}",
                        match=qmodels.MatchValue(value=value),
                    )
                )
        conditions.append(self._enabled_payload_condition())
        flt = qmodels.Filter(must=conditions)

        sparse_field = selected_lexical.active_field
        if active_v2:
            # Per-query evidence, no positive-readiness cache: an active bm25_v2
            # leg only runs when the collection's receipt still recomputes
            # against the live lexical scope.
            await self.verify_bm25_v2_active_readiness(
                collection_name,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                config=selected_lexical,
            )
        elif not await self._is_sparse_ready(
            collection_name,
            sparse_field,
            interactive=True,
        ):
            raise VectorStoreError(
                f"collection '{collection_name}' requires {sparse_field} backfill"
            )

        resp = await self._call(
            lambda: self._client.query_points(
                collection_name=collection_name,
                query=query,
                using=sparse_field,
                limit=int(top_k),
                query_filter=flt,
                with_payload=with_payload,
            ),
            interactive=True,
        )

        hits = list(getattr(resp, "points", None) or [])
        return [
            VectorSearchHit(
                point_id=str(p.id),
                score=float(p.score),
                payload=dict(p.payload or {}),
            )
            for p in hits
        ]

    @staticmethod
    def _require_completed_write(result: Any, *, operation: str) -> None:
        if result is True:
            return
        status = getattr(result, "status", None)
        status_value = getattr(status, "value", status)
        if str(status_value).lower() != "completed":
            raise VectorStoreError(
                f"Qdrant {operation} did not complete (status={status_value!s})"
            )

    @staticmethod
    def _normalize_points_to_scope(
        collection_name: str,
        points: Sequence[qmodels.PointStruct],
        scope: dict[str, str],
    ) -> list[qmodels.PointStruct]:
        normalized: list[qmodels.PointStruct] = []
        for point in points:
            payload = dict(point.payload or {})
            for field_name in ("dataset_id", "tenant_id"):
                supplied = str(payload.get(field_name) or "").strip()
                if supplied and supplied != scope[field_name]:
                    raise VectorStoreError(
                        f"point '{point.id}' {field_name} does not match "
                        f"collection '{collection_name}' immutable scope"
                    )
                payload[field_name] = scope[field_name]
            normalized.append(point.model_copy(update={"payload": payload}))
        return normalized

    async def snapshot_points(
        self,
        collection_name: str,
        point_ids: Sequence[str],
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, qmodels.PointStruct]:
        """Read exact points, including vectors, for compensating rollback.

        A text-generation publish may replace an existing point in place.
        Deleting that ID after a PostgreSQL failure would delete the previous
        serving generation too, so callers snapshot the old value and restore
        it instead.  IDs come from authoritative segment rows, but collection
        scope and every returned payload are still verified before the backup
        is accepted.
        """

        ids = list(
            dict.fromkeys(
                str(point_id or "").strip()
                for point_id in point_ids
                if str(point_id or "").strip()
            )
        )
        if not ids:
            return {}
        scope = await self.require_collection_readable(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        records = await self._call(
            lambda: self._client.retrieve(
                collection_name=collection_name,
                ids=ids,
                with_payload=True,
                with_vectors=True,
            )
        )
        snapshots: dict[str, qmodels.PointStruct] = {}
        requested = set(ids)
        for record in records or []:
            point_id = str(getattr(record, "id", "") or "").strip()
            if not point_id or point_id not in requested or point_id in snapshots:
                raise VectorStoreError(
                    "Qdrant returned an unexpected point during rollback snapshot"
                )
            payload = dict(getattr(record, "payload", None) or {})
            for field_name in ("tenant_id", "dataset_id"):
                supplied = str(payload.get(field_name) or "").strip()
                if supplied and supplied != scope[field_name]:
                    raise VectorStoreError(
                        f"point '{point_id}' escaped {field_name} scope during rollback snapshot"
                    )
                payload[field_name] = scope[field_name]
            vector = getattr(record, "vector", None)
            if vector is None:
                raise VectorStoreError(
                    f"point '{point_id}' has no vector in rollback snapshot"
                )
            snapshots[point_id] = qmodels.PointStruct(
                id=record.id,
                vector=vector,
                payload=payload,
            )
        return snapshots

    async def upsert(
        self,
        collection_name: str,
        points: Sequence[qmodels.PointStruct],
        *,
        expected_ingestion_identity: str | None = None,
        lifecycle_lease_held: bool = False,
    ) -> None:
        if not points:
            return

        # Resolve the immutable physical owner before selecting the PostgreSQL
        # lifecycle lease. Missing payload scope is filled from this authority;
        # conflicting payload scope is rejected before any Qdrant mutation.
        info = await self._call(lambda: self._client.get_collection(collection_name))
        metadata = self._collection_metadata(info)
        scope = self._scope_from_metadata(metadata)
        if scope is None:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing immutable dataset/tenant scope"
            )
        normalized_points = self._normalize_points_to_scope(
            collection_name,
            points,
            scope,
        )
        document_ids = sorted(
            {
                str((point.payload or {}).get("document_id") or "").strip()
                for point in normalized_points
                if str((point.payload or {}).get("document_id") or "").strip()
            }
        )
        if callable(self._dataset_write_lease) and not lifecycle_lease_held:
            lease_kwargs = (
                {"expected_ingestion_identity": expected_ingestion_identity}
                if expected_ingestion_identity is not None
                else {}
            )
            async with self._dataset_write_lease(
                scope["dataset_id"],
                document_ids,
                **lease_kwargs,
            ):
                await self._upsert_points(collection_name, normalized_points)
            return
        await self._upsert_points(collection_name, normalized_points)

    async def _upsert_points(
        self,
        collection_name: str,
        points: Sequence[qmodels.PointStruct],
    ) -> None:

        # Refresh metadata on every writer call. This is deliberate: a warmed
        # replica must observe shadow enablement, cutover, and rollback made by
        # another replica before deciding which sparse fields to write.
        info = await self._call(lambda: self._client.get_collection(collection_name))
        sparse_cfg = getattr(info.config.params, "sparse_vectors", None) or {}
        metadata = self._collection_metadata(info)
        if LEXICAL_V1_FIELD in sparse_cfg:
            self._sparse_collections.add(collection_name)
        vectors_cfg = getattr(info.config.params, "vectors", None)
        if getattr(vectors_cfg, "size", None) is not None:
            self._collection_dims[collection_name] = int(vectors_cfg.size)
        try:
            stored_lexical = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        lexical_config = stored_lexical or LexicalConfig()
        affects_bm25_scope = any(
            self._bm25_v2_point_is_eligible(dict(point.payload or {}))
            for point in points
        )

        receipt_invalidation_error: Exception | None = None
        if stored_lexical is not None and affects_bm25_scope:
            try:
                await self._invalidate_remote_bm25_v2_receipt(
                    collection_name,
                    metadata,
                    reason="runtime_upsert",
                )
            except Exception as exc:
                if lexical_config.reads_bm25_v2:
                    raise
                # Shadow/rollback traffic must keep its legacy base path
                # available. Point-marker clearing plus mandatory manifest
                # comparison still prevents a stale receipt from enabling a
                # cutover, while the operational failure is surfaced below.
                receipt_invalidation_error = exc

        scope = self._scope_from_metadata(metadata)
        if scope is None:
            raise VectorStoreError(
                f"collection '{collection_name}' is missing immutable dataset/tenant scope"
            )
        shadow_only = lexical_config.writes_bm25_v2 and not lexical_config.reads_bm25_v2
        v2_preflight_error: Exception | None = None
        if lexical_config.reads_bm25_v2 and not self.bm25_v2_enabled:
            self._require_bm25_v2_enabled()
        if lexical_config.writes_bm25_v2:
            try:
                self._require_bm25_v2_enabled()
                if scope is None:
                    raise VectorStoreError(
                        f"collection '{collection_name}' is missing dataset/tenant scope"
                    )
                legacy_params = sparse_cfg.get(LEXICAL_V1_FIELD)
                legacy_modifier = getattr(legacy_params, "modifier", None)
                if (
                    getattr(legacy_modifier, "value", legacy_modifier)
                    != qmodels.Modifier.IDF.value
                ):
                    raise VectorStoreError(
                        f"collection '{collection_name}' must retain lexical_v1 with IDF"
                    )
                sparse_params = sparse_cfg.get(BM25_V2_FIELD)
                modifier = getattr(sparse_params, "modifier", None)
                if getattr(modifier, "value", modifier) != qmodels.Modifier.IDF.value:
                    raise VectorStoreError(
                        f"collection '{collection_name}' is missing the versioned bm25_v2 field"
                    )
                await self._ensure_bm25_v2_capability(lexical_config)
                await self._ensure_filtering_profile(
                    collection_name,
                    lexical_config,
                    info=info,
                    allow_mutation=False,
                    enforce_strict=lexical_config.reads_bm25_v2,
                )
            except Exception as exc:
                if lexical_config.reads_bm25_v2:
                    raise
                v2_preflight_error = exc

        base_points: list[qmodels.PointStruct] = []
        active_points: list[qmodels.PointStruct] = []
        shadow_updates: list[tuple[Any, str, dict[str, Any]]] = []
        invalid_shadow_points = 0
        for point in points:
            raw_vector = point.vector
            incoming_payload = dict(point.payload or {})
            incoming_lexical = incoming_payload.get("_lexical")
            preserved_lexical = (
                dict(incoming_lexical) if isinstance(incoming_lexical, dict) else {}
            )
            text_value = str(incoming_payload.get("text") or "")
            is_lexical_point = self._bm25_v2_point_is_eligible(incoming_payload)
            for field_name in ("dataset_id", "tenant_id"):
                supplied = str(incoming_payload.get(field_name) or "").strip()
                if supplied and supplied != scope[field_name]:
                    raise VectorStoreError(
                        f"point '{point.id}' {field_name} does not match collection scope"
                    )
                incoming_payload[field_name] = scope[field_name]

            if isinstance(raw_vector, list):
                base_vector: Any = {"": raw_vector}
            elif isinstance(raw_vector, dict):
                base_vector = dict(raw_vector)
            else:
                if lexical_config.reads_bm25_v2:
                    raise VectorStoreError(
                        "bm25_v2 requires a dense or named-vector point payload"
                    )
                base_points.append(
                    point.model_copy(update={"payload": incoming_payload})
                )
                invalid_shadow_points += int(shadow_only)
                continue

            base_vector.pop(BM25_V2_FIELD, None)
            if LEXICAL_V1_FIELD in sparse_cfg:
                indices, values = text_to_sparse_vector(text_value)
                if not indices:
                    indices, values = [_EMPTY_SPARSE_INDEX], [1.0]
                base_vector.setdefault(
                    LEXICAL_V1_FIELD,
                    qmodels.SparseVector(indices=indices, values=values),
                )

            # A v1/base write clears the marker before v2 succeeds, preventing
            # a stale vector from looking complete after the source text changes.
            if stored_lexical is not None:
                incoming_payload.pop("_lexical", None)
            base_point = point.model_copy(
                update={"vector": base_vector, "payload": incoming_payload}
            )
            base_points.append(base_point)

            marker = {
                **preserved_lexical,
                "versions": list(
                    dict.fromkeys(
                        [
                            *(
                                [str(item) for item in preserved_lexical.get("versions", [])]
                                if isinstance(preserved_lexical.get("versions"), list)
                                else []
                            ),
                            "lexical_v1",
                            "bm25_v2",
                        ]
                    )
                ),
                "bm25_v2_schema_fingerprint": lexical_config.bm25_v2.fingerprint,
                "filtering_profile_fingerprint": lexical_config.filtering.fingerprint,
                "source_text_sha256": hashlib.sha256(
                    text_value.encode("utf-8")
                ).hexdigest(),
            }
            if lexical_config.reads_bm25_v2 and is_lexical_point:
                active_vector = dict(base_vector)
                active_vector[BM25_V2_FIELD] = self._bm25_v2_document(
                    text_value,
                    lexical_config,
                )
                active_payload = {**incoming_payload, "_lexical": marker}
                active_points.append(
                    point.model_copy(
                        update={"vector": active_vector, "payload": active_payload}
                    )
                )
            elif lexical_config.reads_bm25_v2:
                active_points.append(base_point)
            elif shadow_only and v2_preflight_error is None:
                if is_lexical_point and text_value.strip():
                    shadow_updates.append((point.id, text_value, marker))
                elif is_lexical_point:
                    invalid_shadow_points += 1

        prepared_points = active_points if lexical_config.reads_bm25_v2 else base_points

        # Validate vector dimensions match collection
        first_vec = prepared_points[0].vector
        if isinstance(first_vec, list):
            vec_dim = len(first_vec)
        elif isinstance(first_vec, dict):
            # Named vectors -- check the default dense vector
            default_vec = first_vec.get("", first_vec.get("dense"))
            vec_dim = len(default_vec) if isinstance(default_vec, list) else None
        else:
            vec_dim = None

        if vec_dim is not None:
            cached_dim = self._collection_dims.get(collection_name)
            if cached_dim is not None:
                if vec_dim != cached_dim:
                    raise VectorStoreError(
                        f"Dimension mismatch: vectors are {vec_dim}D but collection "
                        f"'{collection_name}' expects {cached_dim}D"
                    )
            else:
                col_dim = int(info.config.params.vectors.size)
                self._collection_dims[collection_name] = col_dim
                if vec_dim != col_dim:
                    raise VectorStoreError(
                        f"Dimension mismatch: vectors are {vec_dim}D but collection "
                        f"'{collection_name}' expects {col_dim}D"
                    )

        base_result = await self._call(
            lambda: self._client.upsert(
                collection_name=collection_name,
                points=prepared_points,
                wait=True,
            )
        )
        self._require_completed_write(
            base_result,
            operation="base vector upsert",
        )

        if shadow_only:
            failed_points = invalid_shadow_points
            failure: Exception | None = (
                v2_preflight_error or receipt_invalidation_error
            )
            if v2_preflight_error is None and shadow_updates:
                try:
                    update_result = await self._call(
                        lambda: self._client.update_vectors(
                            collection_name=collection_name,
                            points=[
                                qmodels.PointVectors(
                                    id=point_id,
                                    vector={
                                        BM25_V2_FIELD: self._bm25_v2_document(
                                            text_value,
                                            lexical_config,
                                        )
                                    },
                                )
                                for point_id, text_value, _marker in shadow_updates
                            ],
                            wait=True,
                        )
                    )
                    self._require_completed_write(
                        update_result,
                        operation="bm25_v2 shadow vector update",
                    )
                    for point_id, _text_value, marker in shadow_updates:
                        marker_result = await self._call(
                            lambda pid=point_id, payload_marker=marker: (
                                self._client.set_payload(
                                    collection_name=collection_name,
                                    payload={"_lexical": payload_marker},
                                    points=[pid],
                                    wait=True,
                                )
                            )
                        )
                        self._require_completed_write(
                            marker_result,
                            operation="bm25_v2 shadow marker update",
                        )
                except Exception as exc:
                    failure = exc
                    failed_points += len(shadow_updates)
            if failure is not None or failed_points:
                self._bm25_v2_shadow_write_failures += 1
                self._bm25_v2_shadow_write_failure_points += max(
                    failed_points,
                    len(points) if failure is not None and not failed_points else 0,
                )
                logger.warning(
                    "bm25_v2_shadow_write_failed collection=%s profile=%s "
                    "failed_points=%s error=%s",
                    collection_name,
                    lexical_config.bm25_v2.fingerprint,
                    max(failed_points, len(points) if failure is not None else 0),
                    failure or "empty source text",
                )
        elif receipt_invalidation_error is not None:
            logger.warning(
                "bm25_v2_receipt_invalidation_failed collection=%s error=%s",
                collection_name,
                receipt_invalidation_error,
            )
        self._sparse_readiness.pop(collection_name, None)

    async def document_has_points(
        self, *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> bool:
        """Find orphan or serving points across all collections before replay.

        A crashed hierarchical or vision write can reach Qdrant before its
        matching PostgreSQL segment. Checking only segment rows is unsafe.
        """
        if not all(str(value or "").strip() for value in (tenant_id, dataset_id, document_id)):
            raise VectorStoreError("tenant_id, dataset_id, and document_id are required")
        document_filter = self._payload_scope_filter(
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            allow_missing_tenant=True,
        )
        response = await self._call(lambda: self._client.get_collections())
        for item in getattr(response, "collections", None) or []:
            collection_name = str(getattr(item, "name", "") or "").strip()
            if collection_name and await self._count_collection_points(
                collection_name, count_filter=document_filter,
            ) > 0:
                return True
        return False

    async def document_point_ids_by_collection(
        self, *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> dict[str, list[str]]:
        """Enumerate the exact old point manifest before a document generation switch."""

        if not all(str(value or "").strip() for value in (tenant_id, dataset_id, document_id)):
            raise VectorStoreError("tenant_id, dataset_id, and document_id are required")
        scope_filter = self._payload_scope_filter(
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            allow_missing_tenant=True,
        )
        response = await self._call(lambda: self._client.get_collections())
        result: dict[str, list[str]] = {}
        for item in getattr(response, "collections", None) or []:
            collection = str(getattr(item, "name", "") or "").strip()
            if not collection:
                continue
            info = await self._call(lambda name=collection: self._client.get_collection(name))
            metadata = self._collection_metadata(info)
            owner = self._scope_from_metadata(metadata)
            if owner is not None and owner != {
                "tenant_id": tenant_id, "dataset_id": dataset_id,
            }:
                continue
            if owner is None and COLLECTION_SCOPE_METADATA_KEY in metadata:
                raise VectorStoreError(f"collection '{collection}' has malformed scope metadata")
            offset: Any = None
            seen: set[str] = set()
            while True:
                points, next_offset = await self._call(
                    lambda name=collection, page=offset: self._client.scroll(
                        collection_name=name,
                        scroll_filter=scope_filter,
                        limit=256,
                        offset=page,
                        with_payload=["tenant_id", "dataset_id", "document_id"],
                        with_vectors=False,
                    )
                )
                for point in points or []:
                    payload = getattr(point, "payload", None)
                    if not isinstance(payload, dict) or (
                        str(payload.get("dataset_id") or "") != dataset_id
                        or str(payload.get("document_id") or "") != document_id
                        or str(payload.get("tenant_id") or tenant_id) != tenant_id
                    ):
                        raise VectorStoreError("document point scope changed during enumeration")
                    point_id = str(getattr(point, "id", "") or "").strip()
                    if not point_id or point_id in seen:
                        raise VectorStoreError("document point enumeration is incomplete")
                    seen.add(point_id)
                if next_offset is None:
                    break
                if next_offset == offset:
                    raise VectorStoreError("document point scroll did not advance")
                offset = next_offset
            if seen:
                result[collection] = sorted(seen)
        return result

    async def document_has_specialized_points(
        self, *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> bool:
        """Detect old image/hierarchy points independent of today's config."""
        if not all(str(value or "").strip() for value in (tenant_id, dataset_id, document_id)):
            raise VectorStoreError("tenant_id, dataset_id, and document_id are required")
        scope = self._payload_scope_filter(
            tenant_id=tenant_id, dataset_id=dataset_id,
            document_id=document_id, allow_missing_tenant=True,
        )
        specialized = qmodels.Filter(should=[
            qmodels.FieldCondition(key="content_type", match=qmodels.MatchValue(value=value))
            for value in ("image", "section", "document_summary")
        ] + [
            qmodels.FieldCondition(key="level", match=qmodels.MatchValue(value=level))
            for level in (1, 2, 3)
        ])
        response = await self._call(lambda: self._client.get_collections())
        for item in getattr(response, "collections", None) or []:
            collection_name = str(getattr(item, "name", "") or "").strip()
            if not collection_name:
                continue
            point_filter = scope if collection_name.endswith(("_sections", "_summary")) else qmodels.Filter(
                must=[scope, specialized],
            )
            if await self._count_collection_points(
                collection_name, count_filter=point_filter,
            ) > 0:
                return True
        return False

    async def delete_document_points(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        """Delete a document from every Qdrant collection owned by its dataset."""

        normalized_tenant = str(tenant_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_document = str(document_id or "").strip()
        if not normalized_tenant or not normalized_dataset or not normalized_document:
            raise VectorStoreError(
                "tenant_id, dataset_id, and document_id are required for document deletion"
            )
        if not lifecycle_lease_held and not callable(self._dataset_write_lease):
            raise VectorStoreError(
                "document deletion requires a dataset lifecycle lease"
            )

        lease_context = (
            contextlib.nullcontext()
            if lifecycle_lease_held
            else self._dataset_write_lease(
                normalized_dataset,
                [normalized_document],
            )
        )
        async with lease_context:
            response = await self._call(lambda: self._client.get_collections())
            owned: list[str] = []
            owned_metadata: dict[str, dict[str, Any]] = {}
            expected_scope = {
                "tenant_id": normalized_tenant,
                "dataset_id": normalized_dataset,
            }
            document_filter = self._payload_scope_filter(
                tenant_id=normalized_tenant,
                dataset_id=normalized_dataset,
                document_id=normalized_document,
                allow_missing_tenant=True,
            )
            for item in getattr(response, "collections", None) or []:
                collection_name = str(getattr(item, "name", "") or "").strip()
                if not collection_name:
                    continue
                info = await self._call(
                    lambda name=collection_name: self._client.get_collection(name)
                )
                metadata = self._collection_metadata(info)
                scope = self._scope_from_metadata(metadata)
                if scope is not None and scope != expected_scope:
                    continue
                if scope is None:
                    if COLLECTION_SCOPE_METADATA_KEY in metadata:
                        raise VectorStoreError(
                            f"collection '{collection_name}' has malformed immutable scope metadata"
                        )
                    # Do not claim a legacy collection wholesale. Only collections
                    # containing an exact authorized document match participate in
                    # the sweep; mixed and foreign points remain untouched.
                    matching = await self._count_collection_points(
                        collection_name,
                        count_filter=document_filter,
                    )
                    if matching <= 0:
                        continue
                owned.append(collection_name)
                owned_metadata[collection_name] = metadata
            for collection_name in owned:
                await self._invalidate_remote_bm25_v2_receipt(
                    collection_name,
                    owned_metadata[collection_name],
                    reason="runtime_document_delete",
                )
                result = await self._call(
                    lambda name=collection_name: self._client.delete(
                        collection_name=name,
                        points_selector=qmodels.FilterSelector(
                            filter=document_filter
                        ),
                        wait=True,
                    )
                )
                self._require_completed_write(
                    result,
                    operation=f"document delete from {collection_name}",
                )
                self._sparse_readiness.pop(collection_name, None)
            return owned

    async def delete_segment_points(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        segment_id: str,
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        """Delete one segment ID from every collection owned by its dataset.

        A segment's hierarchy level is mutable PostgreSQL data and cannot prove
        where older indexing code wrote the point.  Discovering all exact-owned
        collections prevents a stale copy in ``base``, ``_sections``, or
        ``_summary`` from remaining retrievable after the database row is gone.
        """

        normalized_tenant = str(tenant_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_document = str(document_id or "").strip()
        normalized_segment = str(segment_id or "").strip()
        if not all(
            (
                normalized_tenant,
                normalized_dataset,
                normalized_document,
                normalized_segment,
            )
        ):
            raise VectorStoreError(
                "tenant_id, dataset_id, document_id, and segment_id are required "
                "for segment deletion"
            )
        if not lifecycle_lease_held and not callable(self._dataset_write_lease):
            raise VectorStoreError(
                "segment deletion requires a dataset lifecycle lease"
            )

        lease_context = (
            contextlib.nullcontext()
            if lifecycle_lease_held
            else self._dataset_write_lease(
                normalized_dataset,
                [normalized_document],
            )
        )
        async with lease_context:
            response = await self._call(lambda: self._client.get_collections())
            expected_scope = {
                "tenant_id": normalized_tenant,
                "dataset_id": normalized_dataset,
            }
            scope_filter = self._payload_scope_filter(
                tenant_id=normalized_tenant,
                dataset_id=normalized_dataset,
                document_id=normalized_document,
                allow_missing_tenant=True,
            )
            segment_filter = qmodels.Filter(
                must=[
                    *(getattr(scope_filter, "must", None) or []),
                    self._segment_identity_condition(normalized_segment),
                ]
            )
            owned: list[tuple[str, dict[str, Any]]] = []
            for item in getattr(response, "collections", None) or []:
                collection_name = str(getattr(item, "name", "") or "").strip()
                if not collection_name:
                    continue
                info = await self._call(
                    lambda name=collection_name: self._client.get_collection(name)
                )
                metadata = self._collection_metadata(info)
                scope = self._scope_from_metadata(metadata)
                if scope is not None and scope != expected_scope:
                    continue
                if scope is None:
                    if COLLECTION_SCOPE_METADATA_KEY in metadata:
                        raise VectorStoreError(
                            f"collection '{collection_name}' has malformed immutable "
                            "scope metadata"
                        )
                    matching = await self._count_collection_points(
                        collection_name,
                        count_filter=segment_filter,
                    )
                    if matching <= 0:
                        continue
                try:
                    stored_lexical = LexicalConfig.from_collection_metadata(metadata)
                except (LexicalConfigError, TypeError, ValueError) as exc:
                    raise VectorStoreError(str(exc)) from exc
                if COLLECTION_METADATA_KEY in metadata and stored_lexical is None:
                    raise VectorStoreError(
                        f"collection '{collection_name}' has invalid lexical metadata"
                    )
                owned.append((collection_name, metadata))

            touched: list[str] = []
            for collection_name, metadata in owned:
                await self._invalidate_remote_bm25_v2_receipt(
                    collection_name,
                    metadata,
                    reason="runtime_segment_delete",
                )
                result = await self._call(
                    lambda name=collection_name: self._client.delete(
                        collection_name=name,
                        points_selector=qmodels.FilterSelector(filter=segment_filter),
                        wait=True,
                    )
                )
                self._require_completed_write(
                    result,
                    operation=f"segment delete from {collection_name}",
                )
                self._sparse_readiness.pop(collection_name, None)
                touched.append(collection_name)
            return touched

    async def set_segment_payload_enabled(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        segment_id: str,
        enabled: bool,
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        """Set a segment's reversible visibility payload in every owned collection.

        PostgreSQL remains the final serving authority. This Qdrant-side marker
        prevents disabled points from consuming dense or sparse Top-K slots and
        keeps BM25 shadow receipts scoped to the same eligible point set. Writes
        are idempotent so a partial multi-collection failure can be retried with
        the same target value while the caller retains its lifecycle state.
        """

        normalized_tenant = str(tenant_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        normalized_document = str(document_id or "").strip()
        normalized_segment = str(segment_id or "").strip()
        if not all(
            (
                normalized_tenant,
                normalized_dataset,
                normalized_document,
                normalized_segment,
            )
        ):
            raise VectorStoreError(
                "tenant_id, dataset_id, document_id, and segment_id are required "
                "for segment visibility updates"
            )
        if not isinstance(enabled, bool):
            raise VectorStoreError("segment visibility enabled must be a boolean")
        if not lifecycle_lease_held and not callable(self._dataset_write_lease):
            raise VectorStoreError(
                "segment visibility updates require a lifecycle lease"
            )

        lease_context = (
            contextlib.nullcontext()
            if lifecycle_lease_held
            else self._dataset_write_lease(
                normalized_dataset,
                [normalized_document],
            )
        )
        async with lease_context:
            response = await self._call(lambda: self._client.get_collections())
            expected_scope = {
                "tenant_id": normalized_tenant,
                "dataset_id": normalized_dataset,
            }
            scope_filter = self._payload_scope_filter(
                tenant_id=normalized_tenant,
                dataset_id=normalized_dataset,
                document_id=normalized_document,
                allow_missing_tenant=True,
            )
            segment_filter = qmodels.Filter(
                must=[
                    *(getattr(scope_filter, "must", None) or []),
                    self._segment_identity_condition(normalized_segment),
                ]
            )
            owned: list[tuple[str, dict[str, Any]]] = []
            for item in getattr(response, "collections", None) or []:
                collection_name = str(getattr(item, "name", "") or "").strip()
                if not collection_name:
                    continue
                info = await self._call(
                    lambda name=collection_name: self._client.get_collection(name)
                )
                metadata = self._collection_metadata(info)
                scope = self._scope_from_metadata(metadata)
                if scope is not None and scope != expected_scope:
                    continue
                if scope is None:
                    if COLLECTION_SCOPE_METADATA_KEY in metadata:
                        raise VectorStoreError(
                            f"collection '{collection_name}' has malformed immutable "
                            "scope metadata"
                        )
                    matching = await self._count_collection_points(
                        collection_name,
                        count_filter=segment_filter,
                    )
                    if matching <= 0:
                        continue
                try:
                    stored_lexical = LexicalConfig.from_collection_metadata(metadata)
                except (LexicalConfigError, TypeError, ValueError) as exc:
                    raise VectorStoreError(str(exc)) from exc
                if COLLECTION_METADATA_KEY in metadata and stored_lexical is None:
                    raise VectorStoreError(
                        f"collection '{collection_name}' has invalid lexical metadata"
                    )
                owned.append((collection_name, metadata))

            touched: list[str] = []
            for collection_name, metadata in owned:
                await self._invalidate_remote_bm25_v2_receipt(
                    collection_name,
                    metadata,
                    reason="runtime_segment_visibility_update",
                )
                result = await self._call(
                    lambda name=collection_name: self._client.set_payload(
                        collection_name=name,
                        payload={"enabled": enabled},
                        points=qmodels.FilterSelector(filter=segment_filter),
                        wait=True,
                    )
                )
                self._require_completed_write(
                    result,
                    operation=f"segment visibility update in {collection_name}",
                )
                self._sparse_readiness.pop(collection_name, None)
                touched.append(collection_name)
            return touched

    async def delete_dataset_collections(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        authoritative_collection_names: Sequence[str] = (),
        lifecycle_lease_held: bool = False,
    ) -> list[str]:
        """Remove every collection/point set provably owned by one dataset.

        Names persisted by PostgreSQL are authoritative even when an empty
        legacy collection has no Qdrant metadata. Other collections require an
        exact immutable scope or complete point-level ownership evidence.
        Mixed legacy collections are retained while only the authorized
        dataset's points are removed.
        """

        normalized_tenant = str(tenant_id or "").strip()
        normalized_dataset = str(dataset_id or "").strip()
        if not normalized_tenant or not normalized_dataset:
            raise VectorStoreError(
                "tenant_id and dataset_id are required for dataset deletion"
            )
        if not lifecycle_lease_held:
            raise VectorStoreError(
                "dataset collection deletion requires an exclusive lifecycle lease"
            )
        authoritative = {
            str(name or "").strip()
            for name in authoritative_collection_names
            if str(name or "").strip()
        }
        expected_scope = {
            "tenant_id": normalized_tenant,
            "dataset_id": normalized_dataset,
        }
        dataset_filter = self._payload_scope_filter(
            tenant_id=normalized_tenant,
            dataset_id=normalized_dataset,
            allow_missing_tenant=True,
        )

        response = await self._call(lambda: self._client.get_collections())
        actions: list[tuple[str, bool, dict[str, Any]]] = []
        for item in getattr(response, "collections", None) or []:
            collection_name = str(getattr(item, "name", "") or "").strip()
            if not collection_name:
                continue
            try:
                info = await self._call(
                    lambda name=collection_name: self._client.get_collection(name)
                )
            except VectorStoreError as exc:
                # Listed but vanished: a concurrent cleanup already removed it.
                if self._is_collection_missing_error(exc):
                    continue
                raise
            metadata = self._collection_metadata(info)
            scope = self._scope_from_metadata(metadata)
            is_authoritative = collection_name in authoritative
            if scope is not None:
                if scope == expected_scope:
                    actions.append((collection_name, True, metadata))
                elif is_authoritative:
                    raise VectorStoreError(
                        f"authoritative collection '{collection_name}' immutable scope mismatch"
                    )
                continue
            if COLLECTION_SCOPE_METADATA_KEY in metadata:
                raise VectorStoreError(
                    f"collection '{collection_name}' has malformed immutable scope metadata"
                )

            try:
                total = await self._count_collection_points(collection_name)
                matching = await self._count_collection_points(
                    collection_name,
                    count_filter=dataset_filter,
                )
            except VectorStoreError as exc:
                if self._is_collection_missing_error(exc):
                    continue
                raise
            if total == 0:
                if is_authoritative:
                    actions.append((collection_name, True, metadata))
                continue
            if matching == total:
                actions.append((collection_name, True, metadata))
                continue
            if matching > 0:
                actions.append((collection_name, False, metadata))
                continue
            if is_authoritative:
                raise VectorStoreError(
                    f"authoritative collection '{collection_name}' contains no points "
                    "matching its dataset ownership"
                )

        touched: list[str] = []
        for collection_name, delete_whole, metadata in actions:
            if delete_whole:
                try:
                    await self.delete_collection(collection_name)
                except VectorStoreError as exc:
                    # A concurrent cleanup removed it first — removal intent holds.
                    if not self._is_collection_missing_error(exc):
                        raise
            else:
                await self._invalidate_remote_bm25_v2_receipt(
                    collection_name,
                    metadata,
                    reason="runtime_dataset_delete",
                )
                try:
                    result = await self._call(
                        lambda name=collection_name: self._client.delete(
                            collection_name=name,
                            points_selector=qmodels.FilterSelector(
                                filter=dataset_filter
                            ),
                            wait=True,
                        )
                    )
                except VectorStoreError as exc:
                    if self._is_collection_missing_error(exc):
                        touched.append(collection_name)
                        continue
                    raise
                self._require_completed_write(
                    result,
                    operation=f"dataset point delete from {collection_name}",
                )
                self._sparse_readiness.pop(collection_name, None)
            touched.append(collection_name)
        return touched

    async def delete_points(
        self,
        collection_name: str,
        point_ids: Sequence[str],
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        *,
        lifecycle_lease_held: bool = False,
        affects_bm25_scope: bool = True,
    ) -> None:
        """Delete points while fenced against deletion of the owning dataset."""

        ids = [point_id for point_id in point_ids if point_id]
        if not ids:
            return

        lifecycle_dataset_id = str(dataset_id or "").strip()
        if callable(self._dataset_write_lease) and not lifecycle_lease_held:
            # Collection metadata is the immutable ownership authority. Resolve
            # it before choosing the advisory-lock namespace; the implementation
            # re-reads and verifies it after the shared lease is acquired.
            info = await self._call(
                lambda: self._client.get_collection(collection_name)
            )
            scope = self._scope_from_metadata(self._collection_metadata(info))
            if scope is not None:
                lifecycle_dataset_id = scope["dataset_id"]
            if lifecycle_dataset_id:
                async with self._dataset_write_lease(lifecycle_dataset_id, []):
                    await self._delete_points_unfenced(
                        collection_name,
                        ids,
                        tenant_id=tenant_id,
                        dataset_id=dataset_id,
                        affects_bm25_scope=affects_bm25_scope,
                    )
                return

        await self._delete_points_unfenced(
            collection_name,
            ids,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            affects_bm25_scope=affects_bm25_scope,
        )

    async def delete_document_points_by_ids(
        self,
        collection_name: str,
        point_ids: Sequence[str],
        *,
        tenant_id: str,
        dataset_id: str,
        document_id: str,
        lifecycle_lease_held: bool = False,
    ) -> None:
        """Delete a special generation's exact IDs, including legacy tenantless points.

        A prepared manifest can name a collection that was never created before
        a crash.  The caller must hold the dataset publication lease; document
        and dataset filters prevent an ID collision from deleting another owner.
        """

        ids = sorted({str(point_id).strip() for point_id in point_ids if point_id})
        if not ids:
            return
        if not lifecycle_lease_held or not all(
            str(value or "").strip() for value in (tenant_id, dataset_id, document_id)
        ):
            raise VectorStoreError("special point cleanup requires its dataset lease and owner")
        if not await self.collection_exists(collection_name):
            return
        info = await self._call(lambda: self._client.get_collection(collection_name))
        metadata = self._collection_metadata(info)
        owner = self._scope_from_metadata(metadata)
        if owner is not None and owner != {
            "tenant_id": tenant_id, "dataset_id": dataset_id,
        }:
            raise VectorStoreError("special point collection owner differs")
        if owner is None and COLLECTION_SCOPE_METADATA_KEY in metadata:
            raise VectorStoreError("special point collection owner is malformed")
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        if stored is not None:
            await self._invalidate_remote_bm25_v2_receipt(
                collection_name, metadata, reason="special_generation_cleanup",
            )
        selector = qmodels.Filter(must=[
            self._payload_scope_filter(
                tenant_id=tenant_id, dataset_id=dataset_id,
                document_id=document_id, allow_missing_tenant=True,
            ),
            qmodels.HasIdCondition(has_id=ids),
        ])
        result = await self._call(lambda: self._client.delete(
            collection_name=collection_name,
            points_selector=qmodels.FilterSelector(filter=selector),
            wait=True,
        ))
        self._require_completed_write(result, operation="special document point cleanup")
        remaining = await self._count_collection_points(
            collection_name, count_filter=selector,
        )
        if remaining:
            raise VectorStoreError("special document point cleanup is incomplete")
        self._sparse_readiness.pop(collection_name, None)

    async def _delete_points_unfenced(
        self,
        collection_name: str,
        point_ids: Sequence[str],
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        *,
        affects_bm25_scope: bool = True,
    ) -> None:
        """Delete points from collection, optionally verifying tenant ownership.

        Args:
            collection_name: Name of the collection.
            point_ids: Sequence of point IDs to delete.
            tenant_id: If provided, only delete points belonging to this tenant.
            dataset_id: If provided, only delete points belonging to this dataset.
        """
        ids = [pid for pid in point_ids if pid]
        if not ids:
            return

        info = await self._call(lambda: self._client.get_collection(collection_name))
        metadata = self._collection_metadata(info)
        try:
            stored = LexicalConfig.from_collection_metadata(metadata)
        except LexicalConfigError as exc:
            raise VectorStoreError(str(exc)) from exc
        scope = self._scope_from_metadata(metadata)
        if scope is not None:
            if tenant_id and tenant_id != scope["tenant_id"]:
                raise VectorStoreError("delete tenant scope mismatch")
            if dataset_id and dataset_id != scope["dataset_id"]:
                raise VectorStoreError("delete dataset scope mismatch")
            tenant_id = scope["tenant_id"]
            dataset_id = scope["dataset_id"]

        if stored is not None and affects_bm25_scope:
            await self._invalidate_remote_bm25_v2_receipt(
                collection_name,
                metadata,
                reason="runtime_delete",
            )

        if tenant_id or dataset_id:
            # Use filter-based deletion to ensure tenant/dataset isolation.
            conditions: list[Any] = []
            if tenant_id:
                conditions.append(
                    qmodels.FieldCondition(
                        key="tenant_id",
                        match=qmodels.MatchValue(value=tenant_id),
                    )
                )
            if dataset_id:
                conditions.append(
                    qmodels.FieldCondition(
                        key="dataset_id",
                        match=qmodels.MatchValue(value=dataset_id),
                    )
                )
            conditions.append(qmodels.HasIdCondition(has_id=ids))
            result = await self._call(
                lambda: self._client.delete(
                    collection_name=collection_name,
                    points_selector=qmodels.FilterSelector(
                        filter=qmodels.Filter(
                            must=conditions
                        )
                    ),
                    wait=True,
                )
            )
            self._require_completed_write(result, operation="filtered point deletion")
        else:
            result = await self._call(
                lambda: self._client.delete(
                    collection_name=collection_name,
                    points_selector=qmodels.PointIdsList(points=list(ids)),
                    wait=True,
                )
            )
            self._require_completed_write(result, operation="point deletion")
    async def search(
        self,
        collection_name: str,
        query_vector: list[float],
        top_k: int = 5,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        document_id: str | None = None,
        source_type: str | None = None,
        language: str | None = None,
        with_payload: bool = True,
        with_vectors: bool = False,
        query_filter: qmodels.Filter | None = None,
        score_threshold: float | None = None,
        metadata_filter: dict[str, Any] | None = None,
    ) -> list[VectorSearchHit]:
        """Search for similar vectors inside one tenant and dataset scope.

        Args:
            collection_name: Name of the collection.
            query_vector: Query embedding vector.
            top_k: Number of results to return.
            tenant_id: Tenant ID for mandatory multi-tenant isolation.
            dataset_id: Dataset ID for mandatory collection scoping.
            document_id: Filter by document ID.
            source_type: Filter by source type.
            language: Filter by language.
            with_payload: Include payload in results.
            with_vectors: Include vectors in results.
            query_filter: Additional Qdrant filter.
            score_threshold: Minimum score threshold.
            metadata_filter: Exact-match filters under the nested metadata payload.

        Returns:
            List of VectorSearchHit results.
        """
        # Dense search is a generic primitive used by both the normal and
        # hierarchical retrieval paths. Never rely on a collection name alone
        # for authorization: legacy collections require explicit scope, while
        # versioned collections enforce their immutable Qdrant metadata scope.
        authoritative_scope = await self.require_collection_readable(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        tenant_id = authoritative_scope["tenant_id"]
        dataset_id = authoritative_scope["dataset_id"]

        conditions = [self._enabled_payload_condition()]
        # Tenant isolation - should be first for security
        if tenant_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="tenant_id",
                    match=qmodels.MatchValue(value=tenant_id),
                )
            )
        if dataset_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="dataset_id",
                    match=qmodels.MatchValue(value=dataset_id),
                )
            )
        if document_id:
            conditions.append(
                qmodels.FieldCondition(
                    key="document_id",
                    match=qmodels.MatchValue(value=document_id),
                )
            )
        if source_type:
            conditions.append(
                qmodels.FieldCondition(
                    key="source_type",
                    match=qmodels.MatchValue(value=source_type),
                )
            )
        if language:
            conditions.append(
                qmodels.FieldCondition(
                    key="language",
                    match=qmodels.MatchValue(value=language),
                )
            )
        if metadata_filter:
            for key, value in metadata_filter.items():
                if isinstance(value, (str, int, bool)):
                    conditions.append(
                        qmodels.FieldCondition(
                            key=f"metadata.{key}",
                            match=qmodels.MatchValue(value=value),
                        )
                    )
        flt = qmodels.Filter(must=conditions) if conditions else None
        if query_filter is not None:
            if flt is None:
                flt = query_filter
            else:
                flt = qmodels.Filter(
                    must=[*(query_filter.must or []), *conditions],
                    should=query_filter.should,
                    must_not=query_filter.must_not,
                )

        if metadata_filter or query_filter is not None:
            filter_fields = [
                key
                for key, value in (
                    ("tenant_id", tenant_id),
                    ("dataset_id", dataset_id),
                    ("document_id", document_id),
                    ("source_type", source_type),
                    ("language", language),
                )
                if value
            ]
            filter_fields.extend(
                f"metadata.{key}"
                for key, value in (metadata_filter or {}).items()
                if isinstance(value, (str, int, bool))
            )
            await self._validate_query_filter_indexes(
                collection_name,
                filter_fields,
                custom_filter=query_filter,
            )

        # qdrant-client >= 1.11 uses `query_points` as the unified entry point.
        resp = await self._call(
            lambda: self._client.query_points(
                collection_name=collection_name,
                query=query_vector,
                limit=int(top_k),
                with_payload=with_payload,
                with_vectors=with_vectors,
                query_filter=flt,
                score_threshold=score_threshold,
            ),
            interactive=True,
        )
        hits = list(getattr(resp, "points", None) or [])

        results: list[VectorSearchHit] = []
        for p in hits:
            pid = str(p.id)
            payload = dict(p.payload or {})
            vector: list[float] | None = None
            if with_vectors:
                vec = getattr(p, "vector", None)
                if isinstance(vec, list):
                    vector = vec
                elif isinstance(vec, dict):
                    # Named vectors (multi-vector collections). Prefer the default key if present.
                    v = vec.get("") or vec.get("default")
                    if isinstance(v, list):
                        vector = v
            results.append(
                VectorSearchHit(point_id=pid, score=float(p.score), payload=payload, vector=vector)
            )
        return results

    async def retrieve_vectors(
        self,
        collection_name: str,
        point_ids: Sequence[str],
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, list[float]]:
        ids = [pid for pid in (point_ids or []) if pid]
        if not ids:
            return {}

        authoritative_scope = await self.require_collection_readable(
            collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        scoped_filter = qmodels.Filter(
            must=[
                *(
                    getattr(
                        self._payload_scope_filter(
                            tenant_id=authoritative_scope["tenant_id"],
                            dataset_id=authoritative_scope["dataset_id"],
                        ),
                        "must",
                        None,
                    )
                    or []
                ),
                qmodels.HasIdCondition(has_id=list(ids)),
            ]
        )
        scroll_result = await self._call(
            lambda: self._client.scroll(
                collection_name=collection_name,
                scroll_filter=scoped_filter,
                limit=len(ids),
                # MMR only needs the vectors (perf-review 2026-08-16): the
                # full payload used to be rolled through the wire on every
                # diversification pass and discarded. These two fields are
                # kept solely for the tenant/dataset authority check below.
                with_payload=["tenant_id", "dataset_id"],
                with_vectors=True,
            ),
            interactive=True,
        )
        records = scroll_result[0] if isinstance(scroll_result, tuple) else scroll_result
        vectors: dict[str, list[float]] = {}
        for r in records or []:
            rid = str(getattr(r, "id", "") or "")
            payload = dict(getattr(r, "payload", None) or {})
            if (
                rid not in ids
                or str(payload.get("tenant_id") or "").strip()
                != authoritative_scope["tenant_id"]
                or str(payload.get("dataset_id") or "").strip()
                != authoritative_scope["dataset_id"]
            ):
                raise CollectionReadAuthorityError(
                    f"collection '{collection_name}' returned an out-of-scope vector record"
                )
            vec = getattr(r, "vector", None)
            if not rid or vec is None:
                continue
            if isinstance(vec, list):
                vectors[rid] = vec
            elif isinstance(vec, dict):
                v = vec.get("") or vec.get("default")
                if isinstance(v, list):
                    vectors[rid] = v
        return vectors

    async def hybrid_search(
        self,
        collection_name: str,
        query_vector: list[float],
        query_text: str,
        top_k: int = 5,
        tenant_id: str | None = None,
        dataset_id: str | None = None,
        document_id: str | None = None,
        alpha: float = 0.75,
    ) -> list[VectorSearchHit]:
        """Hybrid search = vector candidates + lightweight lexical scoring.

        Args:
            collection_name: Name of the collection.
            query_vector: Query embedding vector.
            query_text: Query text for lexical scoring.
            top_k: Number of results to return.
            tenant_id: Tenant ID for mandatory multi-tenant isolation.
            dataset_id: Dataset ID for mandatory collection scoping.
            document_id: Filter by document ID.
            alpha: Weight of vector similarity (0-1, higher = more vector weight).

        Returns:
            List of VectorSearchHit results with combined scores.
        """
        candidates = await self.search(
            collection_name=collection_name,
            query_vector=query_vector,
            top_k=max(int(top_k) * 4, int(top_k)),
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            document_id=document_id,
            with_payload=True,
        )
        if not candidates:
            return []

        q = (query_text or "").strip().lower()
        q_terms = [t for t in re.split(r"\W+", q) if t]

        def lexical_score(text: str) -> float:
            if not q_terms:
                return 0.0
            t = (text or "").lower()
            if not t:
                return 0.0
            hit = sum(1 for term in q_terms if term and term in t)
            return hit / max(len(q_terms), 1)

        reranked: list[VectorSearchHit] = []
        for h in candidates:
            text = str(h.payload.get("text") or "")
            lex = lexical_score(text)
            vector_score = float(h.score)
            combined = alpha * vector_score + (1 - alpha) * lex
            payload = dict(h.payload or {})
            payload["_vector_score"] = vector_score
            payload["_lexical_score"] = lex
            payload["_combined_score"] = combined
            reranked.append(VectorSearchHit(point_id=h.point_id, score=combined, payload=payload))

        reranked.sort(key=lambda x: x.score, reverse=True)
        return reranked[: int(top_k)]
