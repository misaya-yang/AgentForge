"""PRD T6: the BM25 v2 cross-storage cutover/rollback protocol.

This module owns the *protocol*, not the release decision. The service kill
switch (``KNOWLEDGE_QDRANT__BM25_V2_ENABLED``, default off) still gates every
active flip here, so production stays hard-disabled until the separate
release review turns it on. What T6 delivers is the machine that may then
move a dataset ``lexical_v1 -> bm25_v2`` and back without a retrieval error
or data loss under concurrent ingestion load:

1. **Write exclusion** — steady active-v2 ingestion and segment lifecycle
   writes are supported. Each publication holds the shared
   ``knowledge-dataset-index`` session lock from receipt invalidation through
   cross-verification and authority CAS. Only
   ``Bm25V2LifecycleStore.transition_barrier`` takes the conflicting exclusive
   lock, so cutover/rollback exclude all writers while the durable queue keeps
   active datasets dispatchable in steady state.
2. **Two-phase confirmation** — the completion receipt (the "finished
   backfill" proof) is first swapped for the ``invalidated`` sentinel (the
   existing pattern from runtime writes), then the collection metadata and
   finally the PostgreSQL profile are flipped, and only after a *live*
   re-derivation of agreement between PostgreSQL authority and the Qdrant
   lexical scope is a fresh cutover-certified receipt published. A receipt
   is never trusted as pre-existing evidence under load — concurrent shadow
   writes invalidate it by design, so cutover recomputes everything itself.
3. **Ordering** — cutover flips Qdrant first and PostgreSQL last (PostgreSQL
   is the read authority; the one-way inconsistency "collection says v2,
   PostgreSQL still says v1" is explicitly servable). Rollback flips
   PostgreSQL first and Qdrant last for the same reason. Either direction,
   no window makes an honest reader see an index that cannot serve it.
4. **Rollback is always available** — the ``bm25_v2`` field, its vectors, the
   point markers, and the lifecycle row's evidence are retained; rollback
   only changes selection.
5. **Crash recovery without TTLs** — lifecycle state lives in
   ``kb_bm25_v2_lifecycle`` (migration 105) with an epoch+token CAS. A new
   executor that wins the barrier may reset an in-progress row it finds,
   because winning the barrier proves no live executor owns it.

Dataset-service user-facing boundaries are untouched: ordinary dataset
create/update still rejects an externally requested active version — the CAS
in the lifecycle store is the only writer of an active lexical profile.
"""

from __future__ import annotations

import asyncio
import logging
import time
from typing import Any

from ...persistence.bm25_v2_lifecycle import (
    AuthoritySnapshot,
    Bm25V2LifecycleStore,
    LifecycleStateConflict,
)
from .lexical_config import (
    BM25_V2,
    BM25_V2_AUTHORITY_KIND,
    LEXICAL_V1,
    LexicalConfig,
    LexicalConfigError,
)
from .lifecycle_errors import Bm25V2LifecycleError as Bm25V2LifecycleError
from .vector_store import VectorStore, VectorStoreError

logger = logging.getLogger(__name__)

POINT_ID_ALGORITHM = "sha256(sorted-point-id-newline-v1)"
SOURCE_TEXT_ALGORITHM = "sha256(sorted-point-id-text-sha256-null-newline-v1)"


class QuiescenceTimeout(Bm25V2LifecycleError):
    def __init__(self, message: str) -> None:
        super().__init__(message, code="bm25_v2_quiescence_timeout", http_status=503)


class CrossAuthorityMismatch(Bm25V2LifecycleError):
    def __init__(self, message: str) -> None:
        super().__init__(message, code="bm25_v2_cross_authority_mismatch")


def _profile_summary(profile: LexicalConfig | None) -> dict[str, Any] | None:
    if profile is None:
        return None
    return {
        "active_version": profile.active_version,
        "shadow_write_enabled": profile.bm25_v2_shadow_write_enabled,
        "schema_fingerprint": profile.bm25_v2.fingerprint,
        "filtering_fingerprint": profile.filtering.fingerprint,
        "runtime_revision": profile.runtime_revision,
    }


