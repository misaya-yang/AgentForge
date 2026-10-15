"""PRD T6 (unit tier): the BM25 v2 cutover/rollback protocol orchestration.

These fake both storage boundaries (PostgreSQL lifecycle store and Qdrant
vector store) so the *protocol* — exclusion ordering, two-phase confirmation,
cross-store verification, revert on failure, crash recovery, and idempotence —
is what fails when it breaks. Live-PostgreSQL semantics (barrier vs the real
write lease, real CAS contention) are covered by
tests/database/test_bm25_v2_lifecycle_tierb.py, and the real VectorStore
lifecycle surface (receipt publish/verify/scan) gets its own section below
against a recording Qdrant fake client.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.persistence.bm25_v2_lifecycle import (
    AuthoritySnapshot,
    LifecycleStateConflict,
    LifecycleTransitionBusy,
)
from knowledge_service.persistence.bm25_v2_lifecycle import (
    point_ids_sha256 as store_point_ids_sha256,
)
from knowledge_service.persistence.bm25_v2_lifecycle import (
    source_text_sha256 as store_source_text_sha256,
)
from knowledge_service.services.knowledge.bm25_v2_lifecycle import (
    Bm25V2LifecycleError,
    Bm25V2LifecycleService,
    CrossAuthorityMismatch,
    QuiescenceTimeout,
)
from knowledge_service.services.knowledge.lexical_config import (
    BM25_V2,
    BM25_V2_AUTHORITY_KIND,
    BM25_V2_BACKFILL_METADATA_KEY,
    BM25_V2_FIELD,
    BM25_V2_MODEL,
    COLLECTION_SCOPE_METADATA_KEY,
    LEXICAL_V1,
    LEXICAL_V1_FIELD,
    LexicalConfig,
)
from knowledge_service.services.knowledge.vector_store import (
    VectorStore,
    VectorStoreError,
)
from qdrant_client.http import models as qmodels

# --------------------------------------------------------------------- fakes


def _index_config(
    *,
    active: str = LEXICAL_V1,
    shadow: bool = True,
    k: float = 1.2,
) -> dict[str, Any]:
    return {
        "retrieval": {
            "lexical": {
                "active_version": active,
                "bm25_v2": {
                    "shadow_write_enabled": shadow,
                    "field": BM25_V2_FIELD,
                    "model": BM25_V2_MODEL,
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


def _digest_pair(entries: list[tuple[str, str]]) -> tuple[str, str]:
    return (
        store_point_ids_sha256([pid for pid, _ in entries]),
        store_source_text_sha256(entries),
    )


POINTS = [("seg-a", "alpha text"), ("seg-b", "beta text")]


class FakeLifecycleStore:
    """In-memory clone of the Bm25V2LifecycleStore contract."""

    def __init__(
        self,
        *,
        active: str = LEXICAL_V1,
        shadow: bool = True,
        content_revision: int = 7,
        log: list[str] | None = None,
    ) -> None:
        self.dataset = {
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "content_revision": content_revision,
            "index_config": _index_config(active=active, shadow=shadow),
            "is_deleted": False,
        }
        self.row: dict[str, Any] | None = None
        self.log: list[str] = log if log is not None else []
        self.busy_documents = 0
        self.dispatchable_documents = 0
        self.authority_points = list(POINTS)
        self.last_authority_connection: Any | None = None
        self.flip_fails = False
        self._held = False

    def _event(self, name: str) -> None:
        self.log.append(name)

    @contextlib.asynccontextmanager
    async def transition_barrier(self, _dataset_id: str, *, wait_s: float = 0.0):  # noqa: ARG002
        if self._held:
            raise LifecycleTransitionBusy("barrier held by another executor")
        self._held = True
        self._event("barrier:acquire")
        try:
            yield None
        finally:
            self._held = False
            self._event("barrier:release")

    async def get_state(self, _dataset_id: str) -> dict[str, Any] | None:
        return dict(self.row) if self.row else None

    async def ensure_row(self, *, dataset_id: str, tenant_id: str) -> None:
        if self.row is None:
            self.row = {
                "dataset_id": dataset_id,
                "tenant_id": tenant_id,
                "state": "shadow",
                "epoch": 0,
                "transition_kind": None,
                "lock_token": None,
                "authority_content_revision": None,
                "manifest_sha256": "",
                "pre_evidence": {},
                "post_evidence": {},
                "last_error": None,
            }

    async def begin_transition(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        kind: str,
        from_state: str,
        authority_content_revision: int | None = None,
    ) -> tuple[int, str]:
        await self.ensure_row(dataset_id=dataset_id, tenant_id=tenant_id)
        assert self.row is not None
        if self.row["state"] != from_state:
            raise LifecycleStateConflict(
                f"cannot start {kind} from {self.row['state']}"
            )
        token = f"token-{kind}-{self.row['epoch'] + 1}"
        self.row.update(
            {
                "state": f"{kind}_in_progress",
                "epoch": self.row["epoch"] + 1,
                "transition_kind": kind,
                "lock_token": token,
                "authority_content_revision": authority_content_revision,
                "pre_evidence": {},
                "post_evidence": {},
                "last_error": None,
            }
        )
        self._event(f"begin:{kind}")
        return int(self.row["epoch"]), token

    async def finish_transition(
        self,
        *,
        dataset_id: str,  # noqa: ARG002
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        target_state: str,
        pre_evidence: dict[str, Any] | None = None,
        post_evidence: dict[str, Any] | None = None,
        manifest_sha256: str = "",
        authority_content_revision: int | None = None,
    ) -> None:
        self._settle(
            epoch=epoch,
            lock_token=lock_token,
            in_progress_state=in_progress_state,
            target_state=target_state,
            pre_evidence=pre_evidence,
            post_evidence=post_evidence,
            manifest_sha256=manifest_sha256,
            authority_content_revision=authority_content_revision,
            last_error=None,
        )
        self._event(f"finish:{target_state}")

    async def fail_transition(
        self,
        *,
        dataset_id: str,  # noqa: ARG002
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        recovered_state: str,
        error: str,
        post_evidence: dict[str, Any] | None = None,
    ) -> None:
        self._settle(
            epoch=epoch,
            lock_token=lock_token,
            in_progress_state=in_progress_state,
            target_state=recovered_state,
            pre_evidence=None,
            post_evidence=post_evidence,
            manifest_sha256="",
            authority_content_revision=None,
            last_error=error,
        )
        self._event(f"fail:{recovered_state}")

    def _settle(
        self,
        *,
        epoch: int,
        lock_token: str,
        in_progress_state: str,
        target_state: str,
        pre_evidence: dict[str, Any] | None,
        post_evidence: dict[str, Any] | None,
        manifest_sha256: str,
        authority_content_revision: int | None,
        last_error: str | None,
    ) -> None:
        assert self.row is not None
        if (
            self.row["epoch"] != epoch
            or self.row["lock_token"] != lock_token
            or self.row["state"] != in_progress_state
        ):
            raise LifecycleStateConflict("stale transition CAS")
        self.row.update(
            {
                "state": target_state,
                "epoch": self.row["epoch"] + 1,
                "transition_kind": None,
                "lock_token": None,
                "pre_evidence": pre_evidence or {},
                "post_evidence": post_evidence or {},
                "manifest_sha256": manifest_sha256 or self.row["manifest_sha256"],
                "authority_content_revision": authority_content_revision,
                "last_error": last_error,
            }
        )

    async def reset_stale_transition(
        self,
        *,
        dataset_id: str,  # noqa: ARG002
        in_progress_state: str,
        recovered_state: str,
        error: str,
    ) -> bool:
        assert self.row is not None
        if self.row["state"] != in_progress_state:
            return False
        self.row.update(
            {
                "state": recovered_state,
                "epoch": self.row["epoch"] + 1,
                "transition_kind": None,
                "lock_token": None,
                "last_error": error,
            }
        )
        self._event(f"reset:{recovered_state}")
        return True

    async def reconcile_steady_state(
        self,
        *,
        dataset_id: str,  # noqa: ARG002
        from_state: str,
        target_state: str,
        error: str,
    ) -> bool:
        assert self.row is not None
        if self.row["state"] != from_state or self.row["transition_kind"] is not None:
            return False
        self.row.update(
            {
                "state": target_state,
                "epoch": self.row["epoch"] + 1,
                "last_error": error,
            }
        )
        self._event(f"reconcile:{target_state}")
        return True

    async def certify_active_publication(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        expected_epoch: int,
        authority_content_revision: int,
        manifest_sha256: str,
        post_evidence: dict[str, Any],
        connection: Any,
    ) -> int:
        _ = connection
        assert self.row is not None
        if (
            self.row["dataset_id"] != dataset_id
            or self.row["tenant_id"] != tenant_id
            or self.row["state"] != "active_v2"
            or self.row["epoch"] != expected_epoch
        ):
            raise LifecycleStateConflict("stale active publication CAS")
        self.row.update(
            {
                "epoch": expected_epoch + 1,
                "authority_content_revision": authority_content_revision,
                "manifest_sha256": manifest_sha256,
                "post_evidence": dict(post_evidence),
                "last_error": None,
            }
        )
        self._event("certify:active_publication")
        return int(self.row["epoch"])

    async def count_busy_documents(self, _dataset_id: str) -> int:
        return self.busy_documents

    async def count_dispatchable_documents(self, _dataset_id: str) -> int:
        return self.dispatchable_documents

    async def dataset_snapshot(self, _dataset_id: str) -> dict[str, Any]:
        return dict(self.dataset)

    async def authority_snapshot(
        self,
        *,
        collection_name: str,
        tenant_id: str,
        dataset_id: str,
        connection: Any | None = None,
    ) -> AuthoritySnapshot:
        self.last_authority_connection = connection
        ids_sha, source_sha = _digest_pair(self.authority_points)
        return AuthoritySnapshot(
            collection_name=collection_name,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            content_revision=int(self.dataset["content_revision"]),
            point_count=len(self.authority_points),
            point_ids_sha256=ids_sha,
            source_text_sha256=source_sha,
        )

    async def flip_dataset_lexical_active_version(
        self,
        *,
        dataset_id: str,  # noqa: ARG002
        tenant_id: str,
        expected_active_version: str,
        target_active_version: str,
        shadow_write_enabled: bool,
        expected_content_revision: int,
    ) -> dict[str, Any]:
        self._event(f"flip_pg:{target_active_version}")
        if self.flip_fails:
            raise LifecycleStateConflict("injected PG flip failure")
        _ = tenant_id
        lexical = self.dataset["index_config"]["retrieval"]["lexical"]
        current = lexical.get("active_version", LEXICAL_V1)
        if current != expected_active_version:
            raise LifecycleStateConflict(
                f"expected {expected_active_version}, was {current}"
            )
        if int(self.dataset["content_revision"]) != int(expected_content_revision):
            raise LifecycleStateConflict("revision moved")
        lexical["active_version"] = target_active_version
        lexical["bm25_v2"]["shadow_write_enabled"] = shadow_write_enabled
        return {
            "content_revision": int(self.dataset["content_revision"]),
            "lexical": dict(lexical),
        }


class FakeVectorStore:
    """In-memory clone of the VectorStore lifecycle surface used by T6."""

    def __init__(
        self,
        *,
        profile: LexicalConfig,
        points: list[tuple[str, str]],
        receipt: dict[str, Any] | None = None,
        bm25_v2_enabled: bool = True,
        log: list[str] | None = None,
    ) -> None:
        self.profile = profile
        self.points = points
        self.receipt = receipt
        self.bm25_v2_enabled = bm25_v2_enabled
        self.log: list[str] = log if log is not None else []
        self.verify_fails = False
        self.publish_fails = False
        self.corrupt_markers = False

    def _event(self, name: str) -> None:
        self.log.append(name)

    def scope(self) -> dict[str, Any]:
        ids, sources = _digest_pair(self.points)
        return {
            "point_count": len(self.points),
            "complete_count": 0 if self.corrupt_markers else len(self.points),
            "point_ids_sha256": ids,
            "source_text_sha256": sources,
        }

    async def get_live_lexical_profile(self, _collection_name: str):
        return self.profile, dict(self.receipt) if self.receipt else None

    async def scan_bm25_v2_lexical_scope(
        self, _collection_name: str, *, tenant_id: str, dataset_id: str, config
    ) -> dict[str, Any]:
        _ = (tenant_id, dataset_id, config)
        self._event("scan")
        return self.scope()

    async def invalidate_bm25_v2_receipt(self, _collection_name: str, *, reason: str) -> None:
        self._event(f"invalidate:{reason}")
        if self.receipt and self.receipt.get("status") == "complete":
            self.receipt = {"status": "invalidated", "reason": reason}

    async def ensure_lexical_config(
        self,
        _collection_name: str,
        requested: LexicalConfig,
        *,
        dataset_id: str,
        tenant_id: str,
        allow_runtime_transition: bool = False,
        authority_content_revision: int | None = None,
        active_cutover_authorized: bool = False,
    ) -> bool:
        _ = (dataset_id, tenant_id, authority_content_revision)
        if requested.bm25_v2.fingerprint != self.profile.bm25_v2.fingerprint:
            raise VectorStoreError("frozen fingerprint disagreement")
        if requested.reads_bm25_v2:
            assert allow_runtime_transition, "active flip must be a transition"
            assert active_cutover_authorized, "active flip needs the protocol proof"
            if not self.bm25_v2_enabled:
                raise VectorStoreError("disabled by the service kill switch")
        self._event("ensure:v2" if requested.reads_bm25_v2 else "ensure:v1")
        self.profile = requested
        return True

    async def publish_bm25_v2_cutover_receipt(
        self,
        _collection_name: str,
        *,
        receipt: dict[str, Any],
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any]:
        self._event("publish")
        if self.publish_fails:
            raise VectorStoreError("injected publish failure")
        assert receipt.get("status") == "complete"
        assert receipt.get("tenant_id") == tenant_id
        assert receipt.get("dataset_id") == dataset_id
        self.receipt = dict(receipt)
        return dict(receipt)

    async def verify_bm25_v2_active_readiness(
        self, _collection_name: str, *, tenant_id: str, dataset_id: str, config=None
    ) -> dict[str, Any]:
        _ = (tenant_id, dataset_id)
        self._event("verify")
        if self.verify_fails:
            raise VectorStoreError("injected verify failure")
        if not self.profile.reads_bm25_v2:
            raise VectorStoreError("collection is not cut over to bm25_v2")
        if (
            config is not None
            and config.bm25_v2.fingerprint != self.profile.bm25_v2.fingerprint
        ):
            raise VectorStoreError("fingerprint disagreement")
        scope = self.scope()
        if (
            not self.receipt
            or self.receipt.get("status") != "complete"
            or self.receipt.get("point_ids_sha256") != scope["point_ids_sha256"]
            or self.receipt.get("source_text_sha256") != scope["source_text_sha256"]
            or scope["complete_count"] != scope["point_count"]
        ):
            raise VectorStoreError("no completed bm25_v2 receipt; refusing active reads")
        return {**scope, "status": "complete", "certified_by": "active_readiness_recompute"}

    async def require_collection_readable(
        self,
        _collection_name: str,
        *,
        tenant_id=None,
        dataset_id=None,
        expected_active_v2=False,
    ) -> dict[str, str]:
        _ = (tenant_id, dataset_id)
        if expected_active_v2 and not self.profile.reads_bm25_v2:
            raise VectorStoreError("collection is not cut over to bm25_v2")
        return {"tenant_id": "tenant-a", "dataset_id": "dataset-a"}


def _complete_receipt(points: list[tuple[str, str]], config: LexicalConfig) -> dict[str, Any]:
    ids_sha, source_sha = _digest_pair(points)
    return {
        "status": "complete",
        "collection_name": "collection-a",
        "tenant_id": "tenant-a",
        "dataset_id": "dataset-a",
        "point_count": len(points),
        "point_ids_sha256": ids_sha,
        "source_text_sha256": source_sha,
        "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
        "filtering_profile_fingerprint": config.filtering.fingerprint,
        "authority_kind": BM25_V2_AUTHORITY_KIND,
        "authority_content_revision": 7,
    }


def _world(
    *,
    vs_profile: LexicalConfig | None = None,
    ls_active: str = LEXICAL_V1,
    receipt: dict[str, Any] | None = None,
    bm25_v2_enabled: bool = True,
    quiesce_timeout_s: float = 0.05,
) -> tuple[Bm25V2LifecycleService, FakeVectorStore, FakeLifecycleStore]:
    log: list[str] = []
    lifecycle = FakeLifecycleStore(active=ls_active, log=log)
    store = FakeVectorStore(
        profile=vs_profile
        or LexicalConfig.from_index_config(
            _index_config(active=BM25_V2 if ls_active == BM25_V2 else LEXICAL_V1, shadow=True)
        ),
        points=list(POINTS),
        receipt=receipt,
        bm25_v2_enabled=bm25_v2_enabled,
        log=log,
    )
    service = Bm25V2LifecycleService(
        vector_store=store,
        lifecycle_store=lifecycle,
        quiesce_timeout_s=quiesce_timeout_s,
        quiesce_interval_s=0.001,
    )
    return service, store, lifecycle


@pytest.mark.asyncio
async def test_active_runtime_publication_recertifies_receipt_and_epoch() -> None:
    service, store, lifecycle = _world(
        ls_active=BM25_V2,
        vs_profile=LexicalConfig.from_index_config(
            _index_config(active=BM25_V2, shadow=True)
        ),
    )
    await lifecycle.ensure_row(dataset_id="dataset-a", tenant_id="tenant-a")
    assert lifecycle.row is not None
    lifecycle.row.update({"state": "active_v2", "epoch": 9})
    # Statement-level content triggers advanced the reserved negative marker
    # after the lease began; certification must use the latest negative value.
    lifecycle.dataset["content_revision"] = -1005

    context = await service.active_publication_context("dataset-a")
    assert context is not None
    publication_connection = object()
    certification = await service.recertify_active_publication(
        context,
        publication_revision=-1007,
        connection=publication_connection,
    )
    assert lifecycle.last_authority_connection is publication_connection
    assert certification["target_revision"] == 1006
    assert store.receipt is not None
    assert store.receipt["authority_content_revision"] == 1006
    assert store.log[-3:] == ["scan", "publish", "verify"]

    epoch = await service.settle_active_publication(
        context,
        certification,
        connection=object(),
    )
    assert epoch == 10
    assert lifecycle.row["authority_content_revision"] == 1006
    assert lifecycle.row["manifest_sha256"] == certification["manifest_sha256"]


@pytest.mark.asyncio
async def test_active_publication_kill_switch_fails_before_receipt_mutation() -> None:
    service, store, lifecycle = _world(
        ls_active=BM25_V2,
        vs_profile=LexicalConfig.from_index_config(
            _index_config(active=BM25_V2, shadow=True)
        ),
        receipt=_complete_receipt(
            POINTS,
            LexicalConfig.from_index_config(_index_config(active=BM25_V2, shadow=True)),
        ),
        bm25_v2_enabled=False,
    )
    await lifecycle.ensure_row(dataset_id="dataset-a", tenant_id="tenant-a")
    assert lifecycle.row is not None
    lifecycle.row["state"] = "active_v2"
    original_receipt = dict(store.receipt or {})

    with pytest.raises(Bm25V2LifecycleError) as error:
        await service.active_publication_context("dataset-a")
    assert error.value.http_status == 503
    assert store.receipt == original_receipt
    assert store.log == []


def _index_of(log: list[str], event: str) -> int:
    assert event in log, f"{event} missing from {log}"
    return log.index(event)


# ------------------------------------------------------------------ cutover


@pytest.mark.asyncio
async def test_cutover_two_phase_order_and_certification() -> None:
    service, store, lifecycle = _world()
    result = await service.cutover("dataset-a")

    assert result["applied"] is True
    assert result["state"] == "active_v2"
    log = store.log
    # Live verification precedes the sentinel; the sentinel precedes the
    # Qdrant flip; certification precedes the PostgreSQL flip; the
    # active-readiness gate recomputes only after PostgreSQL moved.
    assert _index_of(log, "scan") < _index_of(log, "invalidate:bm25_v2_cutover_phase1")
    assert _index_of(log, "invalidate:bm25_v2_cutover_phase1") < _index_of(log, "ensure:v2")
    assert _index_of(log, "ensure:v2") < _index_of(log, "publish")
    assert _index_of(log, "publish") < _index_of(log, "flip_pg:bm25_v2")
    assert _index_of(log, "flip_pg:bm25_v2") < _index_of(log, "verify")
    assert _index_of(log, "verify") < _index_of(log, "finish:active_v2")
    assert store.profile.reads_bm25_v2 is True
    assert lifecycle.row["state"] == "active_v2"
    assert lifecycle.row["lock_token"] is None
    assert store.receipt["status"] == "complete"
    ids_sha, source_sha = _digest_pair(POINTS)
    assert store.receipt["point_ids_sha256"] == ids_sha
    assert store.receipt["source_text_sha256"] == source_sha
    assert store.receipt["authority_content_revision"] == 7
    assert result["manifest_sha256"] == ids_sha


@pytest.mark.asyncio
async def test_cutover_requires_enabled_shadow_profile() -> None:
    service, store, lifecycle = _world()
    lifecycle.dataset["index_config"] = _index_config(shadow=False)
    with pytest.raises(Bm25V2LifecycleError, match="shadow profile") as exc:
        await service.cutover("dataset-a")
    assert exc.value.code == "bm25_v2_shadow_not_enabled"
    assert store.log == []


@pytest.mark.asyncio
async def test_cutover_rejects_collection_without_shadow_profile() -> None:
    service, store, _lifecycle = _world(
        vs_profile=LexicalConfig.from_index_config(_index_config(shadow=False))
    )
    with pytest.raises(Bm25V2LifecycleError, match="shadow profile") as exc:
        await service.cutover("dataset-a")
    assert exc.value.code == "bm25_v2_shadow_not_enabled"
    assert store.log == []


@pytest.mark.asyncio
async def test_cutover_rejects_fingerprint_divergence() -> None:
    service, _store, lifecycle = _world(
        vs_profile=LexicalConfig.from_index_config(_index_config(k=1.5))
    )
    with pytest.raises(Bm25V2LifecycleError, match="frozen fingerprints") as exc:
        await service.cutover("dataset-a")
    assert exc.value.code == "bm25_v2_fingerprint_mismatch"
    assert "barrier:acquire" not in lifecycle.log


@pytest.mark.asyncio
async def test_cutover_dry_run_makes_no_mutation() -> None:
    service, store, lifecycle = _world()
    result = await service.cutover("dataset-a", apply=False)
    assert result["dry_run"] is True
    assert result["from_state"] == "shadow"
    assert result["verification"]["agreement"] is True
    # Dry-run performs the real full-scroll verification, but no barrier,
    # receipt publication, or authority flip.
    assert store.log == ["scan"]
    assert lifecycle.row is None or lifecycle.row["state"] == "shadow"
    assert lifecycle.dataset["index_config"]["retrieval"]["lexical"]["active_version"] == LEXICAL_V1


@pytest.mark.asyncio
async def test_cutover_idempotent_after_success() -> None:
    service, store, _lifecycle = _world()
    await service.cutover("dataset-a")
    events_before = len(store.log)
    result = await service.cutover("dataset-a")
    assert result["already_active"] is True
    assert len(store.log) == events_before


@pytest.mark.asyncio
async def test_cutover_quiescence_timeout_aborts_without_flips() -> None:
    service, store, lifecycle = _world()
    lifecycle.busy_documents = 2
    with pytest.raises(QuiescenceTimeout):
        await service.cutover("dataset-a")
    assert "ensure:v2" not in store.log
    assert "scan" not in store.log
    assert "flip_pg:bm25_v2" not in store.log
    assert lifecycle.row["state"] == "shadow"
    assert "in flight" in (lifecycle.row["last_error"] or "")
    assert store.receipt is None  # never even reached the sentinel step
    assert "barrier:release" in store.log


@pytest.mark.asyncio
async def test_cutover_cross_authority_mismatch_aborts_before_any_flip() -> None:
    service, store, lifecycle = _world(
        receipt=_complete_receipt(POINTS, LexicalConfig.from_index_config(_index_config()))
    )
    lifecycle.authority_points = [("seg-a", "alpha text")]  # PG sees one, Qdrant two
    with pytest.raises(CrossAuthorityMismatch):
        await service.cutover("dataset-a")
    # Stale proof retired only after agreement passes: the scan happened,
    # nothing past it did.
    assert "scan" in store.log
    assert "invalidate:bm25_v2_cutover_phase1" not in store.log
    assert "flip_pg:bm25_v2" not in store.log
    assert lifecycle.row["state"] == "shadow"
    # The pre-existing complete receipt survived untouched (no flip happened).
    assert store.receipt["status"] == "complete"


@pytest.mark.asyncio
async def test_cutover_incomplete_markers_abort() -> None:
    service, store, lifecycle = _world()
    store.corrupt_markers = True
    with pytest.raises(CrossAuthorityMismatch, match="marker"):
        await service.cutover("dataset-a")
    assert lifecycle.row["state"] == "shadow"
    assert "ensure:v2" not in store.log


@pytest.mark.asyncio
async def test_cutover_expected_manifest_mismatch_refuses() -> None:
    service, store, _lifecycle = _world()
    with pytest.raises(Bm25V2LifecycleError, match="manifest") as exc:
        await service.cutover("dataset-a", expected_manifest_sha256="0" * 64)
    assert exc.value.code == "bm25_v2_manifest_mismatch"
    assert exc.value.http_status == 400
    assert "invalidate:bm25_v2_cutover_phase1" not in store.log


@pytest.mark.asyncio
async def test_cutover_reverts_qdrant_when_pg_flip_fails_after_qdrant_flip() -> None:
    service, store, lifecycle = _world()
    lifecycle.flip_fails = True
    with pytest.raises(Bm25V2LifecycleError, match="cutover failed"):
        await service.cutover("dataset-a")
    # Qdrant was flipped then reverted to v1; the cutover proof was
    # re-invalidated; the PostgreSQL selection never moved.
    assert "ensure:v2" in store.log
    assert "ensure:v1" in store.log
    assert _index_of(store.log, "flip_pg:bm25_v2") < _index_of(store.log, "ensure:v1")
    assert store.profile.reads_bm25_v2 is False
    assert "invalidate:bm25_v2_cutover_reverted" in store.log
    assert "flip_pg:lexical_v1" not in store.log  # revert CAS never fired
    assert lifecycle.row["state"] == "shadow"


@pytest.mark.asyncio
async def test_cutover_reverts_both_sides_when_post_verify_fails() -> None:
    service, store, lifecycle = _world()
    store.verify_fails = True
    with pytest.raises(Bm25V2LifecycleError, match="cutover failed"):
        await service.cutover("dataset-a")
    # PG flipped, so the revert must flip it back; Qdrant metadata back to v1.
    assert "flip_pg:bm25_v2" in store.log
    assert "flip_pg:lexical_v1" in store.log
    assert "ensure:v1" in store.log
    assert store.profile.reads_bm25_v2 is False
    assert (
        lifecycle.dataset["index_config"]["retrieval"]["lexical"]["active_version"]
        == LEXICAL_V1
    )
    assert lifecycle.row["state"] == "shadow"


@pytest.mark.asyncio
async def test_cutover_respects_kill_switch_before_any_flip() -> None:
    service, store, lifecycle = _world(bm25_v2_enabled=False)
    with pytest.raises(Bm25V2LifecycleError) as error:
        await service.cutover("dataset-a")
    assert error.value.http_status == 503
    assert store.log == []
    assert lifecycle.row is None


# ----------------------------------------------------------------- rollback


@pytest.mark.asyncio
async def test_rollback_flips_pg_first_then_qdrant_and_retains_v2() -> None:
    service, store, lifecycle = _world(ls_active=BM25_V2)
    # Enter through a real cutover first.
    await service.cutover("dataset-a")
    store.log.clear()

    result = await service.rollback("dataset-a")
    log = store.log
    assert _index_of(log, "invalidate:bm25_v2_rollback_phase1") < _index_of(
        log, "flip_pg:lexical_v1"
    )
    assert _index_of(log, "flip_pg:lexical_v1") < _index_of(log, "ensure:v1")
    assert _index_of(log, "ensure:v1") < _index_of(log, "finish:shadow")
    assert result["state"] == "shadow"
    assert result["v2_data_retained"] is True
    assert store.profile.reads_bm25_v2 is False
    assert store.profile.writes_bm25_v2 is True  # keep_shadow_writes default
    assert lifecycle.dataset["index_config"]["retrieval"]["lexical"]["bm25_v2"][
        "shadow_write_enabled"
    ] is True
    assert lifecycle.row["state"] == "shadow"


@pytest.mark.asyncio
async def test_rollback_noop_when_never_cut_over() -> None:
    service, store, _lifecycle = _world()
    result = await service.rollback("dataset-a")
    assert result["already_inactive"] is True
    assert store.log == []


@pytest.mark.asyncio
async def test_rollback_refuses_blindly_when_split_profile() -> None:
    service, store, lifecycle = _world(ls_active=BM25_V2)
    await service.cutover("dataset-a")
    # Simulate PostgreSQL having been reverted out-of-band while the
    # collection still says v2: rollback must refuse a blind pass.
    lifecycle.dataset["index_config"] = _index_config(active=LEXICAL_V1, shadow=True)
    lifecycle.row = None  # pretend steady state
    with pytest.raises(Bm25V2LifecycleError) as exc:
        await service.rollback("dataset-a")
    assert exc.value.code == "bm25_v2_split_profile"
    assert store.log == []


@pytest.mark.asyncio
async def test_rollback_under_kill_switch_forces_shadow_writes_off() -> None:
    service, store, lifecycle = _world(ls_active=BM25_V2)
    await service.cutover("dataset-a")
    store.bm25_v2_enabled = False
    store.log.clear()

    await service.rollback("dataset-a", keep_shadow_writes=True)
    assert store.profile.writes_bm25_v2 is False
    assert lifecycle.dataset["index_config"]["retrieval"]["lexical"]["bm25_v2"][
        "shadow_write_enabled"
    ] is False


@pytest.mark.asyncio
async def test_rollback_then_recutover_round_trip() -> None:
    service, store, _lifecycle = _world(ls_active=BM25_V2)
    await service.cutover("dataset-a")
    await service.rollback("dataset-a")
    # Re-cutover re-derives everything live; the invalidated receipt left by
    # rollback must not block it.
    result = await service.cutover("dataset-a")
    assert result["state"] == "active_v2"
    assert store.receipt["status"] == "complete"
    # v1 data was never destroyed across the whole round trip.
    assert store.points == POINTS


# ------------------------------------------------------------ crash recovery


@pytest.mark.asyncio
async def test_stale_in_progress_row_is_reset_by_barrier_winner() -> None:
    service, store, lifecycle = _world()
    # A dead executor left a cutover_in_progress row (its barrier is gone).
    epoch, token = await lifecycle.begin_transition(
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        kind="cutover",
        from_state="shadow",
        authority_content_revision=7,
    )
    store.log.clear()

    result = await service.cutover("dataset-a")
    assert result["state"] == "active_v2"
    assert "reset:shadow" in store.log
    # The dead executor's late writes can no longer settle the row.
    with pytest.raises(LifecycleStateConflict):
        await lifecycle.finish_transition(
            dataset_id="dataset-a",
            epoch=epoch,
            lock_token=token,
            in_progress_state="cutover_in_progress",
            target_state="active_v2",
        )


@pytest.mark.asyncio
async def test_stale_active_row_recovered_for_rollback() -> None:
    service, store, lifecycle = _world(ls_active=BM25_V2)
    lifecycle.row = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "state": "rollback_in_progress",
        "epoch": 3,
        "transition_kind": "rollback",
        "lock_token": "dead-token",
        "authority_content_revision": 7,
        "manifest_sha256": "",
        "pre_evidence": {},
        "post_evidence": {},
        "last_error": None,
    }
    result = await service.rollback("dataset-a")
    assert result["state"] == "shadow"
    assert "reset:active_v2" in store.log


@pytest.mark.asyncio
async def test_concurrent_cutover_refused_by_barrier() -> None:
    log: list[str] = []
    lifecycle = FakeLifecycleStore(log=log)
    store = FakeVectorStore(
        profile=LexicalConfig.from_index_config(_index_config()),
        points=list(POINTS),
        log=log,
    )
    service = Bm25V2LifecycleService(
        vector_store=store, lifecycle_store=lifecycle, quiesce_timeout_s=30
    )

    original_count = lifecycle.count_busy_documents

    async def slow_quiescence(dataset_id: str) -> int:
        value = await original_count(dataset_id)
        await asyncio.sleep(0.05)
        return value

    lifecycle.count_busy_documents = slow_quiescence  # type: ignore[method-assign]

    first = asyncio.create_task(service.cutover("dataset-a"))
    for _ in range(200):
        if "barrier:acquire" in log:
            break
        await asyncio.sleep(0.005)
    assert "barrier:acquire" in log
    with pytest.raises(LifecycleTransitionBusy):
        await service.cutover("dataset-a")
    result = await first
    assert result["state"] == "active_v2"
    assert log.count("flip_pg:bm25_v2") == 1


# ----------------------------------------------------------------- read side


@pytest.mark.asyncio
async def test_get_lifecycle_state_shape() -> None:
    service, _store, _lifecycle = _world()
    state = await service.get_lifecycle_state("dataset-a")
    assert state["state"] == "shadow"
    assert state["postgres_profile"]["active_version"] == LEXICAL_V1
    assert state["receipt_status"] is None
    assert state["busy_documents"] == 0


@pytest.mark.asyncio
async def test_verify_cross_authority_agreement_and_drift() -> None:
    service, _store, lifecycle = _world()
    report = await service.verify_cross_authority("dataset-a")
    assert report["agreement"] is True
    lifecycle.authority_points = [("seg-a", "changed text")]
    report = await service.verify_cross_authority("dataset-a")
    assert report["agreement"] is False


@pytest.mark.asyncio
async def test_missing_dataset_is_404() -> None:
    service, _store, lifecycle = _world()

    async def no_dataset(_dataset_id: str) -> None:
        return None

    lifecycle.dataset_snapshot = no_dataset  # type: ignore[method-assign]
    with pytest.raises(Bm25V2LifecycleError) as exc:
        await service.cutover("dataset-a")
    assert exc.value.http_status == 404


# ------------------------------------------------- digest contract parity


def test_digest_helpers_match_frozen_backfill_contract() -> None:
    # Load the backfill script by explicit path: a bare ``import scripts`` is
    # ambiguous in this repo — legacy tests insert ``tests/`` into sys.path,
    # where ``tests/scripts/`` (a real package) shadows the repo-root
    # ``scripts`` namespace package and breaks this import mid-sweep.
    import importlib.util
    import sys
    from pathlib import Path

    script_path = Path(__file__).resolve().parents[3] / "scripts" / "backfill_bm25_v2.py"
    spec = importlib.util.spec_from_file_location("backfill_bm25_v2", script_path)
    assert spec is not None and spec.loader is not None
    backfill = importlib.util.module_from_spec(spec)
    # Register before exec: @dataclass processing resolves the defining
    # module through sys.modules during class creation.
    sys.modules[spec.name] = backfill
    spec.loader.exec_module(backfill)

    entries = [("seg-b", "beta text"), ("seg-a", "alpha text")]
    assert store_point_ids_sha256([p for p, _ in entries]) == backfill.point_ids_sha256(
        [p for p, _ in entries]
    )
    assert store_source_text_sha256(entries) == backfill.source_text_sha256(entries)


# ------------------------------------------- real VectorStore T6 surface


def _record(config: LexicalConfig, point_id: str, text: str) -> SimpleNamespace:
    return SimpleNamespace(
        id=point_id,
        vector={BM25_V2_FIELD: qmodels.SparseVector(indices=[1], values=[1.0])},
        payload={
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
            "text": text,
            "_lexical": {
                "versions": [LEXICAL_V1, BM25_V2_FIELD],
                "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
                "filtering_profile_fingerprint": config.filtering.fingerprint,
                "source_text_sha256": hashlib.sha256(text.encode()).hexdigest(),
            },
        },
    )


def _collection_info(
    config: LexicalConfig,
    *,
    records: list[SimpleNamespace],
    with_receipt: dict[str, Any] | None,
) -> SimpleNamespace:
    sparse = {
        LEXICAL_V1_FIELD: qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF),
        BM25_V2_FIELD: qmodels.SparseVectorParams(modifier=qmodels.Modifier.IDF),
    }
    metadata: dict[str, Any] = {
        **config.to_collection_metadata(),
        COLLECTION_SCOPE_METADATA_KEY: {
            "schema_version": 1,
            "dataset_id": "dataset-a",
            "tenant_id": "tenant-a",
        },
    }
    if with_receipt is not None:
        metadata[BM25_V2_BACKFILL_METADATA_KEY] = with_receipt
    payload_schema = {
        field_name: SimpleNamespace(
            data_type=qmodels.PayloadSchemaType.KEYWORD,
            params=SimpleNamespace(is_tenant=field_name == "tenant_id"),
        )
        for field_name in ("tenant_id", "dataset_id")
    }
    return SimpleNamespace(
        config=SimpleNamespace(
            params=SimpleNamespace(
                vectors=SimpleNamespace(size=2),
                sparse_vectors=sparse,
            ),
            metadata=metadata,
            strict_mode_config=None,
        ),
        payload_schema=payload_schema,
        points_count=len(records),
        _records=records,
    )


def _store_with_records(
    monkeypatch: pytest.MonkeyPatch,
    config: LexicalConfig,
    records: list[SimpleNamespace],
    receipt: dict[str, Any] | None,
) -> tuple[VectorStore, SimpleNamespace]:
    info = _collection_info(config, records=records, with_receipt=receipt)

    class Client:
        async def get_collection(self, _collection_name: str) -> Any:
            return info

        async def update_collection(self, **kwargs: Any) -> bool:
            if kwargs.get("metadata") is not None:
                info.config.metadata.update(kwargs["metadata"])
            return True

        async def scroll(self, **_kwargs: Any) -> tuple[list[Any], Any]:
            return list(info._records), None

        async def close(self) -> None:
            return None

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.vector_store.AsyncQdrantClient",
        lambda **_kwargs: Client(),
    )
    store = VectorStore(url="http://qdrant", max_retries=1)
    store._bm25_v2_capability_receipts[store._capability_receipt_key(config)] = float("inf")
    return store, info


def _receipt_for(
    config: LexicalConfig,
    records: list[SimpleNamespace],
    *,
    text_override: dict[str, str] | None = None,
) -> dict[str, Any]:
    ids = [str(record.id) for record in records]
    entries = [
        (str(r.id), (text_override or {}).get(str(r.id), str(r.payload["text"])))
        for r in records
    ]
    return {
        "schema_version": 1,
        "status": "complete",
        "collection_name": "collection-a",
        "tenant_id": "tenant-a",
        "dataset_id": "dataset-a",
        "bm25_v2_schema_fingerprint": config.bm25_v2.fingerprint,
        "filtering_profile_fingerprint": config.filtering.fingerprint,
        "point_count": len(records),
        "point_ids_sha256": store_point_ids_sha256(ids),
        "source_text_sha256": store_source_text_sha256(entries),
        "authority_kind": BM25_V2_AUTHORITY_KIND,
        "authority_content_revision": 7,
    }


ACTIVE = LexicalConfig.from_index_config(_index_config(active=BM25_V2_FIELD))


@pytest.mark.asyncio
async def test_real_store_verify_active_readiness_passes_on_matching_receipt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [
        _record(ACTIVE, "seg-a", "alpha text"),
        _record(ACTIVE, "seg-b", "beta text"),
    ]
    receipt = _receipt_for(ACTIVE, records)
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    summary = await store.verify_bm25_v2_active_readiness(
        "collection-a", tenant_id="tenant-a", dataset_id="dataset-a", config=ACTIVE
    )
    assert summary["point_count"] == 2
    assert summary["status"] == "complete"
    assert summary["certified_by"] == "active_readiness_recompute"


@pytest.mark.asyncio
async def test_real_store_readiness_cache_is_receipt_keyed_and_singleflight(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    receipt = _receipt_for(ACTIVE, records)
    store, info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    store.bm25_v2_readiness_ttl_seconds = 60.0
    scans = 0
    original_scan = store._scan_bm25_v2_lexical_points

    async def counted_scan(*args: Any, **kwargs: Any):
        nonlocal scans
        scans += 1
        await asyncio.sleep(0)
        return await original_scan(*args, **kwargs)

    monkeypatch.setattr(store, "_scan_bm25_v2_lexical_points", counted_scan)
    checks = [
        store.verify_bm25_v2_active_readiness(
            "collection-a",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            config=ACTIVE,
        )
        for _ in range(8)
    ]
    results = await asyncio.gather(*checks)
    assert scans == 1
    assert {result["point_ids_sha256"] for result in results} == {
        receipt["point_ids_sha256"]
    }

    # A new authority revision is a different proof even when corpus digests
    # are unchanged, so it must miss the old positive cache entry.
    info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY] = {
        **receipt,
        "authority_content_revision": 8,
    }
    await store.verify_bm25_v2_active_readiness(
        "collection-a",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        config=ACTIVE,
    )
    assert scans == 2

    await store.invalidate_bm25_v2_receipt(
        "collection-a",
        reason="runtime_upsert",
    )
    with pytest.raises(VectorStoreError, match="no completed bm25_v2 receipt"):
        await store.verify_bm25_v2_active_readiness(
            "collection-a",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            config=ACTIVE,
        )


@pytest.mark.asyncio
async def test_real_store_readiness_ttl_zero_always_full_scrolls(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    receipt = _receipt_for(ACTIVE, records)
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    scans = 0
    original_scan = store._scan_bm25_v2_lexical_points

    async def counted_scan(*args: Any, **kwargs: Any):
        nonlocal scans
        scans += 1
        return await original_scan(*args, **kwargs)

    monkeypatch.setattr(store, "_scan_bm25_v2_lexical_points", counted_scan)
    for _ in range(2):
        await store.verify_bm25_v2_active_readiness(
            "collection-a",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            config=ACTIVE,
        )
    assert scans == 2


@pytest.mark.asyncio
async def test_real_store_readiness_singleflight_survives_first_waiter_cancellation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    receipt = _receipt_for(ACTIVE, records)
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    store.bm25_v2_readiness_ttl_seconds = 60.0
    started = asyncio.Event()
    release = asyncio.Event()
    scans = 0
    original_scan = store._scan_bm25_v2_lexical_points

    async def blocked_scan(*args: Any, **kwargs: Any):
        nonlocal scans
        scans += 1
        started.set()
        await release.wait()
        return await original_scan(*args, **kwargs)

    monkeypatch.setattr(store, "_scan_bm25_v2_lexical_points", blocked_scan)
    first = asyncio.create_task(
        store.verify_bm25_v2_active_readiness(
            "collection-a",
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            config=ACTIVE,
        )
    )
    await started.wait()
    first.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first
    release.set()
    await asyncio.sleep(0)
    await asyncio.sleep(0)

    result = await store.verify_bm25_v2_active_readiness(
        "collection-a",
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        config=ACTIVE,
    )
    assert result["status"] == "complete"
    assert scans == 1


@pytest.mark.asyncio
async def test_real_store_verify_active_readiness_detects_text_drift(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "changed after receipt")]
    receipt = _receipt_for(ACTIVE, records, text_override={"seg-a": "old text"})
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    with pytest.raises(VectorStoreError, match="source-text digest drifted"):
        await store.verify_bm25_v2_active_readiness(
            "collection-a", tenant_id="tenant-a", dataset_id="dataset-a", config=ACTIVE
        )


@pytest.mark.asyncio
async def test_real_store_verify_active_readiness_requires_receipt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, None)
    with pytest.raises(VectorStoreError, match="no completed bm25_v2 receipt"):
        await store.verify_bm25_v2_active_readiness(
            "collection-a", tenant_id="tenant-a", dataset_id="dataset-a", config=ACTIVE
        )


@pytest.mark.asyncio
async def test_real_store_verify_refuses_uncut_collection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shadow = LexicalConfig.from_index_config(_index_config())
    records = [_record(shadow, "seg-a", "alpha text")]
    store, _info = _store_with_records(monkeypatch, shadow, records, None)
    with pytest.raises(VectorStoreError, match="not cut over"):
        await store.verify_bm25_v2_active_readiness(
            "collection-a", tenant_id="tenant-a", dataset_id="dataset-a", config=shadow
        )


@pytest.mark.asyncio
async def test_real_store_publish_cutover_receipt_converges_and_validates(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    receipt = _receipt_for(ACTIVE, records)
    store, info = _store_with_records(monkeypatch, ACTIVE, records, None)
    published = await store.publish_bm25_v2_cutover_receipt(
        "collection-a", receipt=receipt, tenant_id="tenant-a", dataset_id="dataset-a"
    )
    assert published["status"] == "complete"
    assert info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY] == receipt
    with pytest.raises(VectorStoreError, match="incomplete"):
        await store.publish_bm25_v2_cutover_receipt(
            "collection-a",
            receipt={"status": "running"},
            tenant_id="tenant-a",
            dataset_id="dataset-a",
        )
    with pytest.raises(VectorStoreError, match="outside the"):
        await store.publish_bm25_v2_cutover_receipt(
            "collection-a",
            receipt=receipt,
            tenant_id="tenant-b",
            dataset_id="dataset-a",
        )


@pytest.mark.asyncio
async def test_real_store_invalidate_swaps_complete_receipt_for_sentinel(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [_record(ACTIVE, "seg-a", "alpha text")]
    receipt = _receipt_for(ACTIVE, records)
    store, info = _store_with_records(monkeypatch, ACTIVE, records, receipt)
    await store.invalidate_bm25_v2_receipt("collection-a", reason="cutover_phase1")
    assert info.config.metadata[BM25_V2_BACKFILL_METADATA_KEY] == {
        "schema_version": 1,
        "status": "invalidated",
        "reason": "cutover_phase1",
    }


@pytest.mark.asyncio
async def test_real_store_scan_scope_digest_matches_frozen_contract(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    records = [
        _record(ACTIVE, "seg-a", "alpha text"),
        _record(ACTIVE, "seg-b", "beta text"),
    ]
    store, _info = _store_with_records(monkeypatch, ACTIVE, records, None)
    scope = await store.scan_bm25_v2_lexical_scope(
        "collection-a", tenant_id="tenant-a", dataset_id="dataset-a", config=ACTIVE
    )
    assert scope["point_count"] == 2
    assert scope["complete_count"] == 2
    assert scope["point_ids_sha256"] == store_point_ids_sha256(["seg-a", "seg-b"])
    assert scope["source_text_sha256"] == store_source_text_sha256(
        [("seg-a", "alpha text"), ("seg-b", "beta text")]
    )


@pytest.mark.asyncio
async def test_real_store_authorized_ensure_lexical_config_flips_metadata(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    shadow = LexicalConfig.from_index_config(_index_config())
    records = [_record(shadow, "seg-a", "alpha text")]
    store, info = _store_with_records(monkeypatch, shadow, records, None)

    async def canary_query(**_kwargs: Any) -> Any:
        return SimpleNamespace(points=[])

    store._client.query_points = canary_query  # type: ignore[method-assign]
    active = shadow.with_runtime_selection(
        active_version=BM25_V2, shadow_write_enabled=True, filtering=shadow.filtering
    )
    store._bm25_v2_capability_receipts[store._capability_receipt_key(active)] = float("inf")
    changed = await store.ensure_lexical_config(
        "collection-a",
        active,
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        allow_runtime_transition=True,
        active_cutover_authorized=True,
    )
    assert changed is True
    persisted = LexicalConfig.from_collection_metadata(info.config.metadata)
    assert persisted is not None
    assert persisted.active_version == BM25_V2
    assert persisted.reads_bm25_v2 is True
    # The receipt key is untouched by the flip itself.
    assert BM25_V2_BACKFILL_METADATA_KEY not in info.config.metadata
