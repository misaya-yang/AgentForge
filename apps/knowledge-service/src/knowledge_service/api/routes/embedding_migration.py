"""PRD T3 blue-green embedding migration: operator API surface.

This router exposes the already-delivered ``EmbeddingMigrationService``
orchestrator (services/knowledge/embedding_migration.py) as owner-gated
endpoints so an operator can drive a model/dimension change as a blue-green
migration instead of an incident:

    describe -> start -> enqueue(backfill) -> poll -> enqueue(verify) -> poll
             -> enqueue(gate) -> poll -> cutover -> rollback / abort

Backfill/verify/gate are durable PostgreSQL jobs and return HTTP 202. A
worker-role process builds the real shadow-vs-serving evaluator and executes
the action; no FastAPI background task or request-owned coroutine performs
paid work. Cutover remains legal only after the worker records a passing gate.

Conventions mirror routes/knowledge.py: the dataset is resolved through
``svc.require_dataset_access(user, dataset_id, required="owner")`` and service
errors map to 403 / 404 / 400 / 409. When the embedding-version store is
unavailable (PG pool down) every endpoint answers 503 rather than silently
degrading a control plane.

Object-level authorization: owning the PATH dataset is necessary but not
sufficient. Every ``{migration_id}`` action re-checks (via
``_scoped_migration``) that the migration belongs to that dataset and rejects
a foreign or malformed id with 404 — otherwise any dataset owner who learned a
migration UUID (from logs, ``totals.target_collection``, or the gate route)
could drive another dataset's state machine, including its cutover.
"""

from __future__ import annotations

import asyncio
import uuid
from typing import Any

from fastapi import APIRouter, Body, Depends, HTTPException
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ...core.exceptions import PermissionDeniedError, ValidationFailedError
from ...persistence.embedding_version_store import (
    BindingConflictError,
    MigrationStateError,
)
from ...services.knowledge.embedding_migration import (
    EmbeddingMigrationError,
    MixedModelEmbeddingError,
)
from ...services.knowledge.knowledge_service import KnowledgeService
from ...services.knowledge.lexical_config import LexicalConfig, LexicalConfigError
from ..deps import get_knowledge_service, get_user_context

router = APIRouter()

# Everything the orchestrator is allowed to answer "no" with. State-machine
# violations and binding races are conflicts (409); illegal migration requests
# (EmbeddingMigrationError and subclasses) are caller errors (400). Migration-
# StateError/BindingConflictError derive from EmbeddingVersionError, NOT from
# EmbeddingMigrationError, so the except tuples must list all of them or an
# unhandled 500 escapes.
_MIGRATION_ERRORS = (
    MigrationStateError,
    BindingConflictError,
    EmbeddingMigrationError,
    MixedModelEmbeddingError,
)


# --------------------------------------------------------------------------- #
# Request models (kept local: api/schemas/knowledge.py is owned elsewhere).
# --------------------------------------------------------------------------- #
class MigrationStartSchema(BaseModel):
    embedding_provider: str = Field(..., min_length=1)
    embedding_model: str = Field(..., min_length=1)
    embedding_model_version: str = ""
    embedding_dimension: int = Field(..., gt=0, le=8192)
    capabilities: list[str] | None = None


class MigrationGateSchema(BaseModel):
    sample_size: int | None = Field(default=None, ge=1, le=256)
    top_k: int | None = Field(default=None, ge=1, le=50)
    tolerance: float | None = Field(default=None, ge=0.0, le=1.0)
    floor: float | None = Field(default=None, ge=0.0, le=1.0)


class MigrationCutoverSchema(BaseModel):
    retention_seconds: int | None = Field(default=None, ge=0)


class MigrationRollbackSchema(BaseModel):
    keep_shadow: bool | None = None


class MigrationAbortSchema(BaseModel):
    reason: str | None = None
    purge_shadow: bool | None = None


# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
def _migration_service(svc: KnowledgeService | None) -> Any:
    """Return the orchestrator or fail the control plane loudly (503)."""
    if (
        svc is None
        or getattr(svc, "embedding_migration_service", None) is None
        or getattr(svc, "embedding_version_store", None) is None
    ):
        raise HTTPException(
            status_code=503,
            detail="embedding versioning store unavailable",
        )
    return svc.embedding_migration_service


async def _owner_dataset(
    svc: KnowledgeService, user: UserContext, dataset_id: str
) -> dict[str, Any]:
    try:
        return await svc.require_dataset_access(user, dataset_id, required="owner")
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc


