"""PRD T3 (H3 model side): the blue-green embedding-migration machine.

Protocol (PRD §6.3, §6.4-4; addendum §1-T3):

    shadow_build → backfilling → verified → gating → ready → cutover
                                                        (completed)
                                                        ↘ rollback (pointer flip)

Every state lives in PostgreSQL (``EmbeddingVersionStore``); Qdrant is only
ever written to the *target* collection while the *serving* collection keeps
answering queries (zero-window rule). The backfill enumerates the
authoritative PostgreSQL enabled-chunk rows — hierarchical parents included,
so summary vectors are re-embedded from their stored text and content is
never regenerated (addendum §1-T3.1) — and each successfully written chunk
gets a content-hash receipt, which makes the run resumable after a crash or
cancellation ("按内容哈希跳过").

The T0 evaluation gate is injected as an async callable so the eval harness
(T0) owns the pass/fail decision; this module only enforces that a cutover
cannot happen before a recorded passing verdict.

Wiring: the operator API surface lives in ``api/routes/embedding_migration.py``
(describe/start/backfill/verify/gate/cutover/rollback/abort); the retrieval
path enforces the query-embedding identity guard
(``retrieval_service.assert_query_embedding_identity``); dataset updates that
would change the embedding identity are refused and pointed at this endpoint
(``dataset_service``); ingestion stamps the binding provenance per chunk
(``ingestion_service``).
"""

from __future__ import annotations

import hashlib
import json
import logging
import re
from collections.abc import Awaitable, Callable, Sequence
from typing import Any

from ...persistence.embedding_version_store import (
    AUTHORITY_EVIDENCE_KEYS,
    BACKFILL_ENTRY_STATES,
    DEFAULT_RETENTION_SECONDS,
    LIVE_MIGRATION_STATES,
    SCOPE_EVIDENCE_KEYS,
    BindingConflictError,
    EmbeddingVersionStore,
    MigrationStateError,
    authority_matches_scope,
    evidence_matches,
)
from .common import maybe_await

logger = logging.getLogger("knowledge_service.services.knowledge.embedding_migration")

PAYLOAD_METADATA_KEYS = (
    "source_type",
    "citation_text",
    "source_reference",
    "section_title",
    "section_full_path",
    "page_number",
    "chunk_index",
    "paragraph_index",
    "source_document",
    "document_title",
    "madhab",
    "language",
)

DEFAULT_PAGE_SIZE = 200

_PUBLIC_ACTION_JOB_FIELDS = (
    "job_id",
    "migration_id",
    "dataset_id",
    "action",
    "request_hash",
    "state",
    "payload",
    "result",
    "error",
    "requested_by",
    "attempt_count",
    "available_at",
    "lease_expires_at",
    "last_heartbeat_at",
    "started_at",
    "finished_at",
    "created_at",
    "updated_at",
)

# evaluate(migration_context) -> verdict dict; verdict["passed"] decides
# gating → ready | gate_failed. The context carries dataset_id, migration,
# and both bindings so the harness can query shadow vs serving.
GateEvaluator = Callable[[dict[str, Any]], "Awaitable[dict[str, Any]]"]


def _public_action_job(job: dict[str, Any]) -> dict[str, Any]:
    """Keep worker ownership capabilities out of operator API payloads."""

    return {key: job.get(key) for key in _PUBLIC_ACTION_JOB_FIELDS if key in job}


class EmbeddingMigrationError(RuntimeError):
    """The requested migration step cannot be executed safely."""


class MixedModelEmbeddingError(EmbeddingMigrationError):
    """A query embedder does not match the dataset's serving embedding identity."""


def _sanitize(name: str) -> str:
    cleaned = re.sub(r"[^a-zA-Z0-9_-]+", "_", str(name or "").strip())
    return re.sub(r"_+", "_", cleaned).strip("_") or "kb"


def make_shadow_collection_name(dataset_id: str, dimension: int, migration_tag: str = "") -> str:
    """Deterministic, dims-encoding name for one shadow generation.

    The base collection-name convention (``kb_<dataset>_<dim>``, PRD §6.3) is
    kept intact so operational tooling still reads dims from the name; the
    ``_v<n>`` tag distinguishes generations of the same (dataset, dim) pair —
    e.g. two consecutive model upgrades that both land on 1024 dims.
    """
    base = f"kb_{_sanitize(dataset_id)}_{int(dimension)}"
    tag = _sanitize(migration_tag)
    if not tag:
        digest = hashlib.sha256(f"{dataset_id}:{dimension}".encode()).hexdigest()[:8]
        tag = digest
    return f"{base}_v{tag}"


def embedding_identity(value: dict[str, Any]) -> dict[str, Any]:
    """Canonical embedding identity of a dataset row or binding dict."""
    return {
        "embedding_provider": str(value.get("embedding_provider") or ""),
        "embedding_model": str(value.get("embedding_model") or ""),
        "embedding_model_version": str(value.get("embedding_model_version") or ""),
        "embedding_dimension": int(value.get("embedding_dimension") or 0),
    }


def identities_match(left: dict[str, Any], right: dict[str, Any]) -> bool:
    a, b = embedding_identity(left), embedding_identity(right)
    return a == b and a["embedding_dimension"] > 0