class Bm25V2LifecycleService:
    """Cutover/rollback orchestration across PostgreSQL and Qdrant for T6."""

    def __init__(
        self,
        *,
        vector_store: VectorStore,
        lifecycle_store: Bm25V2LifecycleStore,
        quiesce_timeout_s: float = 60.0,
        quiesce_interval_s: float = 0.25,
        barrier_wait_s: float = 5.0,
    ) -> None:
        self.vector_store = vector_store
        self.lifecycle = lifecycle_store
        self.quiesce_timeout_s = float(quiesce_timeout_s)
        self.quiesce_interval_s = float(quiesce_interval_s)
        # Bound on tolerating transient write-lease holders during barrier
        # acquisition; sustained contention still fails closed busy.
        self.barrier_wait_s = float(barrier_wait_s)

    # ------------------------------------------------------------- read side

    async def get_lifecycle_state(self, dataset_id: str) -> dict[str, Any]:
        """Combined persistent + live view for the state endpoint (no barrier)."""

        snap = await self.lifecycle.dataset_snapshot(dataset_id)
        if snap is None or snap["is_deleted"]:
            raise Bm25V2LifecycleError(
                "dataset does not exist", code="bm25_v2_dataset_missing", http_status=404
            )
        row = await self.lifecycle.get_state(dataset_id)
        collection = snap["collection_name"]
        col_profile, receipt = await self.vector_store.get_live_lexical_profile(collection)
        return {
            "dataset_id": dataset_id,
            "tenant_id": snap["tenant_id"],
            "collection_name": collection,
            "content_revision": snap["content_revision"],
            "lifecycle": row,
            "state": (row or {}).get("state") or "shadow",
            "postgres_profile": _profile_summary(_pg_profile(snap)),
            "collection_profile": _profile_summary(col_profile),
            "receipt_status": (receipt or {}).get("status"),
            "busy_documents": await self.lifecycle.count_busy_documents(dataset_id),
            "dispatchable_documents": await self.lifecycle.count_dispatchable_documents(
                dataset_id
            ),
        }

    async def verify_cross_authority(self, dataset_id: str) -> dict[str, Any]:
        """Recompute PG-authority vs Qdrant-scope agreement (no barrier, read-only).

        A mismatch here without a transition in flight means evidence drift;
        the correct response is (re-)backfill, not cutover. Reported, never
        silently repaired.
        """

        snap, pg_profile, col_profile, _receipt = await self._preflight(dataset_id)
        authority = await self.lifecycle.authority_snapshot(
            collection_name=snap["collection_name"],
            tenant_id=snap["tenant_id"],
            dataset_id=dataset_id,
        )
        scope = await self.vector_store.scan_bm25_v2_lexical_scope(
            snap["collection_name"],
            tenant_id=snap["tenant_id"],
            dataset_id=dataset_id,
            config=col_profile if col_profile is not None else pg_profile,
        )
        agreement = (
            authority.point_count == scope["point_count"]
            and authority.point_ids_sha256 == scope["point_ids_sha256"]
            and authority.source_text_sha256 == scope["source_text_sha256"]
            and scope["complete_count"] == scope["point_count"]
        )
        return {
            "dataset_id": dataset_id,
            "agreement": agreement,
            "postgres_authority": {
                "point_count": authority.point_count,
                "point_ids_sha256": authority.point_ids_sha256,
                "source_text_sha256": authority.source_text_sha256,
                "content_revision": authority.content_revision,
                "authority_kind": authority.authority_kind,
            },
            "qdrant_scope": scope,
        }

    async def active_publication_context(
        self,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        """Freeze the steady active-v2 evidence epoch for one shared writer.

        The caller holds the shared ``knowledge-dataset-index`` lease, so an
        exclusive cutover/rollback barrier cannot start between this preflight
        and the later publication CAS.
        """

        snap, pg_profile, col_profile, _receipt = await self._preflight(dataset_id)
        if not pg_profile.reads_bm25_v2:
            if col_profile is not None and col_profile.reads_bm25_v2:
                raise Bm25V2LifecycleError(
                    "collection selects bm25_v2 while PostgreSQL selects lexical_v1",
                    code="bm25_v2_split_profile",
                )
            return None
        if not self.vector_store.bm25_v2_enabled:
            raise Bm25V2LifecycleError(
                "bm25_v2 is disabled by the service kill switch",
                code="bm25_v2_disabled",
                http_status=503,
            )
        if col_profile is None or not col_profile.reads_bm25_v2:
            raise Bm25V2LifecycleError(
                "PostgreSQL selects bm25_v2 but the collection does not",
                code="bm25_v2_split_profile",
            )
        if (
            pg_profile.bm25_v2.fingerprint != col_profile.bm25_v2.fingerprint
            or pg_profile.filtering.fingerprint != col_profile.filtering.fingerprint
        ):
            raise Bm25V2LifecycleError(
                "active bm25_v2 fingerprints disagree across authorities",
                code="bm25_v2_fingerprint_mismatch",
            )
        row = await self.lifecycle.get_state(dataset_id)
        if (
            row is None
            or row.get("state") != "active_v2"
            or row.get("transition_kind") is not None
            or row.get("lock_token") is not None
        ):
            raise Bm25V2LifecycleError(
                "active bm25_v2 lifecycle evidence is absent or not steady",
                code="bm25_v2_state_conflict",
            )
        return {
            "dataset_id": dataset_id,
            "tenant_id": snap["tenant_id"],
            "collection_name": snap["collection_name"],
            "epoch": int(row["epoch"]),
            "profile": pg_profile,
        }

    async def recertify_active_publication(
        self,
        context: dict[str, Any],
        *,
        publication_revision: int,
        connection: Any,
    ) -> dict[str, Any]:
        """Full-scroll the publisher's uncommitted PG authority before its commit.

        The Qdrant scope already carries candidate points. Certification must
        compare it with candidate PostgreSQL rows on this same connection;
        opening another connection would see only the prior serving generation.
        """

        dataset_id = str(context["dataset_id"])
        tenant_id = str(context["tenant_id"])
        collection = str(context["collection_name"])
        profile = context["profile"]
        initial_revision = int(publication_revision)
        if initial_revision >= 0:
            raise LifecycleStateConflict(
                "BM25 v2 certification requires a negative publication revision"
            )
        authority = await self.lifecycle.authority_snapshot(
            collection_name=collection,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            connection=connection,
        )
        # Segment/document statement triggers may increment the reserved
        # negative seqlock while PostgreSQL authority is committed. The shared
        # session publication lock proves this is still the same publisher;
        # freshness comes from the latest negative authority value, not the
        # lease's earlier marker.
        if authority.content_revision >= 0:
            raise LifecycleStateConflict(
                "dataset publication fence closed before BM25 v2 certification"
            )
        target_revision = abs(authority.content_revision) + 1
        scope = await self.vector_store.scan_bm25_v2_lexical_scope(
            collection,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            config=profile,
        )
        self._require_agreement(authority, scope)
        receipt = {
            "schema_version": 1,
            "status": "complete",
            "collection_name": collection,
            "bm25_v2_schema_fingerprint": profile.bm25_v2.fingerprint,
            "filtering_profile_fingerprint": profile.filtering.fingerprint,
            "dataset_id": dataset_id,
            "tenant_id": tenant_id,
            "point_count": scope["point_count"],
            "point_ids_sha256": scope["point_ids_sha256"],
            "manifest_algorithm": POINT_ID_ALGORITHM,
            "source_text_sha256": scope["source_text_sha256"],
            "source_text_algorithm": SOURCE_TEXT_ALGORITHM,
            "authority_kind": BM25_V2_AUTHORITY_KIND,
            "authority_content_revision": target_revision,
            "certified_by": "bm25_v2_runtime_publication",
        }
        await self.vector_store.publish_bm25_v2_cutover_receipt(
            collection,
            receipt=receipt,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
        )
        readiness = await self.vector_store.verify_bm25_v2_active_readiness(
            collection,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            config=profile,
        )
        return {
            "expected_epoch": int(context["epoch"]),
            "target_revision": target_revision,
            "manifest_sha256": scope["point_ids_sha256"],
            "post_evidence": {
                "runtime_publication": True,
                "authority": {
                    **_authority_dict(authority),
                    "content_revision": target_revision,
                },
                "readiness": readiness,
            },
        }

    async def settle_active_publication(
        self,
        context: dict[str, Any],
        certification: dict[str, Any],
        *,
        connection: Any,
    ) -> int:
        """CAS lifecycle evidence in the caller's authority transaction."""

        return await self.lifecycle.certify_active_publication(
            dataset_id=str(context["dataset_id"]),
            tenant_id=str(context["tenant_id"]),
            expected_epoch=int(certification["expected_epoch"]),
            authority_content_revision=int(certification["target_revision"]),
            manifest_sha256=str(certification["manifest_sha256"]),
            post_evidence=dict(certification["post_evidence"]),
            connection=connection,
        )

    # ---------------------------------------------------------- transitions

    async def cutover(
        self,
        dataset_id: str,
        *,
        apply: bool = True,
        expected_manifest_sha256: str | None = None,
    ) -> dict[str, Any]:
        """Move one dataset's lexical selection ``lexical_v1 -> bm25_v2``.

        Requires the shadow profile configured in PostgreSQL, a matching
        collection fingerprint, and the service kill switch on. Idempotent
        when already active.
        """

        snap, pg_profile, col_profile, receipt = await self._preflight(dataset_id)
        collection = snap["collection_name"]
        tenant_id = snap["tenant_id"]

        if pg_profile.reads_bm25_v2:
            if col_profile is None or not col_profile.reads_bm25_v2:
                raise Bm25V2LifecycleError(
                    "PostgreSQL selects bm25_v2 but the collection is not cut over; "
                    "run rollback or repair the collection metadata",
                    code="bm25_v2_split_profile",
                )
            return {
                "dataset_id": dataset_id,
                "applied": False,
                "already_active": True,
                "state": "active_v2",
                "collection_name": collection,
            }
        if not pg_profile.writes_bm25_v2:
            raise Bm25V2LifecycleError(
                "cutover requires the bm25_v2 shadow profile enabled first "
                "(shadow writes must precede any cutover)",
                code="bm25_v2_shadow_not_enabled",
            )
        if col_profile is None or not col_profile.writes_bm25_v2:
            raise Bm25V2LifecycleError(
                "collection does not carry the bm25_v2 shadow profile",
                code="bm25_v2_shadow_not_enabled",
            )
        if (
            col_profile.bm25_v2.fingerprint != pg_profile.bm25_v2.fingerprint
            or col_profile.filtering.fingerprint != pg_profile.filtering.fingerprint
        ):
            raise Bm25V2LifecycleError(
                "frozen fingerprints disagree between PostgreSQL and the collection; "
                "refusing to cutover (backfill a fresh profile instead)",
                code="bm25_v2_fingerprint_mismatch",
            )
        if apply and not self.vector_store.bm25_v2_enabled:
            raise Bm25V2LifecycleError(
                "bm25_v2 is disabled by the service kill switch",
                code="bm25_v2_disabled",
                http_status=503,
            )

        row = await self.lifecycle.get_state(dataset_id)
        if row is None:
            await self.lifecycle.ensure_row(dataset_id=dataset_id, tenant_id=tenant_id)
            row = await self.lifecycle.get_state(dataset_id)
            assert row is not None

        plan = {
            "dataset_id": dataset_id,
            "collection_name": collection,
            "from_state": row["state"],
            "receipt_status_before": (receipt or {}).get("status"),
            "postgres_profile": _profile_summary(pg_profile),
            "collection_profile": _profile_summary(col_profile),
        }
        if not apply:
            authority = await self.lifecycle.authority_snapshot(
                collection_name=collection,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
            scope = await self.vector_store.scan_bm25_v2_lexical_scope(
                collection,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                config=col_profile,
            )
            self._require_agreement(authority, scope)
            if (
                expected_manifest_sha256
                and str(expected_manifest_sha256) != scope["point_ids_sha256"]
            ):
                raise Bm25V2LifecycleError(
                    "expected manifest digest does not match the recomputed lexical scope",
                    code="bm25_v2_manifest_mismatch",
                    http_status=400,
                )
            return {
                "applied": False,
                "dry_run": True,
                **plan,
                "verification": {
                    "authority": _authority_dict(authority),
                    "qdrant_scope": scope,
                    "agreement": True,
                },
            }

        async with self.lifecycle.transition_barrier(dataset_id, wait_s=self.barrier_wait_s):
            row = await self._recover_stale_row(dataset_id, tenant_id)
            if row["state"] != "shadow":
                raise Bm25V2LifecycleError(
                    f"cutover requires the steady 'shadow' lifecycle state, "
                    f"found {row['state']!r}",
                    code="bm25_v2_state_conflict",
                )
            authority = await self.lifecycle.authority_snapshot(
                collection_name=collection, tenant_id=tenant_id, dataset_id=dataset_id
            )
            epoch, token = await self.lifecycle.begin_transition(
                dataset_id=dataset_id,
                tenant_id=tenant_id,
                kind="cutover",
                from_state="shadow",
                authority_content_revision=authority.content_revision,
            )
            pg_flipped = False
            qdrant_flipped = False
            try:
                # (1) queue quiescence — claims cannot even be taken while the
                # barrier is held, this drains what is already in flight.
                await self._await_quiescence(dataset_id)

                # (2) live cross-store verification (never a stale receipt).
                pre_scope = await self.vector_store.scan_bm25_v2_lexical_scope(
                    collection,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    config=col_profile,
                )
                self._require_agreement(authority, pre_scope)
                if expected_manifest_sha256 and str(expected_manifest_sha256) != pre_scope[
                    "point_ids_sha256"
                ]:
                    raise Bm25V2LifecycleError(
                        "expected manifest digest does not match the recomputed "
                        "lexical scope; refusing the cutover",
                        code="bm25_v2_manifest_mismatch",
                        http_status=400,
                    )

                # (3) two-phase confirm, part 1: retire any stale proof.
                await self.vector_store.invalidate_bm25_v2_receipt(
                    collection, reason="bm25_v2_cutover_phase1"
                )

                # (4) Qdrant metadata flips first (v2-metadata/v1-selection is
                # the one servable inconsistency direction).
                requested = pg_profile.with_runtime_selection(
                    active_version=BM25_V2,
                    shadow_write_enabled=True,
                    filtering=pg_profile.filtering,
                )
                await self.vector_store.ensure_lexical_config(
                    collection,
                    requested,
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    allow_runtime_transition=True,
                    active_cutover_authorized=True,
                )
                qdrant_flipped = True

                # (5) certify from fresh evidence, not from pre-existing state.
                post_scope = await self.vector_store.scan_bm25_v2_lexical_scope(
                    collection,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    config=requested,
                )
                self._require_agreement(authority, post_scope)
                receipt_payload = {
                    "schema_version": 1,
                    "status": "complete",
                    "collection_name": collection,
                    "bm25_v2_schema_fingerprint": requested.bm25_v2.fingerprint,
                    "filtering_profile_fingerprint": requested.filtering.fingerprint,
                    "dataset_id": dataset_id,
                    "tenant_id": tenant_id,
                    "point_count": post_scope["point_count"],
                    "point_ids_sha256": post_scope["point_ids_sha256"],
                    "manifest_algorithm": POINT_ID_ALGORITHM,
                    "source_text_sha256": post_scope["source_text_sha256"],
                    "source_text_algorithm": SOURCE_TEXT_ALGORITHM,
                    "authority_kind": BM25_V2_AUTHORITY_KIND,
                    "authority_content_revision": authority.content_revision,
                    "certified_by": "bm25_v2_lifecycle_cutover",
                }
                await self.vector_store.publish_bm25_v2_cutover_receipt(
                    collection,
                    receipt=receipt_payload,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                )

                # (6) PostgreSQL flips last; it is the read authority.
                flipped = await self.lifecycle.flip_dataset_lexical_active_version(
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    expected_active_version=LEXICAL_V1,
                    target_active_version=BM25_V2,
                    shadow_write_enabled=True,
                    expected_content_revision=authority.content_revision,
                )
                pg_flipped = True

                # (7) post-verify with the *active* per-query gate itself.
                post = await self.vector_store.verify_bm25_v2_active_readiness(
                    collection,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                    config=requested,
                )
                if post.get("point_ids_sha256") != authority.point_ids_sha256 or post.get(
                    "source_text_sha256"
                ) != authority.source_text_sha256:
                    raise CrossAuthorityMismatch(
                        "post-cutover readiness disagrees with the PostgreSQL "
                        "authority snapshot"
                    )

                await self.lifecycle.finish_transition(
                    dataset_id=dataset_id,
                    epoch=epoch,
                    lock_token=token,
                    in_progress_state="cutover_in_progress",
                    target_state="active_v2",
                    pre_evidence={
                        "authority": _authority_dict(authority),
                        "pre_scope": pre_scope,
                        "receipt_status_before": (receipt or {}).get("status"),
                    },
                    post_evidence={"readiness": post, "flipped": flipped},
                    manifest_sha256=receipt_payload["point_ids_sha256"],
                    authority_content_revision=authority.content_revision,
                )
                return {
                    "applied": True,
                    "dry_run": False,
                    "dataset_id": dataset_id,
                    "collection_name": collection,
                    "state": "active_v2",
                    "epoch": epoch,
                    "manifest_sha256": receipt_payload["point_ids_sha256"],
                    "authority_content_revision": authority.content_revision,
                    "readiness": post,
                }
            except Exception as exc:
                await self._abort(
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    collection=collection,
                    epoch=epoch,
                    token=token,
                    in_progress_state="cutover_in_progress",
                    recovered_state="shadow",
                    pg_flipped=pg_flipped,
                    qdrant_flipped=qdrant_flipped,
                    error=exc,
                )
                if isinstance(exc, Bm25V2LifecycleError):
                    raise
                raise Bm25V2LifecycleError(
                    f"bm25_v2 cutover failed: {exc}", code="bm25_v2_cutover_failed"
                ) from exc

    async def rollback(
        self,
        dataset_id: str,
        *,
        apply: bool = True,
        keep_shadow_writes: bool = True,
    ) -> dict[str, Any]:
        """Move one dataset back ``bm25_v2 -> lexical_v1``. Always available.

        The ``bm25_v2`` field, vectors, markers, and lifecycle evidence stay
        in place; only the selection changes, so a re-cutover is a cheap
        re-verify afterwards. ``keep_shadow_writes`` defaults on: v1 serving
        with v2 shadow writes continues accruing evidence. With the service
        kill switch off the shadow flag is forced off (the collection write
        path can no longer prove v2 anyway, matching the existing
        emergency-rollback semantics in ``ensure_lexical_config``).
        """

        snap, pg_profile, col_profile, receipt = await self._preflight(dataset_id)
        collection = snap["collection_name"]
        tenant_id = snap["tenant_id"]

        row = await self.lifecycle.get_state(dataset_id)

        if not pg_profile.reads_bm25_v2 and (row is None or row["state"] == "shadow"):
            if col_profile is not None and col_profile.reads_bm25_v2:
                raise Bm25V2LifecycleError(
                    "collection metadata says bm25_v2 while PostgreSQL selects "
                    "lexical_v1 — run verify; refusing a blind rollback",
                    code="bm25_v2_split_profile",
                )
            return {
                "dataset_id": dataset_id,
                "applied": False,
                "already_inactive": True,
                "state": "shadow",
                "collection_name": collection,
            }
        if not keep_shadow_writes or not self.vector_store.bm25_v2_enabled:
            keep_shadow_writes = bool(keep_shadow_writes and self.vector_store.bm25_v2_enabled)

        plan = {
            "dataset_id": dataset_id,
            "collection_name": collection,
            "from_state": (row or {}).get("state") or "shadow",
            "keep_shadow_writes": keep_shadow_writes,
            "postgres_profile": _profile_summary(pg_profile),
        }
        if not apply:
            return {"applied": False, "dry_run": True, **plan}

        async with self.lifecycle.transition_barrier(dataset_id, wait_s=self.barrier_wait_s):
            row = await self._recover_stale_row(dataset_id, tenant_id)
            if row["state"] != "active_v2":
                raise Bm25V2LifecycleError(
                    f"rollback requires the steady 'active_v2' lifecycle state, "
                    f"found {row['state']!r}",
                    code="bm25_v2_state_conflict",
                )
            authority = await self.lifecycle.authority_snapshot(
                collection_name=collection, tenant_id=tenant_id, dataset_id=dataset_id
            )
            epoch, token = await self.lifecycle.begin_transition(
                dataset_id=dataset_id,
                tenant_id=tenant_id,
                kind="rollback",
                from_state="active_v2",
                authority_content_revision=authority.content_revision,
            )
            pg_flipped = False
            try:
                # (1) two-phase confirm, part 1: retire the cutover proof first
                # so no replica can serve active mode from it mid-transition.
                await self.vector_store.invalidate_bm25_v2_receipt(
                    collection, reason="bm25_v2_rollback_phase1"
                )

                # (2) PostgreSQL flips first on rollback: reads move to the
                # v1 leg while the collection still says v2 — the explicitly
                # servable direction.
                await self.lifecycle.flip_dataset_lexical_active_version(
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    expected_active_version=BM25_V2,
                    target_active_version=LEXICAL_V1,
                    shadow_write_enabled=keep_shadow_writes,
                    expected_content_revision=authority.content_revision,
                )
                pg_flipped = True

                # (3) Qdrant metadata follows.
                reverted = (col_profile or pg_profile).with_runtime_selection(
                    active_version=LEXICAL_V1,
                    shadow_write_enabled=keep_shadow_writes,
                    filtering=(col_profile or pg_profile).filtering,
                )
                await self.vector_store.ensure_lexical_config(
                    collection,
                    reverted,
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    allow_runtime_transition=True,
                )

                # (4) post-verify: legacy selection is readable again.
                await self.vector_store.require_collection_readable(
                    collection, tenant_id=tenant_id, dataset_id=dataset_id
                )

                await self.lifecycle.finish_transition(
                    dataset_id=dataset_id,
                    epoch=epoch,
                    lock_token=token,
                    in_progress_state="rollback_in_progress",
                    target_state="shadow",
                    pre_evidence={"receipt_status_before": (receipt or {}).get("status")},
                    post_evidence={
                        "keep_shadow_writes": keep_shadow_writes,
                        "v2_field_retained": True,
                    },
                    # ``begin_transition`` cleared the digest for the
                    # in-progress row; restore the cutover-certified digest
                    # so the steady shadow row keeps recording which v2
                    # content is retained.
                    manifest_sha256=str(row.get("manifest_sha256") or ""),
                    authority_content_revision=None,
                )
                return {
                    "applied": True,
                    "dry_run": False,
                    "dataset_id": dataset_id,
                    "collection_name": collection,
                    "state": "shadow",
                    "epoch": epoch,
                    "keep_shadow_writes": keep_shadow_writes,
                    "v2_data_retained": True,
                }
            except Exception as exc:
                await self._abort(
                    dataset_id=dataset_id,
                    tenant_id=tenant_id,
                    collection=collection,
                    epoch=epoch,
                    token=token,
                    in_progress_state="rollback_in_progress",
                    recovered_state="active_v2",
                    pg_flipped=pg_flipped,
                    qdrant_flipped=False,
                    # On revert the rollback path restores the active
                    # selection; the receipt stays invalidated, so active
                    # reads fail closed until a fresh verify re-certifies it.
                    error=exc,
                )
                if isinstance(exc, Bm25V2LifecycleError):
                    raise
                raise Bm25V2LifecycleError(
                    f"bm25_v2 rollback failed: {exc}", code="bm25_v2_rollback_failed"
                ) from exc

    # -------------------------------------------------------------- internals

    async def _preflight(
        self, dataset_id: str
    ) -> tuple[dict[str, Any], LexicalConfig, LexicalConfig | None, dict[str, Any] | None]:
        snap = await self.lifecycle.dataset_snapshot(dataset_id)
        if snap is None or snap["is_deleted"]:
            raise Bm25V2LifecycleError(
                "dataset does not exist", code="bm25_v2_dataset_missing", http_status=404
            )
        if not snap["collection_name"]:
            raise Bm25V2LifecycleError(
                "dataset has no persisted collection", code="bm25_v2_no_collection",
                http_status=400,
            )
        try:
            col_profile, receipt = await self.vector_store.get_live_lexical_profile(
                snap["collection_name"]
            )
        except VectorStoreError as exc:
            raise Bm25V2LifecycleError(str(exc), code="bm25_v2_collection_unreadable") from exc
        return snap, _pg_profile(snap), col_profile, receipt

    async def _recover_stale_row(self, dataset_id: str, tenant_id: str) -> dict[str, Any]:
        """Normalize the lifecycle row to a steady state under the barrier."""

        row = await self.lifecycle.get_state(dataset_id)
        if row is None:
            await self.lifecycle.ensure_row(dataset_id=dataset_id, tenant_id=tenant_id)
            row = await self.lifecycle.get_state(dataset_id)
            assert row is not None
        snap = await self.lifecycle.dataset_snapshot(dataset_id)
        steady = "shadow"
        if snap is not None:
            try:
                if _pg_profile(snap).reads_bm25_v2:
                    steady = "active_v2"
            except Bm25V2LifecycleError:
                steady = "shadow"
        if row["state"] in ("cutover_in_progress", "rollback_in_progress"):
            recovered = await self.lifecycle.reset_stale_transition(
                dataset_id=dataset_id,
                in_progress_state=row["state"],
                recovered_state=steady,
                error="reset by a barrier-winning executor after a dead transition",
            )
            if not recovered:
                raise LifecycleStateConflict(
                    f"dataset {dataset_id} in-progress row vanished during recovery"
                )
        elif row["state"] != steady:
            # Steady-state divergence: the row contradicts the PostgreSQL
            # profile (e.g. a failed revert in _abort left 'shadow' while the
            # profile moved, or a pre-table cutover). The row is evidence,
            # not authority — reconcile it to the storage truth so rollback
            # stays available (protocol principle 4).
            reconciled = await self.lifecycle.reconcile_steady_state(
                dataset_id=dataset_id,
                from_state=str(row["state"]),
                target_state=steady,
                error="reconciled steady state with the PostgreSQL lexical profile",
            )
            if not reconciled:
                raise LifecycleStateConflict(
                    f"dataset {dataset_id} steady-state row vanished during reconcile"
                )
        row = await self.lifecycle.get_state(dataset_id)
        assert row is not None
        return row

    async def _await_quiescence(self, dataset_id: str) -> None:
        deadline = time.monotonic() + self.quiesce_timeout_s
        while True:
            busy = await self.lifecycle.count_busy_documents(dataset_id)
            if busy == 0:
                return
            if time.monotonic() >= deadline:
                raise QuiescenceTimeout(
                    f"dataset {dataset_id} still has {busy} documents in flight "
                    f"after {self.quiesce_timeout_s:.0f}s; aborting the transition"
                )
            await asyncio.sleep(self.quiesce_interval_s)

    @staticmethod
    def _require_agreement(authority: AuthoritySnapshot, scope: dict[str, Any]) -> None:
        if scope["complete_count"] != scope["point_count"]:
            raise CrossAuthorityMismatch(
                f"{scope['point_count'] - scope['complete_count']} lexical points "
                "lack a complete bm25_v2 marker; run the backfill first"
            )
        if (
            authority.point_count != scope["point_count"]
            or authority.point_ids_sha256 != scope["point_ids_sha256"]
            or authority.source_text_sha256 != scope["source_text_sha256"]
        ):
            raise CrossAuthorityMismatch(
                "PostgreSQL authority and the Qdrant lexical scope disagree "
                "(count/digests differ); run the backfill before cutting over"
            )

    async def _abort(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        collection: str,
        epoch: int,
        token: str,
        in_progress_state: str,
        recovered_state: str,
        pg_flipped: bool,
        qdrant_flipped: bool,
        error: Exception,
    ) -> None:
        logger.error(
            "bm25_v2 %s failed for dataset %s; reverting (pg_flipped=%s, "
            "qdrant_flipped=%s): %s",
            in_progress_state,
            dataset_id,
            pg_flipped,
            qdrant_flipped,
            error,
        )
        try:
            snap = await self.lifecycle.dataset_snapshot(dataset_id)
            # Reverse order of the flips relative to the failed direction:
            # undo PostgreSQL then Qdrant for a cutover (pg last in, first
            # out) — each guarded by its own CAS so we never clobber a
            # selection that never actually moved.
            if pg_flipped and snap is not None:
                if in_progress_state == "cutover_in_progress":
                    await self.lifecycle.flip_dataset_lexical_active_version(
                        dataset_id=dataset_id,
                        tenant_id=tenant_id,
                        expected_active_version=BM25_V2,
                        target_active_version=LEXICAL_V1,
                        shadow_write_enabled=True,
                        expected_content_revision=int(snap["content_revision"]),
                    )
                else:  # rollback failed mid-way: restore the active selection
                    await self.lifecycle.flip_dataset_lexical_active_version(
                        dataset_id=dataset_id,
                        tenant_id=tenant_id,
                        expected_active_version=LEXICAL_V1,
                        target_active_version=BM25_V2,
                        shadow_write_enabled=True,
                        expected_content_revision=int(snap["content_revision"]),
                    )
            if in_progress_state == "cutover_in_progress" and qdrant_flipped:
                col_profile, _ = await self.vector_store.get_live_lexical_profile(collection)
                if col_profile is not None and col_profile.reads_bm25_v2:
                    await self.vector_store.invalidate_bm25_v2_receipt(
                        collection, reason="bm25_v2_cutover_reverted"
                    )
                    await self.vector_store.ensure_lexical_config(
                        collection,
                        col_profile.with_runtime_selection(
                            active_version=LEXICAL_V1,
                            shadow_write_enabled=True,
                            filtering=col_profile.filtering,
                        ),
                        dataset_id=dataset_id,
                        tenant_id=tenant_id,
                        allow_runtime_transition=True,
                    )
        except Exception as revert_exc:  # pragma: no cover - defensive
            logger.critical(
                "bm25_v2 revert failed for dataset %s after %s; the lifecycle "
                "row will fail closed until manually repaired: %s",
                dataset_id,
                in_progress_state,
                revert_exc,
            )
        try:
            await self.lifecycle.fail_transition(
                dataset_id=dataset_id,
                epoch=epoch,
                lock_token=token,
                in_progress_state=in_progress_state,
                recovered_state=recovered_state,
                error=str(error),
            )
        except LifecycleStateConflict as conflict_exc:  # pragma: no cover
            logger.critical(
                "bm25_v2 transition row for dataset %s could not be settled: %s",
                dataset_id,
                conflict_exc,
            )


def _pg_profile(snap: dict[str, Any]) -> LexicalConfig:
    try:
        return LexicalConfig.from_index_config(snap.get("index_config") or {})
    except LexicalConfigError as exc:
        raise Bm25V2LifecycleError(
            f"dataset lexical profile is invalid: {exc}",
            code="bm25_v2_invalid_profile",
            http_status=400,
        ) from exc


def _authority_dict(authority: AuthoritySnapshot) -> dict[str, Any]:
    return {
        "point_count": authority.point_count,
        "point_ids_sha256": authority.point_ids_sha256,
        "source_text_sha256": authority.source_text_sha256,
        "content_revision": authority.content_revision,
        "authority_kind": authority.authority_kind,
    }


__all__ = [
    "Bm25V2LifecycleError",
    "Bm25V2LifecycleService",
    "CrossAuthorityMismatch",
    "QuiescenceTimeout",
]