async def _scoped_migration(
    service: Any, dataset_id: str, migration_id: str
) -> dict[str, Any]:
    """Object-level authorization for the ``{migration_id}`` path segment.

    Dataset ownership was already enforced by the caller; this re-checks that
    the migration belongs to that SAME dataset. A malformed id (not a UUID) or
    a migration owned by another dataset is answered with 404 — the same
    response as a genuinely missing migration — so the endpoint never becomes
    an oracle for probing foreign migration ids, and one owner can never drive
    another dataset's migration state machine (backfill through cutover).
    """
    try:
        uuid.UUID(str(migration_id or "").strip())
    except (ValueError, AttributeError, TypeError) as exc:
        raise HTTPException(status_code=404, detail="migration not found") from exc
    migration = await service.store.get_migration(migration_id)
    if migration is None:
        raise HTTPException(status_code=404, detail="migration not found")
    if str(migration.get("dataset_id") or "") != str(dataset_id):
        # Deliberately indistinguishable from "not found": leaking that the
        # id exists-but-belongs-elsewhere would be an enumeration oracle.
        raise HTTPException(status_code=404, detail="migration not found")
    return migration


def _map_migration_error(exc: Exception) -> HTTPException:
    # State / binding conflicts are 409; everything else raised by the
    # orchestrator is a caller error (400).
    if isinstance(exc, (MigrationStateError, BindingConflictError)):
        return HTTPException(status_code=409, detail=str(exc))
    return HTTPException(status_code=400, detail=str(exc))


def _store_unavailable(exc: Exception) -> HTTPException:
    detail = (
        str(exc)
        if isinstance(exc, RuntimeError)
        else "embedding versioning store unavailable"
    )
    return HTTPException(status_code=503, detail=detail)