def assert_query_embedding_identity(
    serving_binding: dict[str, Any],
    *,
    embedding_provider: str,
    embedding_model: str,
    embedding_model_version: str | None = None,
    embedding_dimension: int | None = None,
) -> None:
    """PRD T3 item 1: refuse a query whose embedder differs from the serving
    collection's generation. Vectors from two models are not comparable; a
    silent mixed-model query returns noise, not answers.

    Called with the dataset's serving binding (resolved through
    ``EmbeddingMigrationService.describe``/store, falling back to the dataset
    row for legacy unbound datasets) at the query boundary.
    """
    provider = str(embedding_provider or "").strip()
    model = str(embedding_model or "").strip()
    bound_provider = str(serving_binding.get("embedding_provider") or "").strip()
    bound_model = str(serving_binding.get("embedding_model") or "").strip()
    bound_version = str(
        serving_binding.get("embedding_model_version") or ""
    ).strip()
    if not bound_provider or not bound_model:
        # A binding without a recorded identity predates T3 adoption; reject
        # nothing until the generation is known (loud failure would break
        # legacy datasets). The next migration stamps it.
        return
    if provider and provider.lower() != bound_provider.lower():
        raise MixedModelEmbeddingError(
            f"query embedder provider '{provider}' does not match the serving "
            f"collection generation '{bound_provider}:{bound_model}'"
        )
    if model and model.lower() != bound_model.lower():
        raise MixedModelEmbeddingError(
            f"query embedder model '{model}' does not match the serving "
            f"collection generation '{bound_provider}:{bound_model}'"
        )
    version = (
        str(embedding_model_version).strip()
        if embedding_model_version is not None
        else None
    )
    if version is not None and version != bound_version:
        raise MixedModelEmbeddingError(
            f"query embedder model version '{version}' does not match the serving "
            f"collection generation '{bound_provider}:{bound_model}:{bound_version}'"
        )
    if embedding_dimension is not None:
        bound_dim = int(serving_binding.get("embedding_dimension") or 0)
        if bound_dim > 0 and int(embedding_dimension) != bound_dim:
            raise MixedModelEmbeddingError(
                f"query embedder dimension {embedding_dimension} does not match "
                f"the serving collection dimension {bound_dim}"
            )


