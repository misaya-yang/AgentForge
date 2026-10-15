"""Offline tests for the T3 blue-green migration operator routes.

These pin the route layer's contract (authz mapping, kwarg pass-through, 503
when the version store is down) without live PG/Qdrant, by invoking the route
handlers directly with a fake KnowledgeService + fake orchestrator — the same
pattern as test_gateway_dataset_authorize.py. The orchestrator itself is
covered by test_embedding_versioning_unit.py / the tier-b migration tests.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.embedding_migration import (
    MigrationCutoverSchema,
    MigrationGateSchema,
    MigrationRollbackSchema,
    MigrationStartSchema,
    abort_migration,
    backfill_migration,
    cancel_migration_job,
    cutover_migration,
    describe_migration,
    gate_migration,
    get_migration_job,
    rollback_migration,
    start_migration,
    verify_migration,
)
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError, ValidationFailedError
from knowledge_service.persistence.embedding_version_store import BindingConflictError
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.embedding_migration import (
    EmbeddingMigrationError,
    MigrationStateError,
)

# A migration id is a UUID end-to-end (the store casts it with ::uuid); the
# route scope check rejects anything else with 404.
_MIGRATION_ID = "3f2c1a4e-9b8d-4e6f-8a1b-2c3d4e5f6071"
_FOREIGN_MIGRATION_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
_JOB_ID = "11111111-2222-4333-8444-555555555555"


class FakeMigrationStore:
    """The slice of EmbeddingVersionStore the route scope check touches."""

    def __init__(self, migrations: dict[str, dict[str, Any]] | None = None) -> None:
        self.migrations = migrations if migrations is not None else {}

    async def get_migration(self, migration_id: str) -> dict[str, Any] | None:
        return self.migrations.get(str(migration_id))


def _default_store() -> FakeMigrationStore:
    # The default fixture owns exactly one migration on the path dataset, so
    # every well-formed in-scope call passes the scope check.
    return FakeMigrationStore(
        {
            _MIGRATION_ID: {
                "migration_id": _MIGRATION_ID,
                "dataset_id": "dataset-a",
                "state": "backfilling",
            }
        }
    )


class FakeMigrationService:
    def __init__(self, *, describe: dict[str, Any] | None = None) -> None:
        self.describe_result = describe or {"dataset_id": "dataset-a"}
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.store = _default_store()
        self.jobs: dict[str, dict[str, Any]] = {}

    async def describe(self, dataset: dict[str, Any]) -> dict[str, Any]:
        self.calls.append(("describe", {"dataset": dataset}))
        return dict(self.describe_result)

    async def start_migration(self, dataset: dict[str, Any], **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("start", {"dataset": dataset, **kwargs}))
        return {"migration": {"migration_id": _MIGRATION_ID}, **kwargs}

    async def backfill(self, migration_id: str, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("backfill", {"migration_id": migration_id, **kwargs}))
        return {"migration_id": migration_id, "state": "backfilling"}

    async def verify(self, migration_id: str) -> dict[str, Any]:
        self.calls.append(("verify", {"migration_id": migration_id}))
        return {"migration_id": migration_id, "state": "verified"}

    async def run_gate(self, migration_id: str, evaluate: Any) -> dict[str, Any]:
        # Do NOT invoke evaluate here: the route test pins wiring, not the
        # evaluator (covered separately). Record that a callable was supplied.
        self.calls.append(
            ("gate", {"migration_id": migration_id, "callable": callable(evaluate)})
        )
        return {"migration_id": migration_id, "passed": True}

    async def enqueue_action(
        self,
        migration_id: str,
        *,
        action: str,
        payload: dict[str, Any] | None = None,
        requested_by: str = "",
    ) -> dict[str, Any]:
        self.calls.append(
            (
                "enqueue",
                {
                    "migration_id": migration_id,
                    "action": action,
                    "payload": payload or {},
                    "requested_by": requested_by,
                },
            )
        )
        job = {
            "job_id": _JOB_ID,
            "migration_id": migration_id,
            "dataset_id": "dataset-a",
            "action": action,
            "state": "queued",
            "reused": False,
        }
        self.jobs[_JOB_ID] = job
        return dict(job)

    async def get_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        job = self.jobs.get(job_id)
        if (
            job is None
            or str(job.get("migration_id") or "") != migration_id
            or str(job.get("dataset_id") or "") != dataset_id
        ):
            return None
        return job

    async def cancel_action_job(
        self,
        job_id: str,
        *,
        migration_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        job = await self.get_action_job(
            job_id,
            migration_id=migration_id,
            dataset_id=dataset_id,
        )
        if job is not None:
            job = {**job, "state": "failed", "error": "cancelled by operator"}
            self.jobs[job_id] = job
        return job

    async def cutover(self, migration_id: str, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("cutover", {"migration_id": migration_id, **kwargs}))
        return {"migration_id": migration_id, "state": "completed"}

    async def rollback(self, migration_id: str, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("rollback", {"migration_id": migration_id, **kwargs}))
        return {"migration_id": migration_id, "state": "rolled_back"}

    async def abort(self, migration_id: str, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("abort", {"migration_id": migration_id, **kwargs}))
        return {"migration_id": migration_id, "state": "abandoned"}


def _svc(
    *,
    migration_service: Any | None = "auto",
    deny: bool = False,
    index_config: Any = None,
) -> SimpleNamespace:
    service = (
        FakeMigrationService() if migration_service == "auto" else migration_service
    )

    async def require_dataset_access(
        _user: Any, dataset_id: str, required: str = "viewer"
    ) -> dict[str, Any]:
        if deny:
            raise PermissionDeniedError("not the owner")
        dataset = {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "collection-a",
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 2,
        }
        if index_config is not None:
            dataset["index_config"] = index_config
        return dataset

    return SimpleNamespace(
        embedding_migration_service=service,
        embedding_version_store=object(),
        require_dataset_access=require_dataset_access,
        vector_store=SimpleNamespace(),
        db=SimpleNamespace(),
    )


_USER = SimpleNamespace(user_id="user-a", tenant_id="tenant-a")


@pytest.mark.asyncio
async def test_describe_returns_orchestrator_payload() -> None:
    payload = {
        "dataset_id": "dataset-a",
        "active_action_job": {"job_id": _JOB_ID, "state": "running"},
        "recent_action_jobs": [{"job_id": "older-job", "state": "failed"}],
    }
    svc = _svc(migration_service=FakeMigrationService(describe=payload))
    result = await describe_migration("dataset-a", svc=svc, user=_USER)
    assert result == payload


@pytest.mark.asyncio
async def test_start_maps_body_to_orchestrator_kwargs() -> None:
    svc = _svc()
    body = MigrationStartSchema(
        embedding_provider="openai_compatible",
        embedding_model="Qwen3-Embedding-0.6B",
        embedding_model_version="v1",
        embedding_dimension=1024,
        capabilities=["vision"],
    )
    await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "start"
    assert kwargs["target_provider"] == "openai_compatible"
    assert kwargs["target_model"] == "Qwen3-Embedding-0.6B"
    assert kwargs["target_model_version"] == "v1"
    assert kwargs["target_dimension"] == 1024
    assert kwargs["capabilities"] == ["vision"]


@pytest.mark.asyncio
async def test_start_without_capabilities_passes_none_for_inheritance() -> None:
    # None (not []) reaches the orchestrator so it inherits the serving
    # binding's capabilities; an explicit [] would be operator intent.
    svc = _svc()
    body = MigrationStartSchema(
        embedding_provider="openai_compatible",
        embedding_model="Qwen3-Embedding-0.6B",
        embedding_dimension=1024,
    )
    await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "start"
    assert kwargs["capabilities"] is None


@pytest.mark.asyncio
async def test_start_passes_configured_dataset_lexical_config_to_orchestrator() -> None:
    # The shadow generation must be built with the dataset's configured lexical
    # leg: a retrieval.lexical selection in index_config reaches the
    # orchestrator as a configured LexicalConfig (review-t3-wiring P2-5).
    svc = _svc(
        index_config={
            "retrieval": {
                "lexical": {
                    "active_version": "lexical_v1",
                    "bm25_v2": {"shadow_write_enabled": True},
                }
            }
        }
    )
    body = MigrationStartSchema(
        embedding_provider="openai_compatible",
        embedding_model="Qwen3-Embedding-0.6B",
        embedding_dimension=1024,
    )
    await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "start"
    lexical = kwargs["lexical_config"]
    assert lexical is not None
    assert lexical.configured is True
    assert lexical.writes_bm25_v2 is True


@pytest.mark.asyncio
async def test_start_without_lexical_selection_passes_unconfigured_config() -> None:
    # No retrieval.lexical in index_config: the derived config is unconfigured
    # and the orchestrator's `.configured` gate drops it (no lexical kwarg
    # reaches ensure_collection).
    svc = _svc()
    body = MigrationStartSchema(
        embedding_provider="openai_compatible",
        embedding_model="Qwen3-Embedding-0.6B",
        embedding_dimension=1024,
    )
    await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    _name, kwargs = svc.embedding_migration_service.calls[-1]
    assert kwargs["lexical_config"].configured is False


@pytest.mark.asyncio
async def test_start_rejects_malformed_dataset_lexical_config_with_400() -> None:
    svc = _svc(
        index_config={"retrieval": {"lexical": {"active_version": "bogus_version"}}}
    )
    body = MigrationStartSchema(
        embedding_provider="openai_compatible",
        embedding_model="Qwen3-Embedding-0.6B",
        embedding_dimension=1024,
    )
    with pytest.raises(HTTPException) as excinfo:
        await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    assert excinfo.value.status_code == 400
    assert "lexical config is invalid" in str(excinfo.value.detail)
    # The orchestrator was never invoked with a broken config.
    assert all(name != "start" for name, _ in svc.embedding_migration_service.calls)


@pytest.mark.asyncio
async def test_owner_gate_maps_permission_denied_to_403() -> None:
    svc = _svc(deny=True)
    with pytest.raises(HTTPException) as excinfo:
        await describe_migration("dataset-a", svc=svc, user=_USER)
    assert excinfo.value.status_code == 403


@pytest.mark.asyncio
async def test_start_rejects_cross_tenant_admin_from_real_dataset_service() -> None:
    class DatasetDatabase:
        async def get_dataset(self, dataset_id: str) -> dict[str, Any] | None:
            if dataset_id != "dataset-a":
                return None
            return {
                "dataset_id": dataset_id,
                "tenant_id": "tenant-a",
                "visibility": "private",
                "created_by": "owner-a",
            }

        async def get_dataset_permission(
            self, _dataset_id: str, _subject_type: str, _subject_id: str
        ) -> None:
            return None

    migration_service = FakeMigrationService()
    dataset_service = DatasetService(  # type: ignore[arg-type]
        SimpleNamespace(), DatasetDatabase()
    )
    svc = SimpleNamespace(
        embedding_migration_service=migration_service,
        embedding_version_store=object(),
        require_dataset_access=dataset_service.require_dataset_access,
    )
    foreign_admin = UserContext(
        user_id="admin-b",
        tenant_id="tenant-b",
        user_tier="admin",
        roles=["user"],
    )

    with pytest.raises(HTTPException) as excinfo:
        await start_migration(
            "dataset-a",
            body=MigrationStartSchema(
                embedding_provider="local",
                embedding_model="hash-384-v2",
                embedding_dimension=384,
            ),
            svc=svc,  # type: ignore[arg-type]
            user=foreign_admin,
        )

    assert excinfo.value.status_code == 403
    assert migration_service.calls == []

    with pytest.raises(HTTPException) as describe_exc:
        await describe_migration(
            "dataset-a",
            svc=svc,  # type: ignore[arg-type]
            user=foreign_admin,
        )
    assert describe_exc.value.status_code == 403
    assert migration_service.calls == []


@pytest.mark.asyncio
async def test_missing_migration_service_is_503() -> None:
    svc = _svc(migration_service=None)
    with pytest.raises(HTTPException) as excinfo:
        await describe_migration("dataset-a", svc=svc, user=_USER)
    assert excinfo.value.status_code == 503


@pytest.mark.asyncio
async def test_gate_enqueues_serializable_evaluator_overrides() -> None:
    svc = _svc()
    body = MigrationGateSchema(sample_size=8, top_k=3, tolerance=0.2, floor=0.7)
    result = await gate_migration("dataset-a", _MIGRATION_ID, body=body, svc=svc, user=_USER)
    assert result["state"] == "queued"
    assert result["job_id"] == _JOB_ID
    assert result["execution_id"] == _JOB_ID
    assert result["job_url"].endswith(f"/{_MIGRATION_ID}/jobs/{_JOB_ID}")
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "enqueue"
    assert kwargs["migration_id"] == _MIGRATION_ID
    assert kwargs["action"] == "gate"
    assert kwargs["payload"] == {
        "sample_size": 8,
        "top_k": 3,
        "tolerance": 0.2,
        "floor": 0.7,
    }


@pytest.mark.asyncio
async def test_migration_job_get_and_cancel_share_durable_receipt_url() -> None:
    svc = _svc()
    enqueued = await backfill_migration(
        "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
    )
    fetched = await get_migration_job(
        "dataset-a", _MIGRATION_ID, _JOB_ID, svc=svc, user=_USER
    )
    cancelled = await cancel_migration_job(
        "dataset-a", _MIGRATION_ID, _JOB_ID, svc=svc, user=_USER
    )

    assert enqueued["job_url"] == fetched["job_url"] == cancelled["job_url"]
    assert cancelled["state"] == "failed"
    assert cancelled["error"] == "cancelled by operator"


@pytest.mark.asyncio
async def test_backfill_verify_cutover_pass_through() -> None:
    svc = _svc()
    await backfill_migration("dataset-a", _MIGRATION_ID, svc=svc, user=_USER)
    await verify_migration("dataset-a", _MIGRATION_ID, svc=svc, user=_USER)
    await cutover_migration(
        "dataset-a", _MIGRATION_ID, body=MigrationCutoverSchema(retention_seconds=60),
        svc=svc, user=_USER,
    )
    names = [name for name, _ in svc.embedding_migration_service.calls]
    assert names == ["enqueue", "enqueue", "cutover"]
    assert svc.embedding_migration_service.calls[0][1]["action"] == "backfill"
    assert svc.embedding_migration_service.calls[1][1]["action"] == "verify"
    cutover_kwargs = svc.embedding_migration_service.calls[-1][1]
    assert cutover_kwargs["retention_seconds"] == 60


@pytest.mark.asyncio
async def test_abort_maps_body_kwargs() -> None:
    svc = _svc()
    await abort_migration(
        "dataset-a", _MIGRATION_ID,
        body=SimpleNamespace(reason="stopped", purge_shadow=False),
        svc=svc, user=_USER,
    )
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "abort"
    assert kwargs["reason"] == "stopped"
    assert kwargs["purge_shadow"] is False


@pytest.mark.asyncio
async def test_abort_defaults_reason_and_purge_shadow() -> None:
    svc = _svc()
    await abort_migration("dataset-a", _MIGRATION_ID, body=None, svc=svc, user=_USER)
    name, kwargs = svc.embedding_migration_service.calls[-1]
    assert name == "abort"
    assert kwargs["reason"] == "aborted"
    assert kwargs["purge_shadow"] is True


@pytest.mark.asyncio
async def test_rollback_keep_shadow_defaults_true_and_override_flips_it() -> None:
    svc = _svc()
    await rollback_migration("dataset-a", _MIGRATION_ID, body=None, svc=svc, user=_USER)
    await rollback_migration(
        "dataset-a", _MIGRATION_ID, body=MigrationRollbackSchema(keep_shadow=False), svc=svc, user=_USER
    )
    assert svc.embedding_migration_service.calls[0][1]["keep_shadow"] is True
    assert svc.embedding_migration_service.calls[1][1]["keep_shadow"] is False


# ----------------------------------------------------------- durable actions


@pytest.mark.asyncio
async def test_gate_without_body_enqueues_empty_payload() -> None:
    svc = _svc()
    result = await gate_migration(
        "dataset-a", _MIGRATION_ID, body=None, svc=svc, user=_USER
    )
    assert result["state"] == "queued"
    assert svc.embedding_migration_service.calls[-1][1]["payload"] == {}


@pytest.mark.asyncio
async def test_long_backfill_is_never_run_in_http_request() -> None:
    class BlockingLegacyAction(FakeMigrationService):
        async def backfill(
            self, migration_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            await asyncio.sleep(31)
            return {"migration_id": migration_id}

    service = BlockingLegacyAction()
    svc = _svc(migration_service=service)
    result = await asyncio.wait_for(
        backfill_migration(
            "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
        ),
        timeout=0.1,
    )
    assert result["job_id"] == _JOB_ID
    assert [name for name, _kwargs in service.calls] == ["enqueue"]


@pytest.mark.asyncio
async def test_disconnect_during_enqueue_does_not_cancel_transaction() -> None:
    class SlowEnqueue(FakeMigrationService):
        def __init__(self) -> None:
            super().__init__()
            self.started = asyncio.Event()
            self.release = asyncio.Event()

        async def enqueue_action(self, *args: Any, **kwargs: Any) -> dict[str, Any]:
            self.started.set()
            await self.release.wait()
            return await super().enqueue_action(*args, **kwargs)

    service = SlowEnqueue()
    svc = _svc(migration_service=service)
    request = asyncio.create_task(
        backfill_migration(
            "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
        )
    )
    await asyncio.wait_for(service.started.wait(), timeout=1)
    request.cancel()
    await asyncio.sleep(0)
    assert not request.done()
    request.cancel()
    await asyncio.sleep(0)
    assert not request.done()
    service.release.set()
    with pytest.raises(asyncio.CancelledError):
        await request
    assert service.jobs[_JOB_ID]["state"] == "queued"


@pytest.mark.asyncio
async def test_job_poll_is_scoped_to_dataset_and_migration() -> None:
    svc = _svc()
    await backfill_migration(
        "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
    )
    job = await get_migration_job(
        "dataset-a",
        _MIGRATION_ID,
        _JOB_ID,
        svc=svc,
        user=_USER,
    )
    assert job["state"] == "queued"
    assert job["action"] == "backfill"

    svc.embedding_migration_service.jobs[_JOB_ID] = {
        **job,
        "dataset_id": "dataset-other",
    }
    with pytest.raises(HTTPException) as excinfo:
        await get_migration_job(
            "dataset-a",
            _MIGRATION_ID,
            _JOB_ID,
            svc=svc,
            user=_USER,
        )
    assert excinfo.value.status_code == 404


@pytest.mark.asyncio
async def test_missing_action_job_store_maps_to_503() -> None:
    class MissingJobStore(FakeMigrationService):
        async def enqueue_action(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            raise RuntimeError("embedding migration action jobs require migration 110")

    svc = _svc(migration_service=MissingJobStore())
    with pytest.raises(HTTPException) as excinfo:
        await gate_migration(
            "dataset-a", _MIGRATION_ID, body=None, svc=svc, user=_USER
        )
    assert excinfo.value.status_code == 503
    assert "migration 110" in str(excinfo.value.detail)


@pytest.mark.asyncio
async def test_missing_action_job_store_on_describe_maps_to_503() -> None:
    class MissingJobStore(FakeMigrationService):
        async def describe(self, _dataset: dict[str, Any]) -> dict[str, Any]:
            raise RuntimeError("embedding migration action jobs require migration 110")

    svc = _svc(migration_service=MissingJobStore())
    with pytest.raises(HTTPException) as excinfo:
        await describe_migration("dataset-a", svc=svc, user=_USER)
    assert excinfo.value.status_code == 503
    assert "migration 110" in str(excinfo.value.detail)


@pytest.mark.asyncio
async def test_database_transport_errors_are_generic_503() -> None:
    class DatabaseDown(FakeMigrationService):
        async def enqueue_action(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            raise OSError("internal database endpoint must not leak")

        async def describe(self, _dataset: dict[str, Any]) -> dict[str, Any]:
            raise OSError("internal database endpoint must not leak")

    svc = _svc(migration_service=DatabaseDown())
    for call in (
        lambda: backfill_migration(
            "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
        ),
        lambda: describe_migration("dataset-a", svc=svc, user=_USER),
    ):
        with pytest.raises(HTTPException) as excinfo:
            await call()
        assert excinfo.value.status_code == 503
        assert excinfo.value.detail == "embedding versioning store unavailable"


# ------------------------------------------------------------- error mapping


@pytest.mark.asyncio
async def test_unknown_dataset_maps_to_404() -> None:
    async def not_found(_user: Any, dataset_id: str, required: str = "viewer") -> dict[str, Any]:
        raise ValidationFailedError("Dataset not found")

    svc = _svc()
    svc.require_dataset_access = not_found
    with pytest.raises(HTTPException) as excinfo:
        await describe_migration("ds-missing", svc=svc, user=_USER)
    assert excinfo.value.status_code == 404


@pytest.mark.asyncio
async def test_missing_version_store_is_503() -> None:
    svc = _svc()
    svc.embedding_version_store = None
    with pytest.raises(HTTPException) as excinfo:
        await describe_migration("dataset-a", svc=svc, user=_USER)
    assert excinfo.value.status_code == 503
    assert excinfo.value.detail == "embedding versioning store unavailable"


@pytest.mark.asyncio
async def test_state_error_maps_to_409_not_500() -> None:
    """MigrationStateError is NOT an EmbeddingMigrationError subclass; the
    except tuples must catch it explicitly or a state race becomes a 500."""

    class StateConflict(FakeMigrationService):
        async def enqueue_action(
            self, _migration_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            raise MigrationStateError("migration in state 'gating' cannot move to verified")

    svc = _svc(migration_service=StateConflict())
    with pytest.raises(HTTPException) as excinfo:
        await verify_migration("dataset-a", _MIGRATION_ID, svc=svc, user=_USER)
    assert excinfo.value.status_code == 409


@pytest.mark.asyncio
async def test_concurrent_backfill_claim_maps_to_stable_409() -> None:
    class BackfillConflict(FakeMigrationService):
        async def enqueue_action(
            self, _migration_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            raise MigrationStateError(
                "embedding migration backfill is already running"
            )

    svc = _svc(migration_service=BackfillConflict())
    with pytest.raises(HTTPException) as excinfo:
        await backfill_migration(
            "dataset-a", _MIGRATION_ID, svc=svc, user=_USER
        )

    assert excinfo.value.status_code == 409
    assert "already running" in str(excinfo.value.detail)


@pytest.mark.asyncio
async def test_binding_conflict_maps_to_409() -> None:
    class ConflictStart(FakeMigrationService):
        async def start_migration(self, _dataset: dict[str, Any], **_kwargs: Any) -> dict[str, Any]:
            raise BindingConflictError("collection name already bound to another dataset")

    svc = _svc(migration_service=ConflictStart())
    body = MigrationStartSchema(
        embedding_provider="local", embedding_model="m2", embedding_dimension=1536
    )
    with pytest.raises(HTTPException) as excinfo:
        await start_migration("dataset-a", body=body, svc=svc, user=_USER)
    assert excinfo.value.status_code == 409


@pytest.mark.asyncio
async def test_orchestrator_request_error_maps_to_400() -> None:
    class BadRequest(FakeMigrationService):
        async def enqueue_action(
            self, _migration_id: str, **_kwargs: Any
        ) -> dict[str, Any]:
            raise EmbeddingMigrationError("no embedder supplied for the target generation")

    svc = _svc(migration_service=BadRequest())
    with pytest.raises(HTTPException) as excinfo:
        await backfill_migration("dataset-a", _MIGRATION_ID, svc=svc, user=_USER)
    assert excinfo.value.status_code == 400
    assert "embedder" in excinfo.value.detail


# ------------------------------------ object-level authorization (scoping)


@pytest.mark.asyncio
async def test_action_on_foreign_dataset_migration_is_404() -> None:
    """A well-formed migration id owned by ANOTHER dataset must be rejected:
    owning the path dataset is necessary but not sufficient to drive someone
    else's migration state machine (cutover included). The service must never
    be invoked."""
    service = FakeMigrationService()
    service.store.migrations[_FOREIGN_MIGRATION_ID] = {
        "migration_id": _FOREIGN_MIGRATION_ID,
        "dataset_id": "dataset-OTHER",
        "state": "ready",
    }
    svc = _svc(migration_service=service)
    for action in (
        lambda: backfill_migration("dataset-a", _FOREIGN_MIGRATION_ID, svc=svc, user=_USER),
        lambda: verify_migration("dataset-a", _FOREIGN_MIGRATION_ID, svc=svc, user=_USER),
        lambda: gate_migration("dataset-a", _FOREIGN_MIGRATION_ID, body=None, svc=svc, user=_USER),
        lambda: cutover_migration("dataset-a", _FOREIGN_MIGRATION_ID, body=None, svc=svc, user=_USER),
        lambda: rollback_migration("dataset-a", _FOREIGN_MIGRATION_ID, body=None, svc=svc, user=_USER),
        lambda: abort_migration("dataset-a", _FOREIGN_MIGRATION_ID, body=None, svc=svc, user=_USER),
        lambda: get_migration_job(
            "dataset-a",
            _FOREIGN_MIGRATION_ID,
            _JOB_ID,
            svc=svc,
            user=_USER,
        ),
    ):
        with pytest.raises(HTTPException) as excinfo:
            await action()
        assert excinfo.value.status_code == 404
    assert service.calls == []


@pytest.mark.asyncio
async def test_action_on_malformed_migration_id_is_404() -> None:
    svc = _svc()
    with pytest.raises(HTTPException) as excinfo:
        await backfill_migration("dataset-a", "not-a-uuid", svc=svc, user=_USER)
    assert excinfo.value.status_code == 404
    assert svc.embedding_migration_service.calls == []


@pytest.mark.asyncio
async def test_action_on_missing_migration_is_404() -> None:
    svc = _svc()
    missing = "99999999-8888-4777-8666-555555555555"
    with pytest.raises(HTTPException) as excinfo:
        await verify_migration("dataset-a", missing, svc=svc, user=_USER)
    assert excinfo.value.status_code == 404
    assert svc.embedding_migration_service.calls == []


@pytest.mark.asyncio
async def test_in_scope_migration_passes_the_scope_check() -> None:
    svc = _svc()
    await backfill_migration("dataset-a", _MIGRATION_ID, svc=svc, user=_USER)
    names = [name for name, _ in svc.embedding_migration_service.calls]
    assert names == ["enqueue"]


# --------------------------------------------------------- router registration


def test_router_exposes_the_declared_migration_surface() -> None:
    import knowledge_service.api.routes.embedding_migration as module

    paths = {
        (route.path, tuple(sorted(route.methods or ()))) for route in module.router.routes
    }
    base = "/knowledge/datasets/{dataset_id}/embedding-migration"
    assert (base, ("GET",)) in paths
    assert (f"{base}/start", ("POST",)) in paths
    for verb in ("backfill", "verify", "gate", "cutover", "rollback", "abort"):
        assert (f"{base}/{{migration_id}}/{verb}", ("POST",)) in paths
    assert (
        f"{base}/{{migration_id}}/jobs/{{job_id}}",
        ("GET",),
    ) in paths
    async_routes = {
        route.path: route.status_code
        for route in module.router.routes
        if route.path.endswith(("/backfill", "/verify", "/gate"))
    }
    assert set(async_routes.values()) == {202}