async def _enqueue_action(
    service: Any,
    dataset_id: str,
    migration_id: str,
    *,
    action: str,
    user: UserContext,
    payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    enqueue = asyncio.create_task(
        service.enqueue_action(
            migration_id,
            action=action,
            payload=payload,
            requested_by=str(getattr(user, "user_id", "") or ""),
        )
    )
    try:
        job = await asyncio.shield(enqueue)
        return _migration_job_receipt(dataset_id, migration_id, job)
    except asyncio.CancelledError as cancellation:
        # Disconnect cancellation must not roll back an in-flight enqueue.
        # This shields only the short PostgreSQL transaction; paid/long work
        # is exclusively worker-owned.
        while not enqueue.done():
            try:
                await asyncio.shield(enqueue)
            except asyncio.CancelledError:
                # A repeated disconnect/shutdown cancellation still must not
                # leak through to the transaction task.
                continue
        if not enqueue.cancelled():
            enqueue.exception()  # consume a database error after client exit
        raise cancellation
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except Exception as exc:
        raise _store_unavailable(exc) from exc


def _migration_job_receipt(
    dataset_id: str,
    migration_id: str,
    job: dict[str, Any],
) -> dict[str, Any]:
    job_id = str(job.get("job_id") or "").strip()
    return {
        **job,
        "execution_id": job_id or None,
        "job_url": (
            f"/api/v1/knowledge/datasets/{dataset_id}/embedding-migration/"
            f"{migration_id}/jobs/{job_id}"
            if job_id
            else None
        ),
    }


# --------------------------------------------------------------------------- #
# Endpoints
# --------------------------------------------------------------------------- #
@router.get("/knowledge/datasets/{dataset_id}/embedding-migration")
async def describe_migration(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    dataset = await _owner_dataset(svc, user, dataset_id)
    try:
        return await service.describe(dataset)
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc
    except Exception as exc:
        raise _store_unavailable(exc) from exc


@router.post("/knowledge/datasets/{dataset_id}/embedding-migration/start")
async def start_migration(
    dataset_id: str,
    body: MigrationStartSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    dataset = await _owner_dataset(svc, user, dataset_id)
    # The shadow generation must be built with the dataset's configured lexical
    # leg, not an unconfigured default: otherwise a dataset whose retrieval
    # config selects a lexical version would get a shadow collection that
    # cannot serve it after cutover. A malformed persisted config is a 400
    # (the dataset must be repaired before a migration can start).
    try:
        lexical_config = LexicalConfig.from_index_config(dataset.get("index_config"))
    except LexicalConfigError as exc:
        raise HTTPException(
            status_code=400,
            detail=f"dataset lexical config is invalid: {exc}",
        ) from exc
    try:
        return await service.start_migration(
            dataset,
            target_provider=body.embedding_provider,
            target_model=body.embedding_model,
            target_model_version=body.embedding_model_version,
            target_dimension=body.embedding_dimension,
            # None = inherit the serving binding's capabilities in the
            # orchestrator; an explicit list (even []) is operator intent.
            capabilities=body.capabilities,
            # Unconfigured (no retrieval.lexical in index_config) instances are
            # dropped by the orchestrator's `.configured` gate.
            lexical_config=lexical_config,
        )
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/backfill",
    status_code=202,
)
async def backfill_migration(
    dataset_id: str,
    migration_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    return await _enqueue_action(
        service,
        dataset_id,
        migration_id,
        action="backfill",
        user=user,
    )


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/verify",
    status_code=202,
)
async def verify_migration(
    dataset_id: str,
    migration_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    return await _enqueue_action(
        service,
        dataset_id,
        migration_id,
        action="verify",
        user=user,
    )


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/gate",
    status_code=202,
)
async def gate_migration(
    dataset_id: str,
    migration_id: str,
    body: MigrationGateSchema | None = Body(None),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    overrides: dict[str, Any] = {}
    if body is not None:
        for key in ("sample_size", "top_k", "tolerance", "floor"):
            value = getattr(body, key)
            if value is not None:
                overrides[key] = value
    return await _enqueue_action(
        service,
        dataset_id,
        migration_id,
        action="gate",
        payload=overrides,
        user=user,
    )


@router.get(
    "/knowledge/datasets/{dataset_id}/embedding-migration/"
    "{migration_id}/jobs/{job_id}"
)
async def get_migration_job(
    dataset_id: str,
    migration_id: str,
    job_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    try:
        uuid.UUID(str(job_id or "").strip())
    except (ValueError, AttributeError, TypeError) as exc:
        raise HTTPException(status_code=404, detail="migration job not found") from exc
    try:
        job = await service.get_action_job(
            job_id,
            migration_id=migration_id,
            dataset_id=dataset_id,
        )
    except Exception as exc:
        raise _store_unavailable(exc) from exc
    if (
        job is None
        or str(job.get("migration_id") or "") != str(migration_id)
        or str(job.get("dataset_id") or "") != str(dataset_id)
    ):
        raise HTTPException(status_code=404, detail="migration job not found")
    return _migration_job_receipt(dataset_id, migration_id, job)


@router.delete(
    "/knowledge/datasets/{dataset_id}/embedding-migration/"
    "{migration_id}/jobs/{job_id}"
)
async def cancel_migration_job(
    dataset_id: str,
    migration_id: str,
    job_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    try:
        uuid.UUID(str(job_id or "").strip())
    except (ValueError, AttributeError, TypeError) as exc:
        raise HTTPException(status_code=404, detail="migration job not found") from exc
    try:
        job = await service.cancel_action_job(
            job_id,
            migration_id=migration_id,
            dataset_id=dataset_id,
        )
    except MigrationStateError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except Exception as exc:
        raise _store_unavailable(exc) from exc
    if job is None:
        raise HTTPException(status_code=404, detail="migration job not found")
    return _migration_job_receipt(dataset_id, migration_id, job)


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/cutover"
)
async def cutover_migration(
    dataset_id: str,
    migration_id: str,
    body: MigrationCutoverSchema | None = Body(None),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    kwargs: dict[str, Any] = {}
    if body is not None and body.retention_seconds is not None:
        kwargs["retention_seconds"] = int(body.retention_seconds)
    try:
        return await service.cutover(migration_id, **kwargs)
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/rollback"
)
async def rollback_migration(
    dataset_id: str,
    migration_id: str,
    body: MigrationRollbackSchema | None = Body(None),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    keep_shadow = True if body is None or body.keep_shadow is None else bool(body.keep_shadow)
    try:
        return await service.rollback(migration_id, keep_shadow=keep_shadow)
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc


@router.post(
    "/knowledge/datasets/{dataset_id}/embedding-migration/{migration_id}/abort"
)
async def abort_migration(
    dataset_id: str,
    migration_id: str,
    body: MigrationAbortSchema | None = Body(None),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    service = _migration_service(svc)
    await _owner_dataset(svc, user, dataset_id)
    await _scoped_migration(service, dataset_id, migration_id)
    reason = str(body.reason) if body is not None and body.reason else "aborted"
    purge_shadow = True if body is None or body.purge_shadow is None else bool(body.purge_shadow)
    try:
        return await service.abort(
            migration_id, reason=reason, purge_shadow=purge_shadow
        )
    except _MIGRATION_ERRORS as exc:
        raise _map_migration_error(exc) from exc