class EmbeddingMigrationService:
    """Stateful orchestrator over the T3 tables + a VectorStore surface.

    ``vector_store`` is only required to expose ``ensure_collection``,
    ``upsert``, optionally ``count_points``/``_count_collection_points`` and
    ``delete_collection`` — the real ``VectorStore`` and unit-test fakes both
    satisfy this. ``embedder_factory(identity) -> BaseEmbedding`` supplies the
    new-generation embedder for backfill (server-owned config resolution
    stays with the caller, mirroring dataset_service).
    """

    def __init__(
        self,
        *,
        store: EmbeddingVersionStore,
        vector_store: Any,
        embedder_factory: Callable[[dict[str, Any]], Any] | None = None,
    ) -> None:
        self.store = store
        self.vector_store = vector_store
        self.embedder_factory = embedder_factory

    async def _scan_target_scope(
        self,
        target_binding: dict[str, Any],
    ) -> dict[str, Any]:
        """Return fresh, exact Qdrant evidence for one target generation."""

        scanner = getattr(self.vector_store, "scan_embedding_migration_scope", None)
        if not callable(scanner):
            raise EmbeddingMigrationError(
                "target collection scope verifier is unavailable"
            )
        try:
            evidence = await maybe_await(
                scanner(
                    str(target_binding.get("collection_name") or ""),
                    tenant_id=str(target_binding.get("tenant_id") or ""),
                    dataset_id=str(target_binding.get("dataset_id") or ""),
                    embedding_model=str(
                        target_binding.get("embedding_model") or ""
                    ),
                    embedding_model_version=str(
                        target_binding.get("embedding_model_version") or ""
                    ),
                    embedding_dimension=int(
                        target_binding.get("embedding_dimension") or 0
                    ),
                )
            )
        except EmbeddingMigrationError:
            raise
        except Exception as exc:
            raise EmbeddingMigrationError(
                f"target collection verification failed: {exc}"
            ) from exc
        if not isinstance(evidence, dict) or not all(
            key in evidence for key in SCOPE_EVIDENCE_KEYS
        ):
            raise EmbeddingMigrationError(
                "target collection verifier returned incomplete evidence"
            )
        return {
            "point_count": int(evidence["point_count"]),
            "point_ids_sha256": str(evidence["point_ids_sha256"]),
            "source_text_sha256": str(evidence["source_text_sha256"]),
        }

    async def _collect_migration_evidence(
        self,
        migration: dict[str, Any],
    ) -> tuple[int, dict[str, Any], dict[str, Any]]:
        dataset_id = str(migration.get("dataset_id") or "")
        target = await self.store.get_binding(
            str(migration.get("target_binding_id") or "")
        )
        if target is None:
            raise MigrationStateError("target binding disappeared")
        pending = await self.store.count_pending_segments(
            str(migration.get("migration_id") or ""),
            dataset_id=dataset_id,
        )
        authority = await self.store.authority_snapshot(dataset_id)
        target_scope = await self._scan_target_scope(target)
        return pending, authority, target_scope

    @staticmethod
    def _require_evidence_agreement(
        *,
        pending: int,
        authority: dict[str, Any],
        target_scope: dict[str, Any],
        verified_authority: Any | None = None,
        verified_target_scope: Any | None = None,
    ) -> None:
        if pending:
            raise EmbeddingMigrationError(
                f"{pending} enabled chunks are not yet embedded into the shadow collection"
            )
        if not authority_matches_scope(authority, target_scope):
            raise EmbeddingMigrationError(
                "shadow collection does not match the PostgreSQL authority "
                "(point/source digest mismatch)"
            )
        if verified_authority is not None and not evidence_matches(
            authority,
            verified_authority,
            keys=AUTHORITY_EVIDENCE_KEYS,
        ):
            raise EmbeddingMigrationError(
                "PostgreSQL corpus changed after verification"
            )
        if verified_target_scope is not None and not evidence_matches(
            target_scope,
            verified_target_scope,
            keys=SCOPE_EVIDENCE_KEYS,
        ):
            raise EmbeddingMigrationError(
                "shadow collection changed after verification"
            )

    # ------------------------------------------------------------- planning

    async def resolve_serving_binding(
        self, dataset: dict[str, Any]
    ) -> dict[str, Any] | None:
        """The indirection layer with legacy fallback: binding row if present,
        else lazily registered from the datasets row (single source of truth
        until the first cutover flips both)."""
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        if not dataset_id:
            raise EmbeddingMigrationError("dataset_id is required")
        binding = await self.store.get_serving_binding(dataset_id)
        if binding is not None:
            return binding
        if not str(dataset.get("collection_name") or "").strip():
            return None
        try:
            return await self.store.register_serving_binding_from_dataset_row(dataset)
        except BindingConflictError:
            # Another dataset legally reserves the name; do not adopt blindly.
            raise

    async def _refuse_unmigratable_auxiliary_collections(
        self, serving: dict[str, Any]
    ) -> None:
        """Seam S2 (addendum §1-T3): fail closed on auxiliary collections.

        Backfill re-embeds the authoritative PostgreSQL text segments into
        the shadow collection only. The hierarchical indexer's sibling
        ``_summary``/``_sections`` collections are not part of that
        enumeration — and section rows are stored with plain ``text`` content
        type, so the ledger cannot even tell them apart — so a cutover would
        strand them on the retired generation while retrieval keeps reading
        them. Refuse the migration until those stores join the protocol.
        """
        serving_collection = str(serving.get("collection_name") or "").strip()
        exists_probe = getattr(self.vector_store, "collection_exists", None)
        if not serving_collection or exists_probe is None:
            return
        stranded: list[str] = []
        for suffix in ("_summary", "_sections"):
            sibling = f"{serving_collection}{suffix}"
            try:
                present = bool(await maybe_await(exists_probe(sibling)))
            except Exception as probe_err:
                # An unknown answer is treated as present: refusing a
                # migration is cheap, stranding a live retrieval store at
                # cutover is not.
                logger.warning(
                    "auxiliary collection probe failed for %s; refusing migration: %s",
                    sibling,
                    probe_err,
                )
                present = True
            if present:
                stranded.append(sibling)
        if stranded:
            raise EmbeddingMigrationError(
                "dataset has hierarchical auxiliary collections outside the "
                f"blue-green enumeration: {', '.join(stranded)}; cutover would "
                "strand them on the retired generation"
            )

    async def start_migration(
        self,
        dataset: dict[str, Any],
        *,
        target_provider: str,
        target_model: str,
        target_model_version: str = "",
        target_dimension: int,
        capabilities: Sequence[str] | None = None,
        lexical_config: Any = None,
        migration_tag: str = "",
    ) -> dict[str, Any]:
        """Create the shadow collection + binding and open the migration job.

        The old collection keeps serving throughout; same-model/same-dimension
        changes are NOT a blue-green migration (that is the T1 in-place reembed
        verb) and are rejected here.

        ``capabilities`` defaults to the serving binding's capabilities
        (``None`` = inherit): the new generation replaces the serving one
        wholesale, so an unspecified capability list must not silently drop
        what the dataset currently has. An explicit list (including ``[]``)
        is the operator's deliberate choice and wins.
        """
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        target_dimension = int(target_dimension or 0)
        if target_dimension <= 0:
            raise EmbeddingMigrationError("target embedding_dimension must be positive")
        serving = await self.resolve_serving_binding(dataset)
        if serving is None:
            raise EmbeddingMigrationError(
                "dataset has no resolvable serving collection; index it first"
            )
        target_identity = {
            "embedding_provider": str(target_provider or "").strip(),
            "embedding_model": str(target_model or "").strip(),
            "embedding_model_version": str(target_model_version or "").strip(),
            "embedding_dimension": target_dimension,
        }
        if not target_identity["embedding_provider"] or not target_identity["embedding_model"]:
            raise EmbeddingMigrationError(
                "target embedding provider and model are required"
            )
        if identities_match(serving, target_identity):
            raise EmbeddingMigrationError(
                "identical embedding identity needs no blue-green migration; "
                "use the in-place reembed verb (T1) for vector repair"
            )
        await self._refuse_unmigratable_auxiliary_collections(serving)
        if capabilities is None:
            inherited = serving.get("capabilities")
            capabilities = (
                [str(item) for item in inherited if str(item or "").strip()]
                if isinstance(inherited, (list, tuple))
                else []
            )
        if not migration_tag:
            migration_tag = hashlib.sha256(
                json.dumps(target_identity, sort_keys=True).encode()
            ).hexdigest()[:8]
        collection_name = make_shadow_collection_name(dataset_id, target_dimension, migration_tag)
        tenant_id = str(serving.get("tenant_id") or dataset.get("tenant_id") or "")
        ensure_kwargs: dict[str, Any] = {
            "dataset_id": dataset_id,
            "dimension": target_dimension,
            "collection_name": collection_name,
            "tenant_id": tenant_id,
        }
        if lexical_config is not None and getattr(lexical_config, "configured", False):
            ensure_kwargs["lexical_config"] = lexical_config

        # Retry / double-submit guard: the shadow name is deterministic per
        # target identity, so an earlier attempt may still reserve it. Resume
        # a failed attempt (or adopt a crashed start's orphan binding) instead
        # of racing a new binding for the same name — the old unconditional
        # orphan-cleanup deleted the in-flight generation on the way out.
        reserved_binding: dict[str, Any] | None = None
        lookup = getattr(self.store, "get_binding_by_collection_name", None)
        if lookup is not None:
            reserved_binding = await lookup(collection_name)
        if reserved_binding is not None:
            if str(reserved_binding.get("dataset_id") or "") != dataset_id:
                # Another owner reserves the name: refuse before touching
                # anything (never adopt, never delete, never re-ensure).
                raise BindingConflictError(
                    f"collection '{collection_name}' is reserved by another dataset"
                )
            prior = None
            by_target = getattr(self.store, "get_migration_by_target_binding", None)
            if by_target is not None:
                prior = await by_target(str(reserved_binding.get("binding_id") or ""))
            prior_state = str(prior.get("state") or "") if prior else ""
            if prior is not None and prior_state in ("failed", "gate_failed"):
                # A failed attempt does not hold the one-live-per-dataset slot,
                # but a DIFFERENT migration may have gone live since. Resuming
                # into that situation would 409 (at best) on the first
                # backfilling transition; refuse here with the clearer message.
                live = await self.store.get_live_migration(dataset_id)
                if live is not None and str(live.get("migration_id") or "") != str(
                    prior.get("migration_id") or ""
                ):
                    raise MigrationStateError(
                        f"dataset already has a live embedding migration "
                        f"({live.get('state')}); abort or finish it before "
                        f"resuming migration {prior.get('migration_id')}"
                    )
                logger.info(
                    "Resuming %s migration %s for dataset %s on start",
                    prior_state,
                    prior.get("migration_id"),
                    dataset_id,
                )
                return {
                    "migration": prior,
                    "serving_binding": serving,
                    "target_binding": reserved_binding,
                    "resumed": True,
                }
            if prior is not None:
                if prior_state in LIVE_MIGRATION_STATES:
                    raise MigrationStateError(
                        f"dataset already has a live embedding migration ({prior_state})"
                    )
                if prior_state == "rolled_back":
                    # Post-cutover rollback with keep_shadow=True leaves the
                    # migration rolled_back and the shadow binding reserved.
                    # Reopening it (rolled_back -> backfilling, target still
                    # holds the vectors) is the retry path; without it start
                    # would 409 "abort that migration" while abort refused the
                    # rolled_back state — an unwedgeable deadlock.
                    reopen = getattr(self.store, "reopen_migration_for_retry", None)
                    reopened = (
                        await reopen(str(prior.get("migration_id") or ""))
                        if reopen is not None
                        else None
                    )
                    if reopened is not None:
                        logger.info(
                            "Reopened rolled-back migration %s for dataset %s on start",
                            reopened.get("migration_id"),
                            dataset_id,
                        )
                        return {
                            "migration": reopened,
                            "serving_binding": serving,
                            "target_binding": reserved_binding,
                            "resumed": True,
                            "reopened": True,
                        }
                raise MigrationStateError(
                    f"shadow collection '{collection_name}' is reserved by an "
                    f"earlier migration in state '{prior_state}'; abort that "
                    "migration (purging the shadow) before starting a new one"
                )
            # Binding without a migration: debris of a crashed start
            # (create_binding landed, begin_migration did not). Adopt it; the
            # collection is (re-)ensured idempotently below.
            await self.vector_store.ensure_collection(**ensure_kwargs)
            binding = reserved_binding
        else:
            await self.vector_store.ensure_collection(**ensure_kwargs)
            try:
                binding = await self.store.create_binding(
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    collection_name=collection_name,
                    embedding_provider=target_identity["embedding_provider"],
                    embedding_model=target_identity["embedding_model"],
                    embedding_model_version=target_identity["embedding_model_version"],
                    embedding_dimension=target_dimension,
                    capabilities=capabilities,
                )
            except EmbeddingMigrationError:
                raise
            except Exception:
                # Zero-orphan rule: a collection nobody can bind to must not
                # linger — but a live binding row reserving the name (this
                # dataset's own earlier attempt, or one created a heartbeat
                # ago by a racing start) means the collection is NOT an
                # orphan. Deleting it would destroy a retryable generation.
                referenced = None
                if lookup is not None:
                    try:
                        referenced = await lookup(collection_name)
                    except Exception:  # pragma: no cover - best-effort guard
                        referenced = None
                if referenced is None:
                    try:
                        await self.vector_store.delete_collection(collection_name)
                    except Exception:  # pragma: no cover - best-effort cleanup
                        logger.warning(
                            "shadow collection cleanup failed after binding conflict: %s",
                            collection_name,
                            exc_info=True,
                        )
                raise
        migration = await self.store.begin_migration(
            dataset_id=dataset_id,
            source_binding_id=str(serving["binding_id"]),
            target_binding_id=str(binding["binding_id"]),
        )
        await self.store.merge_migration_progress(
            str(migration["migration_id"]),
            checkpoint={"source_identity": embedding_identity(serving)},
            totals={"target_collection": collection_name},
        )
        logger.info(
            "Blue-green migration opened for dataset %s: %s -> %s",
            dataset_id,
            serving["collection_name"],
            collection_name,
        )
        return {
            "migration": await self.store.get_migration(str(migration["migration_id"])),
            "serving_binding": serving,
            "target_binding": binding,
        }

    # ------------------------------------------------------------- backfill

    def _build_point(self, migration_ctx: dict[str, Any], segment: dict[str, Any], vector: Sequence[float]) -> Any:
        from qdrant_client import models as qmodels

        target = migration_ctx["target_binding"]
        try:
            metadata = (
                segment.get("metadata")
                if isinstance(segment.get("metadata"), dict)
                else json.loads(str(segment.get("metadata") or "{}"))
            )
        except (TypeError, ValueError):
            metadata = {}
        payload_meta = {
            key: metadata.get(key)
            for key in PAYLOAD_METADATA_KEYS
            if metadata.get(key) is not None
        }
        payload = {
            "tenant_id": str(target.get("tenant_id") or ""),
            "dataset_id": str(target.get("dataset_id") or ""),
            "document_id": str(segment["document_id"]),
            "segment_id": str(segment["segment_id"]),
            "position": int(segment.get("position") or 0),
            "text": str(segment.get("text") or ""),
            "token_count": int(segment.get("token_count") or 0),
            "source_type": payload_meta.get("source_type", "unknown"),
            "language": payload_meta.get("language", "en"),
            "metadata": payload_meta,
            "citation_text": payload_meta.get("citation_text"),
            "source_reference": payload_meta.get("source_reference"),
            # T3 item 1: auditable embedding provenance carried on the point.
            "embedding_model": str(target.get("embedding_model") or ""),
            "embedding_model_version": str(target.get("embedding_model_version") or ""),
            "content_hash": str(segment.get("content_hash") or ""),
        }
        return qmodels.PointStruct(
            id=str(segment["vector_id"]),
            vector=[float(component) for component in vector],
            payload=payload,
        )

    async def backfill(
        self,
        migration_id: str,
        embedder: Any | None = None,
        *,
        page_size: int = DEFAULT_PAGE_SIZE,
        max_rounds: int = 1_000,
    ) -> dict[str, Any]:
        """Run one exclusively claimed backfill for this migration."""

        async with self.store.backfill_lease(migration_id):
            return await self._backfill_claimed(
                migration_id,
                embedder,
                page_size=page_size,
                max_rounds=max_rounds,
            )

    async def _backfill_claimed(
        self,
        migration_id: str,
        embedder: Any | None = None,
        *,
        page_size: int = DEFAULT_PAGE_SIZE,
        max_rounds: int = 1_000,
    ) -> dict[str, Any]:
        """Re-embed every authoritative enabled chunk into the shadow collection.

        Resumable: chunks that already carry a content-hash receipt are absent
        from the pending query, so a re-run (after crash or failure) continues
        where it left off and only re-does rows whose text changed. Runs
        several rounds per call because the corpus may receive edits while
        backfilling; stops when one pass finds nothing pending. The old
        collection is untouched and continues serving (zero-window).
        """
        migration = await self.store.get_migration(migration_id)
        if migration is None:
            raise MigrationStateError("migration not found")
        moved = await self.store.transition_migration(
            migration_id, to_state="backfilling", from_states=BACKFILL_ENTRY_STATES
        )
        if moved is None:
            raise MigrationStateError(
                f"migration in state '{migration['state']}' cannot be backfilled"
            )
        migration = moved
        target_binding = await self.store.get_binding(str(migration["target_binding_id"]))
        if target_binding is None:
            raise MigrationStateError("target binding disappeared")
        embedder = embedder or (
            await maybe_await(
                self.embedder_factory(
                    {
                        **embedding_identity(target_binding),
                        # The identity dict is deliberately minimal, but the
                        # server-owned config resolver needs the tenant scope
                        # to pick tenant-scoped embedding credentials; the
                        # binding carries it, so pass it through.
                        "tenant_id": str(target_binding.get("tenant_id") or ""),
                    }
                )
            )
            if self.embedder_factory
            else None
        )
        if embedder is None:
            raise EmbeddingMigrationError("no embedder supplied for the target generation")

        ctx = {
            "migration": migration,
            "target_binding": target_binding,
            "dataset_id": str(migration["dataset_id"]),
        }
        embedded_total = 0
        rounds = 0
        cache_identity = {
            "embedding_provider": str(target_binding.get("embedding_provider") or ""),
            "embedding_model": str(target_binding.get("embedding_model") or ""),
            "embedding_model_version": str(
                target_binding.get("embedding_model_version") or ""
            ),
        }
        cache_lookup = getattr(self.store, "lookup_embeddings_batch", None)
        cache_store = getattr(self.store, "store_embeddings_batch", None)
        try:
            while rounds < max_rounds:
                rounds += 1
                pending = await self.store.list_pending_segments(
                    migration_id,
                    dataset_id=str(migration["dataset_id"]),
                    limit=page_size,
                )
                if not pending:
                    break
                target_dim = int(target_binding["embedding_dimension"])
                content_hashes = [
                    str(row.get("content_hash") or "").strip() for row in pending
                ]
                # T3 item 4: the identity-keyed vector cache lets a re-run —
                # or a sibling migration to the same generation — reuse
                # already-embedded vectors instead of paying the embedder
                # again. Misses are embedded below and written back; the
                # receipt ledger stays the authority on progress either way.
                cached_vectors: dict[str, Sequence[float]] = {}
                if cache_lookup is not None and any(content_hashes):
                    try:
                        cached_vectors = (
                            await maybe_await(
                                cache_lookup(
                                    content_hashes=[h for h in content_hashes if h],
                                    **cache_identity,
                                )
                            )
                            or {}
                        )
                    except Exception as cache_err:
                        logger.warning(
                            "vector cache lookup failed for migration %s; "
                            "embedding the full page: %s",
                            migration_id,
                            cache_err,
                        )
                        cached_vectors = {}
                vectors: list[Sequence[float] | None] = [None] * len(pending)
                missing_indices: list[int] = []
                for index, content_hash in enumerate(content_hashes):
                    cached_vector = (
                        cached_vectors.get(content_hash) if content_hash else None
                    )
                    # A stale/corrupt cache entry (wrong dimension) is dropped
                    # on the floor and re-embedded rather than trusted.
                    if cached_vector is not None and len(cached_vector) == target_dim:
                        vectors[index] = cached_vector
                    else:
                        missing_indices.append(index)
                if missing_indices:
                    texts = [
                        str(pending[index].get("text") or "")
                        for index in missing_indices
                    ]
                    embedded = await embedder.embed_texts(texts, text_type="document")
                    if len(embedded) != len(missing_indices):
                        raise EmbeddingMigrationError(
                            f"embedder returned {len(embedded)} vectors for "
                            f"{len(missing_indices)} chunks"
                        )
                    for position, index in enumerate(missing_indices):
                        vectors[index] = embedded[position]
                points = []
                receipts = []
                for row, vector in zip(pending, vectors, strict=True):
                    if vector is None:
                        # Refuse silent misalignment (addendum §2: NaN/None skip
                        # corrupts the text<->vector pairing); fail the batch.
                        raise EmbeddingMigrationError(
                            f"embedder returned no vector for segment {row['segment_id']}"
                        )
                    dim = int(target_binding["embedding_dimension"])
                    if len(vector) != dim:
                        raise EmbeddingMigrationError(
                            f"vector dimension drift for segment {row['segment_id']}: "
                            f"got {len(vector)}, target collection is {dim}"
                        )
                    points.append(self._build_point(ctx, row, vector))
                    receipts.append(
                        {
                            "segment_id": str(row["segment_id"]),
                            "document_id": str(row["document_id"]),
                            "position": int(row.get("position") or 0),
                            "vector_id": str(row["vector_id"]),
                            "content_hash": str(row.get("content_hash") or ""),
                        }
                    )
                if cache_store is not None and missing_indices:
                    # Write freshly embedded vectors back before the point
                    # write. Non-fatal: the cache is a cost optimization, the
                    # receipts are the ledger.
                    fresh_entries = [
                        (content_hashes[index], vectors[index])
                        for index in missing_indices
                        if content_hashes[index] and vectors[index] is not None
                    ]
                    if fresh_entries:
                        try:
                            await maybe_await(
                                cache_store(entries=fresh_entries, **cache_identity)
                            )
                        except Exception as cache_write_err:
                            logger.warning(
                                "vector cache write-back failed for migration %s "
                                "(non-fatal): %s",
                                migration_id,
                                cache_write_err,
                            )
                await self.vector_store.upsert(
                    collection_name=str(target_binding["collection_name"]),
                    points=points,
                )
                # Receipt AFTER successful point write: a crash between the two
                # only costs a re-embed of this page, never a false receipt.
                await self.store.record_progress_receipts(migration_id, receipts)
                embedded_total += len(receipts)
                await self.store.merge_migration_progress(
                    migration_id,
                    checkpoint={
                        "last_round": rounds,
                        "last_page_size": page_size,
                    },
                    totals={"embedded_in_run": embedded_total},
                )
        except Exception as exc:
            await self.store.transition_migration(
                migration_id,
                to_state="failed",
                from_states=("shadow_build", "backfilling"),
                error=f"{type(exc).__name__}: {exc}",
            )
            raise
        remaining = await self.store.count_pending_segments(
            migration_id, dataset_id=str(migration["dataset_id"])
        )
        total_enabled = await self.store.count_enabled_segments(str(migration["dataset_id"]))
        await self.store.merge_migration_progress(
            migration_id,
            totals={
                "enabled_chunks": total_enabled,
                "pending_after_backfill": remaining,
                "backfill_rounds": rounds,
            },
        )
        logger.info(
            "Backfill pass finished for migration %s: %d embedded, %d still pending",
            migration_id,
            embedded_total,
            remaining,
        )
        return {
            "embedded": embedded_total,
            "pending": remaining,
            "enabled_chunks": total_enabled,
            "rounds": rounds,
        }

    # -------------------------------------------------------- verify + gate

    async def verify(
        self,
        migration_id: str,
        *,
        action_job_id: str = "",
    ) -> dict[str, Any]:
        """Completeness check against the PostgreSQL authority, then verified.

        Qdrant cannot self-witness. ``verified`` requires both the receipt
        ledger and a live full-scope point/source digest to equal PostgreSQL,
        then pins that evidence into the existing migration ``totals`` JSON.
        """
        migration = await self.store.get_migration(migration_id)
        if migration is None:
            raise MigrationStateError("migration not found")
        pending, authority, target_scope = (
            await self._collect_migration_evidence(migration)
        )
        if pending:
            await self.store.merge_migration_progress(
                migration_id, totals={"pending_at_verify": pending}
            )
        self._require_evidence_agreement(
            pending=pending,
            authority=authority,
            target_scope=target_scope,
        )
        await self.store.merge_migration_progress(
            migration_id,
            totals={
                "verified_enabled_chunks": authority["point_count"],
                "verified_points": target_scope["point_count"],
                "verified_authority": authority,
                "verified_target_scope": target_scope,
                "verify_action_job_id": str(action_job_id or ""),
            },
        )
        moved = await self.store.transition_migration(
            migration_id,
            to_state="verified",
            from_states=("backfilling", "verified", "failed"),
        )
        if moved is None:
            raise MigrationStateError(
                f"migration in state '{migration['state']}' cannot move to verified"
            )
        moved = await self.store.get_migration(migration_id) or moved
        return {
            "migration": moved,
            "enabled_chunks": authority["point_count"],
            "points": target_scope["point_count"],
            "authority_snapshot": authority,
            "target_scope": target_scope,
        }

    async def run_gate(self, migration_id: str, evaluate: GateEvaluator) -> dict[str, Any]:
        """Apply the T0 evaluation gate; cutover is only legal after 'ready'.

        A migration may re-gate from 'gate_failed' after an evaluator failure
        while the verified corpus is unchanged. Corpus/target drift invalidates
        the generation and requires abort + fresh backfill. 'gating' is also
        accepted so a migration stranded mid-gate can escape forward by
        re-running the gate instead of only backward via abort.
        """
        moved = await self.store.transition_migration(
            migration_id,
            to_state="gating",
            from_states=("verified", "gate_failed", "gating"),
        )
        if moved is None:
            migration = await self.store.get_migration(migration_id)
            raise MigrationStateError(
                "the T0 gate needs a verified migration; run backfill+verify first "
                f"(state: {migration['state'] if migration else 'missing'})"
            )
        target_binding = await self.store.get_binding(str(moved["target_binding_id"]))
        source_binding = (
            await self.store.get_binding(str(moved["source_binding_id"]))
            if moved.get("source_binding_id")
            else None
        )
        context = {
            "migration": moved,
            "dataset_id": str(moved["dataset_id"]),
            "target_binding": target_binding,
            "source_binding": source_binding,
        }
        totals = moved.get("totals") if isinstance(moved.get("totals"), dict) else {}
        verified_authority = totals.get("verified_authority")
        verified_target_scope = totals.get("verified_target_scope")
        if not isinstance(verified_authority, dict) or not isinstance(
            verified_target_scope, dict
        ):
            error = "gate requires pinned verify authority evidence"
            await self.store.record_gate_verdict(
                migration_id,
                verdict={"passed": False, "error": error, "phase": "precheck"},
                passed=False,
            )
            raise EmbeddingMigrationError(f"{error}; abort and restart the migration")

        pre_authority: dict[str, Any] | None = None
        pre_target_scope: dict[str, Any] | None = None
        try:
            pending, pre_authority, pre_target_scope = (
                await self._collect_migration_evidence(moved)
            )
            self._require_evidence_agreement(
                pending=pending,
                authority=pre_authority,
                target_scope=pre_target_scope,
                verified_authority=verified_authority,
                verified_target_scope=verified_target_scope,
            )
        except Exception as exc:
            await self.store.record_gate_verdict(
                migration_id,
                verdict={
                    "passed": False,
                    "error": f"{type(exc).__name__}: {exc}",
                    "phase": "precheck",
                    "authority_snapshot": pre_authority,
                    "target_scope": pre_target_scope,
                },
                passed=False,
            )
            raise EmbeddingMigrationError(
                "evaluation gate authority precheck failed; abort and restart "
                "the migration"
            ) from exc

        try:
            verdict = await evaluate(context)
        except Exception as exc:
            await self.store.record_gate_verdict(
                migration_id,
                verdict={
                    "passed": False,
                    "error": f"{type(exc).__name__}: {exc}",
                    "phase": "evaluation",
                    "authority_snapshot": pre_authority,
                    "target_scope": pre_target_scope,
                },
                passed=False,
            )
            raise EmbeddingMigrationError(f"evaluation gate crashed: {exc}") from exc
        if not isinstance(verdict, dict):
            # Record a failing verdict so the migration escapes 'gating'
            # (gate_failed) instead of being stranded by a malformed
            # evaluator return; the state hole would otherwise need manual
            # surgery or an abort.
            await self.store.record_gate_verdict(
                migration_id,
                verdict={
                    "passed": False,
                    "error": "gate evaluator returned a non-dict verdict",
                    "phase": "evaluation",
                    "authority_snapshot": pre_authority,
                    "target_scope": pre_target_scope,
                },
                passed=False,
            )
            raise EmbeddingMigrationError("gate evaluator must return a dict verdict")

        # The evaluator may be slow. Recompute both authorities after it
        # returns and only persist ready when the corpus and target still
        # equal the verify snapshot it actually evaluated.
        try:
            pending, post_authority, post_target_scope = (
                await self._collect_migration_evidence(moved)
            )
            self._require_evidence_agreement(
                pending=pending,
                authority=post_authority,
                target_scope=post_target_scope,
                verified_authority=verified_authority,
                verified_target_scope=verified_target_scope,
            )
        except Exception as exc:
            failed_verdict = {
                **verdict,
                "passed": False,
                "error": f"authority changed during gate: {exc}",
                "phase": "postcheck",
                "authority_snapshot": pre_authority,
                "target_scope": pre_target_scope,
            }
            await self.store.record_gate_verdict(
                migration_id,
                verdict=failed_verdict,
                passed=False,
            )
            raise EmbeddingMigrationError(
                "evaluation gate authority changed; abort and restart the migration"
            ) from exc

        passed = bool(verdict.get("passed"))
        audited_verdict = {
            **verdict,
            "passed": passed,
            "authority_snapshot": post_authority,
            "target_scope": post_target_scope,
        }
        recorded = await self.store.record_gate_verdict(
            migration_id, verdict=audited_verdict, passed=passed
        )
        if recorded is None:  # pragma: no cover - state raced underneath us
            raise MigrationStateError("gate verdict raced with a state change")
        logger.info(
            "T0 gate for migration %s: %s",
            migration_id,
            "passed" if passed else "FAILED",
        )
        return {
            "migration": recorded,
            "verdict": audited_verdict,
            "passed": passed,
        }

    # ------------------------------------------------- cutover / rollback

    async def cutover(
        self,
        migration_id: str,
        *,
        retention_seconds: int = DEFAULT_RETENTION_SECONDS,
    ) -> dict[str, Any]:
        """Atomically flip the dataset to the new generation.

        Under the dataset-exclusive writer barrier, re-scroll the target and
        ask PostgreSQL to recompute pending/corpus evidence before its CAS.
        A gate-time drift is a 409 and leaves both bindings untouched. The old
        collection is only demoted after all evidence agrees.
        """
        migration = await self.store.get_migration(migration_id)
        if migration is None:
            raise MigrationStateError("migration not found")
        dataset_id = str(migration.get("dataset_id") or "")
        target_binding = await self.store.get_binding(
            str(migration.get("target_binding_id") or "")
        )
        if target_binding is None:
            raise MigrationStateError("target binding disappeared")
        async with self.store.dataset_exclusive_lease(dataset_id) as conn:
            try:
                target_scope = await self._scan_target_scope(target_binding)
            except EmbeddingMigrationError as exc:
                raise MigrationStateError(
                    f"fresh target collection evidence is unavailable: {exc}"
                ) from exc
            result = await self.store.cutover_migration(
                migration_id,
                retention_seconds=retention_seconds,
                target_scope=target_scope,
                _connection=conn,
                _dataset_lock_held=True,
            )
        logger.info(
            "Blue-green cutover completed for dataset %s (migration %s)",
            result["dataset_id"],
            migration_id,
        )
        return result

    async def rollback(self, migration_id: str, *, keep_shadow: bool = True) -> dict[str, Any]:
        """Rollback path (PRD §6.4-4): flip the pointer back to the retained old
        collection. ``keep_shadow=True`` (default) preserves the finished shadow
        generation for a retried gate+cutover; False retires it for reclamation.
        """
        migration = await self.store.get_migration(migration_id)
        target_binding = (
            await self.store.get_binding(str(migration["target_binding_id"]))
            if migration and migration.get("target_binding_id")
            else None
        )
        result = await self.store.rollback_migration(
            migration_id, target_state="shadow" if keep_shadow else "retired"
        )
        if not keep_shadow and target_binding is not None:
            # Zero orphans: a rolled-back-and-retired generation has no DB
            # claim left, so its physical collection goes with it.
            try:
                await self.vector_store.delete_collection(
                    str(target_binding["collection_name"])
                )
            except Exception:  # pragma: no cover - retryable cleanup
                logger.warning(
                    "rolled-back shadow collection delete failed for %s",
                    target_binding["collection_name"],
                    exc_info=True,
                )
        logger.info(
            "Rolled back dataset %s to the retained collection (migration %s)",
            result["dataset_id"],
            migration_id,
        )
        return result

    async def abort(self, migration_id: str, *, reason: str = "aborted", purge_shadow: bool = True) -> dict[str, Any]:
        """Terminate a pre-cutover migration.

        The serving generation is untouched (dataset keeps answering), the
        shadow binding is retired, and — when ``purge_shadow`` — the physical
        shadow collection is deleted (zero orphans).
        """
        migration = await self.store.get_migration(migration_id)
        if migration is None:
            raise MigrationStateError("migration not found")
        target_binding = await self.store.get_binding(str(migration["target_binding_id"]))
        result = await self.store.abandon_migration(migration_id, reason=reason)
        if purge_shadow and target_binding is not None:
            try:
                await self.vector_store.delete_collection(
                    str(target_binding["collection_name"])
                )
            except Exception:  # pragma: no cover - physical cleanup is retryable
                logger.warning(
                    "shadow collection delete failed for %s; it is retired and "
                    "remains reclaimable",
                    target_binding["collection_name"],
                    exc_info=True,
                )
        return result

    async def reclaim_expired(self, *, delete_collections: bool = True) -> list[str]:
        """Explicit reclamation: bindings past their retention period are
        retired (and their physical collections deleted on request). A retained
        binding is only ever reclaimed after it stopped serving AND the
        deadline lapsed — never the in-service collection."""
        reclaimed: list[str] = []
        for binding in await self.store.list_reclaimable_bindings():
            retired = await self.store.retire_binding(str(binding["binding_id"]))
            if retired is None:  # pragma: no cover - raced with a rollback
                continue
            reclaimed.append(str(retired["collection_name"]))
            if delete_collections:
                try:
                    await self.vector_store.delete_collection(
                        str(retired["collection_name"])
                    )
                except Exception:  # pragma: no cover - retryable
                    logger.warning(
                        "retired collection delete failed for %s",
                        retired["collection_name"],
                        exc_info=True,
                    )
        return reclaimed

    # ------------------------------------------------------- durable actions

    async def enqueue_action(
        self,
        migration_id: str,
        *,
        action: str,
        payload: dict[str, Any] | None = None,
        requested_by: str = "",
    ) -> dict[str, Any]:
        """Persist a long action and return immediately for HTTP 202."""

        await self.store.require_action_job_store()
        job, reused = await self.store.enqueue_action_job(
            migration_id,
            action=action,
            payload=payload,
            requested_by=requested_by,
        )
        return {
            **_public_action_job(job),
            "reused": reused,
            "poll_after_ms": 500,
        }

    async def get_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        await self.store.require_action_job_store()
        job = await self.store.get_scoped_action_job(
            job_id,
            migration_id=migration_id,
            dataset_id=dataset_id,
        )
        return _public_action_job(job) if job is not None else None

    async def cancel_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        await self.store.require_action_job_store()
        job = await self.store.cancel_scoped_action_job(
            job_id,
            migration_id=migration_id,
            dataset_id=dataset_id,
        )
        return _public_action_job(job) if job is not None else None

    # ------------------------------------------------------------- surface

    async def describe(self, dataset: dict[str, Any]) -> dict[str, Any]:
        """Refresh-safe migration control plane with explicit health evidence."""
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        await self.store.require_action_job_store()
        try:
            serving = await self.resolve_serving_binding(dict(dataset))
        except BindingConflictError as exc:
            # A legacy dataset whose collection name is reserved by another
            # binding cannot be adopted lazily. describe is a read-only
            # surface: degrade instead of letting a GET answer 409 forever.
            logger.warning(
                "describe cannot adopt a serving binding for dataset %s: %s",
                dataset_id,
                exc,
            )
            serving = None
        live = await self.store.get_live_migration(dataset_id)
        if live is None:
            # Failed attempts stay operator-actionable (resume backfill,
            # re-gate, abort): surface the newest one so its migration_id is
            # discoverable and the dataset is never wedged behind an
            # invisible shadow reservation.
            recoverable = getattr(self.store, "get_recoverable_migration", None)
            if recoverable is not None:
                live = await recoverable(dataset_id)

        recent_lookup = getattr(self.store, "list_recent_migrations", None)
        if callable(recent_lookup):
            recent = await recent_lookup(dataset_id, limit=5)
            latest = recent[0] if recent else None
        else:
            latest_lookup = getattr(self.store, "get_latest_migration", None)
            latest = (
                await latest_lookup(dataset_id)
                if callable(latest_lookup)
                else live
            )
            recent = [latest] if latest is not None else []
        selected = live or latest

        active, terminal = await self.store.describe_action_jobs(
            dataset_id, terminal_limit=10
        )
        active_action_job = (
            _public_action_job(active) if active is not None else None
        )
        recent_action_jobs = [_public_action_job(job) for job in terminal]

        pending = None
        if selected is not None:
            pending = await self.store.count_pending_segments(
                str(selected["migration_id"]), dataset_id=dataset_id
            )

        source_binding = None
        target_binding = None
        binding_lookup = getattr(self.store, "get_binding", None)
        if selected is not None and callable(binding_lookup):
            if selected.get("source_binding_id"):
                source_binding = await binding_lookup(
                    str(selected["source_binding_id"])
                )
            if selected.get("target_binding_id"):
                target_binding = await binding_lookup(
                    str(selected["target_binding_id"])
                )

        totals = (
            selected.get("totals")
            if selected is not None and isinstance(selected.get("totals"), dict)
            else {}
        )
        gate_report = (
            selected.get("gate")
            if selected is not None and isinstance(selected.get("gate"), dict)
            else None
        )
        collection_health: dict[str, Any] = {
            "status": "unknown",
            "checked_live": False,
            "collection_name": (
                str(target_binding.get("collection_name") or "")
                if target_binding is not None
                else None
            ),
            "pending_chunks": pending,
            "authority": None,
            "target_scope": None,
            "verified_authority": totals.get("verified_authority"),
            "verified_target_scope": totals.get("verified_target_scope"),
            "gate_report": gate_report,
            "reason": "no migration target is available",
        }
        if (
            selected is not None
            and target_binding is not None
            and target_binding.get("state") != "retired"
        ):
            try:
                authority = await self.store.authority_snapshot(dataset_id)
                target_scope = await self._scan_target_scope(target_binding)
            except Exception as exc:
                logger.warning(
                    "live embedding collection health check failed for dataset %s: %s",
                    dataset_id,
                    exc,
                )
                collection_health["reason"] = (
                    "live collection health check is unavailable"
                )
            else:
                healthy = not pending and authority_matches_scope(
                    authority, target_scope
                )
                collection_health.update(
                    {
                        "status": "healthy" if healthy else "drifted",
                        "checked_live": True,
                        "authority": authority,
                        "target_scope": target_scope,
                        "reason": None
                        if healthy
                        else "pending chunks or point/source digest mismatch",
                    }
                )
        elif target_binding is not None and target_binding.get("state") == "retired":
            collection_health["reason"] = "migration target binding is retired"
        return {
            "dataset_id": dataset_id,
            "serving_binding": serving,
            "live_migration": live,
            "latest_migration": latest,
            "recent_migrations": recent,
            "source_binding": source_binding,
            "target_binding": target_binding,
            "active_action_job": active_action_job,
            "recent_action_jobs": recent_action_jobs,
            "pending_chunks": pending,
            "enabled_chunks": await self.store.count_enabled_segments(dataset_id),
            "collection_health": collection_health,
        }
