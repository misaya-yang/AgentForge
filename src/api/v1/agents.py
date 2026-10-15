"""Tenant-scoped Agent Studio identity, Draft, Version and ACL APIs."""

from __future__ import annotations

import json
import os
import re
import time
import uuid
from typing import Annotated, Any

from ai_gateway_core.eval.agent_version_candidate import (
    AgentReleaseCandidateError,
    AgentReleaseProfileUnavailableError,
    build_agent_version_candidate,
    build_model_authorization_evidence,
    evaluate_agent_version_candidate,
    require_available_release_profile,
)
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentArchivedError,
    AgentDraftConflictError,
    AgentLastOwnerError,
    AgentNotFoundError,
    AgentPrincipalNotFoundError,
    AgentPublicationNotFoundError,
    AgentReleaseEvaluationNotFoundError,
    AgentReleaseEvaluationStaleError,
    AgentReleaseEvaluationTerminalError,
    AgentReleaseGateError,
    AgentReleaseIdempotencyConflictError,
    AgentRepositoryError,
    AgentRuntimeUnavailableError,
    AgentValidationError,
    DatabaseAgentRepository,
    require_public_release_expiry,
)
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
)
from ai_gateway_core.security.redaction import redact_trace_text
from ai_gateway_core.storage import get_file_storage
from fastapi import APIRouter, Depends, Header, HTTPException, Query, Request, Response

from ...core.auth.user_resolver import UserContext
from ...services.agent_runtime_cleanup import (
    AgentRuntimeCleanupClient,
    AgentRuntimeCleanupClientError,
)
from ...services.metrics.redaction import redact_sensitive_text
from ..deps import get_user_context
from ..schemas.agent_runtime import (
    AgentApiTokenCreateRequest,
    AgentApiTokenIssueResponse,
    AgentApiTokenListResponse,
    AgentApiTokenMetadata,
    AgentApiTokenRotateRequest,
)
from ..schemas.agents import (
    AgentAnalyticsResponse,
    AgentArchiveRequest,
    AgentAuditEventResponse,
    AgentAuditPageResponse,
    AgentCacheInvalidationResponse,
    AgentCopyRequest,
    AgentCreateRequest,
    AgentCredentialRevocationResponse,
    AgentDataDeletionRequest,
    AgentDataDeletionResponse,
    AgentDetail,
    AgentDraftMutationResponse,
    AgentDraftResponse,
    AgentDraftUpdateRequest,
    AgentErrorResponse,
    AgentGovernancePolicyResponse,
    AgentGovernancePolicyUpdate,
    AgentMemberMutationResponse,
    AgentMemberRequest,
    AgentMemberResponse,
    AgentMutationResponse,
    AgentPageResponse,
    AgentPublicationResponse,
    AgentPublishEventResponse,
    AgentPublishRequest,
    AgentReleaseDiffResponse,
    AgentReleaseEvaluationListResponse,
    AgentReleaseEvaluationRequest,
    AgentReleaseEvaluationResponse,
    AgentReleaseMutationResponse,
    AgentRollbackRequest,
    AgentStatusResponse,
    AgentUpdateRequest,
    AgentValidationResponse,
    AgentVersionMutationResponse,
    AgentVersionResponse,
)

router = APIRouter(prefix="/agents", tags=["Agent Studio"])
publication_router = APIRouter(prefix="/publications", tags=["Agent Studio Publications"])

ERROR_RESPONSES = {
    400: {"model": AgentErrorResponse},
    404: {"model": AgentErrorResponse},
    409: {"model": AgentErrorResponse},
    422: {"model": AgentErrorResponse},
    428: {"model": AgentErrorResponse},
    503: {"model": AgentErrorResponse},
}


def _publish_enabled() -> bool:
    return os.getenv("AGENT_STUDIO_PUBLISH_ENABLED", "true").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def _management_mutations_enabled() -> bool:
    return os.getenv("AGENT_STUDIO_MANAGEMENT_ENABLED", "true").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def _request_id(request: Request) -> str:
    value = str(
        getattr(request.state, "request_id", "")
        or getattr(request.state, "trace_id", "")
        or uuid.uuid4()
    )
    request.state.request_id = value
    return value


def _raise_agent_error(
    request: Request,
    status_code: int,
    code: str,
    message: str,
    **extra: Any,
) -> None:
    detail = {
        "code": code,
        "message": message,
        "request_id": _request_id(request),
        **extra,
    }
    raise HTTPException(status_code=status_code, detail=detail)


def _materialize_agent_default_model(request: Request, spec: dict[str, Any]) -> dict[str, Any]:
    """Seal the deployment default into a Draft before hashing or persistence."""

    materialized = dict(spec)
    model = dict(materialized.get("model") or {})
    model_id = str(model.get("model_id") or "").strip()
    if model_id:
        model["model_id"] = model_id
    else:
        settings = getattr(request.app.state, "settings", None)
        default_model = str(getattr(settings, "default_model", "") or "").strip()
        if not default_model:
            _raise_agent_error(
                request,
                503,
                "AGENT_RUNTIME_MODEL_UNAVAILABLE",
                "Default Agent model is unavailable",
            )
        model["model_id"] = default_model
        # The empty-ID sentinel delegates provider selection to the server too;
        # do not seal a UI placeholder provider beside the resolved model.
        model["provider_id"] = None
    materialized["model"] = model
    return materialized


def _require_actor(request: Request, user: UserContext) -> None:
    if (
        request.method.upper() not in {"GET", "HEAD", "OPTIONS"}
        and not _management_mutations_enabled()
    ):
        _raise_agent_error(
            request,
            503,
            "AGENT_STUDIO_MUTATIONS_DISABLED",
            "Agent Studio mutations are temporarily disabled",
        )
    if not user.is_authenticated or not user.user_id:
        _raise_agent_error(request, 401, "AUTHENTICATION_REQUIRED", "Authentication required")
    if not user.tenant_id or user.tenant_id == "public":
        _raise_agent_error(request, 403, "TENANT_REQUIRED", "Tenant identity required")


def _require_publish_mutation(request: Request) -> None:
    if not _publish_enabled():
        _raise_agent_error(
            request,
            503,
            "AGENT_PUBLISH_DISABLED",
            "Agent publish mutations are disabled",
        )


def _parse_idempotency_key(request: Request, raw: str | None) -> str:
    value = str(raw or "").strip()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{7,254}", value):
        _raise_agent_error(
            request,
            428 if not value else 400,
            "AGENT_IDEMPOTENCY_KEY_REQUIRED" if not value else "AGENT_IDEMPOTENCY_KEY_INVALID",
            "A valid Idempotency-Key header is required",
        )
    return value


def _is_tenant_admin(user: UserContext) -> bool:
    roles = {str(role).lower() for role in (user.roles or [])}
    return bool(roles & {"admin", "tenant_admin"}) or str(user.tier).lower() == "admin"


def _model_access_levels(user: UserContext) -> set[str]:
    if _is_tenant_admin(user):
        return {"public", "premium", "admin"}
    if str(user.tier).lower() in {"premium", "enterprise"}:
        return {"public", "premium"}
    return {"public"}


def _release_audience_user(
    user: UserContext, *, agent_id: str, channel: str, auth_mode: str
) -> UserContext:
    """Check broad releases with a representative least-privileged reader."""

    if channel == "api":
        return UserContext(
            user_id=f"agent-token:release-check:{agent_id}",
            tenant_id=user.tenant_id,
            tier="service",
            is_authenticated=True,
            roles=["agent_runtime"],
            user_type="service",
        )
    if channel not in {"hosted", "embed"}:
        return user
    if auth_mode == "tenant":
        return UserContext(
            user_id=f"agent-release-tenant:{agent_id}",
            tenant_id=user.tenant_id,
            tier="normal",
            is_authenticated=True,
            roles=["user"],
        )
    if auth_mode not in {"public", "token"}:
        return user
    return UserContext(
        user_id=f"agent-release-public:{agent_id}",
        tenant_id=user.tenant_id,
        tier="anonymous",
        is_authenticated=False,
        roles=["guest"],
        ip="",
        user_type="anonymous",
    )


def _get_repository(request: Request) -> Any:
    repository = getattr(request.app.state, "agent_repository", None)
    if repository is not None:
        return repository
    database = getattr(request.app.state, "database", None)
    if database is None:
        _raise_agent_error(request, 503, "AGENT_STORAGE_UNAVAILABLE", "Agent storage unavailable")
    repository = DatabaseAgentRepository(
        database,
        knowledge_resolver=getattr(
            request.app.state,
            "agent_runtime_knowledge_resolver",
            None,
        ),
    )
    request.app.state.agent_repository = repository
    return repository


def _get_runtime_cleanup_client(request: Request) -> AgentRuntimeCleanupClient:
    client = getattr(request.app.state, "agent_runtime_cleanup_client", None)
    if client is None:
        database = getattr(request.app.state, "database", None)
        client = AgentRuntimeCleanupClient(database=database)
        request.app.state.agent_runtime_cleanup_client = client
    return client


def _get_trace_repository(request: Request) -> AgentTraceRepository:
    repository = getattr(request.app.state, "agent_trace_repository", None)
    if repository is not None:
        return repository
    database = getattr(request.app.state, "database", None)
    if database is None:
        _raise_agent_error(request, 503, "AGENT_STORAGE_UNAVAILABLE", "Agent storage unavailable")
    repository = AgentTraceRepository(database)
    request.app.state.agent_trace_repository = repository
    return repository


def _parse_etag(request: Request, raw: str | None) -> int:
    if raw is None:
        _raise_agent_error(
            request,
            428,
            "AGENT_DRAFT_PRECONDITION_REQUIRED",
            "If-Match with the current Draft revision is required",
        )
    value = raw.strip()
    match = re.fullmatch(r'"([1-9][0-9]*)"', value)
    if match is None:
        _raise_agent_error(request, 400, "AGENT_DRAFT_ETAG_INVALID", "Invalid If-Match value")
    return int(match.group(1))


async def _resolve_release_model_authorization(
    *,
    request: Request,
    user: UserContext,
    resolution: dict[str, Any],
    model_id: str,
    provider_id: str,
) -> dict[str, Any]:
    """Return a server-owned model authorization token with no credential data."""

    if os.getenv("ASSISTANT_E2E_STUB_LLM", "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }:
        return build_model_authorization_evidence(
            source="e2e_stub",
            model_id=model_id,
            provider_id=provider_id,
            access_level="public",
            model_enabled=True,
            provider_enabled=True,
            runtime_provider_configured=True,
        )

    resolver = getattr(request.app.state, "agent_runtime_model_resolver", None)
    if resolver is not None:
        from inspect import isawaitable

        requested = resolution.get("spec", {}).get("model", {})
        effective_requested = dict(requested) if isinstance(requested, dict) else {}
        effective_requested.update({"model_id": model_id, "provider_id": provider_id})
        result = resolver.resolve(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            model=effective_requested,
        )
        if isawaitable(result):
            result = await result
        if (
            not isinstance(result, dict)
            or str(result.get("id") or model_id) != model_id
            or str(result.get("provider") or "") != provider_id
        ):
            raise AgentReleaseCandidateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE")
        access_level = str(result.get("access_level") or "public")
        if access_level not in _model_access_levels(user):
            raise AgentReleaseCandidateError("AGENT_RUNTIME_MODEL_FORBIDDEN")
        return build_model_authorization_evidence(
            source="agent_runtime_resolver",
            model_id=model_id,
            provider_id=provider_id,
            access_level=access_level,
            model_enabled=True,
            provider_enabled=True,
            runtime_provider_configured=True,
            model_updated_at=result.get("authorization_version"),
        )

    model_meta = getattr(request.app.state, "model_meta", None)
    model_service = getattr(model_meta, "model_service", None)
    provider_service = getattr(model_meta, "provider_service", None)
    if model_meta is None or model_service is None or provider_service is None:
        raise AgentReleaseCandidateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")
    row = await model_service.get_model(
        user.tenant_id,
        model_id,
        provider_id=provider_id,
    )
    provider = await provider_service.get_provider(user.tenant_id, provider_id)
    configured = await model_meta.is_provider_configured(user.tenant_id, provider_id)
    access_level = str((row or {}).get("access_level") or "public")
    if (
        not row
        or not provider
        or not bool(row.get("is_enabled", True))
        or not bool(provider.get("is_enabled", True))
        or not configured
        or access_level not in _model_access_levels(user)
    ):
        raise AgentReleaseCandidateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")
    return build_model_authorization_evidence(
        source="database",
        model_id=model_id,
        provider_id=provider_id,
        access_level=access_level,
        model_enabled=bool(row.get("is_enabled", True)),
        provider_enabled=bool(provider.get("is_enabled", True)),
        runtime_provider_configured=bool(configured),
        model_updated_at=row.get("updated_at"),
        provider_updated_at=provider.get("updated_at"),
    )


def _model_revalidator(
    *,
    request: Request,
    user: UserContext,
    resolution: dict[str, Any],
    model_id: str,
    provider_id: str,
) -> Any:
    async def revalidate() -> dict[str, Any]:
        return await _resolve_release_model_authorization(
            request=request,
            user=user,
            resolution=resolution,
            model_id=model_id,
            provider_id=provider_id,
        )

    return revalidate


async def _resolve_release_candidate(
    *,
    request: Request,
    user: UserContext,
    repository: Any,
    agent_id: str,
    draft_revision: int,
    channel: str,
    auth_mode: str,
    channel_policy: dict[str, Any],
    dataset_id: str | None,
) -> dict[str, Any]:
    """Resolve trusted release state from the saved Draft and runtime adapters."""

    require_public_release_expiry(auth_mode, channel_policy)
    resolution = await repository.resolve_preview_runtime(
        tenant_id=user.tenant_id,
        agent_id=agent_id,
        user_id=user.user_id,
        is_tenant_admin=_is_tenant_admin(user),
        draft_revision=draft_revision,
    )
    release_resolution = {
        **resolution,
        "publication": {
            "publication_id": None,
            "channel": channel,
            "auth_mode": auth_mode,
            "policy": channel_policy,
        },
    }
    from .agent_runtime import _build_snapshot

    audience_user = _release_audience_user(
        user, agent_id=agent_id, channel=channel, auth_mode=auth_mode
    )
    snapshot = await _build_snapshot(
        request,
        release_resolution,
        audience_user,
        channel=channel,
    )
    snapshot_model = snapshot.get("model") if isinstance(snapshot.get("model"), dict) else {}
    model_authorization = await _resolve_release_model_authorization(
        request=request,
        user=audience_user,
        resolution=resolution,
        model_id=str(snapshot_model.get("id") or ""),
        provider_id=str(snapshot_model.get("provider") or ""),
    )
    dataset_snapshot = None
    if dataset_id:
        dataset_snapshot = await repository.resolve_eval_dataset_snapshot(
            tenant_id=user.tenant_id,
            agent_id=agent_id,
            dataset_id=dataset_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    candidate = build_agent_version_candidate(
        resolution=resolution,
        runtime_snapshot=snapshot,
        channel=channel,
        auth_mode=auth_mode,
        channel_policy=channel_policy,
        dataset_id=dataset_id,
        dataset_snapshot=dataset_snapshot,
        model_authorization=model_authorization,
    )
    candidate["_model_authorization_revalidator"] = _model_revalidator(
        request=request,
        user=audience_user,
        resolution=resolution,
        model_id=str(snapshot_model.get("id") or ""),
        provider_id=str(snapshot_model.get("provider") or ""),
    )
    return candidate


async def _resolve_selected_run_version(
    *,
    request: Request,
    user: UserContext,
    repository: Any,
    agent_id: str,
    run_id: str,
) -> tuple[str, str]:
    """Rebuild the evaluated Version's Preview identity from server-owned state."""

    from ai_gateway_contracts.agent_runtime import runtime_sha256

    from .agent_runtime import _build_snapshot

    run = await _get_trace_repository(request).get_experiment_run(
        tenant_id=user.tenant_id, run_id=run_id
    )
    target = run.get("target_snapshot") if isinstance(run, dict) else None
    target = target if isinstance(target, dict) else {}
    if (
        not run
        or run.get("status") != "succeeded"
        or run.get("run_mode") != "live_candidate"
        or target.get("candidate_type") != "agent_version"
        or str(target.get("agent_id") or "") != agent_id
    ):
        raise AgentReleaseGateError("AGENT_EVAL_RUN_UNAVAILABLE")
    version_id = str(target.get("agent_version_id") or "")
    try:
        uuid.UUID(version_id)
    except ValueError as exc:
        raise AgentReleaseGateError("AGENT_EVAL_RUN_IDENTITY_MISMATCH") from exc
    resolution = await repository.resolve_version_runtime(
        tenant_id=user.tenant_id,
        agent_id=agent_id,
        agent_version_id=version_id,
        user_id=user.user_id,
        is_tenant_admin=_is_tenant_admin(user),
    )
    snapshot = await _build_snapshot(request, resolution, user, channel="preview")
    snapshot_hash = runtime_sha256(snapshot)
    if (
        str(resolution["version"].get("spec_hash") or "")
        != str(target.get("agent_spec_hash") or "")
        or snapshot_hash != str(target.get("agent_runtime_snapshot_hash") or "")
    ):
        raise AgentReleaseGateError("AGENT_EVAL_RUN_IDENTITY_MISMATCH")
    return version_id, snapshot_hash


async def _decorate_release_evaluation_freshness(
    *,
    request: Request,
    user: UserContext,
    repository: Any,
    evaluation: dict[str, Any],
) -> dict[str, Any]:
    """Derive current runtime staleness without mutating immutable evidence."""

    result = dict(evaluation)
    reasons = [str(item) for item in result.get("stale_reasons") or [] if str(item)]
    if result.get("stale") or str(result.get("status") or "") == "stale":
        result["stale"] = True
        result["status"] = "stale"
        result["stale_reasons"] = reasons or ["draft_changed"]
        return result
    if str(result.get("status") or "") != "passed":
        result["stale_reasons"] = reasons
        return result
    try:
        candidate = await _resolve_release_candidate(
            request=request,
            user=user,
            repository=repository,
            agent_id=str(result["agent_id"]),
            draft_revision=int(result["draft_revision"]),
            channel=str(result["channel"]),
            auth_mode=str(result["auth_mode"]),
            channel_policy=dict(result.get("channel_policy") or {}),
            dataset_id=(str(result["dataset_id"]) if result.get("dataset_id") else None),
        )
    except HTTPException as exc:
        detail = exc.detail if isinstance(exc.detail, dict) else {}
        reasons.append(str(detail.get("code") or "runtime_unavailable").lower())
    except (AgentReleaseCandidateError, AgentRepositoryError) as exc:
        reasons.append(str(exc).lower())
    except Exception:  # noqa: BLE001 - readiness uncertainty is stale/fail closed
        reasons.append("runtime_unavailable")
    else:
        comparisons = {
            "runtime_fingerprint_changed": "runtime_fingerprint_hash",
            "release_identity_changed": "release_identity_hash",
            "evaluation_identity_changed": "evaluation_identity_hash",
            "eval_dataset_changed": "dataset_manifest_hash",
        }
        for reason, field in comparisons.items():
            expected = result.get(field)
            actual = candidate.get(field)
            if expected is not None and str(expected) != str(actual):
                reasons.append(reason)
    if reasons:
        result["stale"] = True
        result["status"] = "stale"
    result["stale_reasons"] = sorted(set(reasons))
    return result


def _failed_release_gate(
    *,
    profile: dict[str, Any],
    code: str,
    validation_duration_ms: float,
) -> dict[str, Any]:
    return {
        "schema_version": "agent-release-gate/v1",
        "status": "failed",
        "profile_id": str(profile.get("profile_id") or "unavailable"),
        "profile_version": str(profile.get("profile_version") or "unavailable"),
        "execution_scope": str(profile.get("execution_scope") or "release_integrity"),
        "model_quality_evaluated": False,
        "blocking_findings": [
            {
                "code": code,
                "field": "release_candidate",
                "message": "The server-owned release candidate changed or became unavailable",
            }
        ],
        "non_blocking_findings": [],
        "metrics": {
            "critical_pass_rate": 0.0,
            "configured_critical_pass_rate": float(profile.get("critical_pass_rate") or 1.0),
            "validation_duration_ms": max(0.0, round(validation_duration_ms, 3)),
            "provider_cost_cents": 0.0,
            "evaluator_results": [],
        },
    }


def _map_repository_error(request: Request, exc: Exception) -> None:
    if isinstance(exc, AgentDraftConflictError):
        _raise_agent_error(
            request,
            409,
            "AGENT_DRAFT_CONFLICT",
            "Draft revision is stale",
            current_revision=exc.current_revision,
        )
    if isinstance(exc, AgentLastOwnerError):
        _raise_agent_error(
            request,
            409,
            "AGENT_LAST_OWNER",
            "The last Agent Owner cannot be removed or demoted",
        )
    if isinstance(exc, AgentPrincipalNotFoundError):
        _raise_agent_error(
            request,
            404,
            "AGENT_PRINCIPAL_NOT_FOUND",
            "Principal is not available in this tenant",
        )
    if isinstance(exc, AgentReleaseEvaluationStaleError):
        _raise_agent_error(
            request,
            409,
            "AGENT_EVAL_STALE",
            "The release evaluation no longer matches the current Draft",
            current_revision=exc.current_revision,
        )
    if isinstance(exc, AgentReleaseEvaluationNotFoundError):
        _raise_agent_error(request, 404, "AGENT_EVAL_NOT_FOUND", "Evaluation not found")
    if isinstance(exc, AgentReleaseEvaluationTerminalError):
        _raise_agent_error(
            request,
            409,
            "AGENT_EVAL_TERMINAL",
            "A terminal evaluation cannot be cancelled or executed again",
        )
    if isinstance(exc, AgentPublicationNotFoundError):
        _raise_agent_error(
            request,
            404,
            "AGENT_PUBLICATION_NOT_FOUND",
            "Publication not found",
        )
    if isinstance(exc, AgentReleaseIdempotencyConflictError):
        _raise_agent_error(
            request,
            409,
            "AGENT_RELEASE_IDEMPOTENCY_CONFLICT",
            "The Idempotency-Key was already used for a different release request",
        )
    if isinstance(exc, AgentReleaseGateError):
        _raise_agent_error(
            request,
            409,
            exc.code,
            "The Agent release gate rejected this operation",
            findings=exc.findings,
        )
    if isinstance(exc, AgentRuntimeUnavailableError):
        _raise_agent_error(
            request,
            409,
            "AGENT_EVAL_STALE" if exc.code == "AGENT_PREVIEW_REVISION_STALE" else exc.code,
            "The Agent release candidate is unavailable",
        )
    if isinstance(exc, AgentValidationError):
        _raise_agent_error(
            request,
            422,
            "AGENT_SPEC_INVALID",
            "Agent Draft validation failed",
            errors=exc.errors,
        )
    if isinstance(exc, AgentArchivedError):
        _raise_agent_error(request, 409, "AGENT_ARCHIVED", "Archived Agent is read-only")
    if isinstance(exc, AgentNotFoundError):
        _raise_agent_error(request, 404, "AGENT_NOT_FOUND", "Agent not found")
    if isinstance(exc, AgentRepositoryError):
        code = str(exc)
        if code == "AGENT_CURSOR_INVALID":
            _raise_agent_error(request, 400, code, "Invalid pagination cursor")
        if code in {
            "AGENT_GOVERNANCE_POLICY_INVALID",
            "AGENT_DATA_DELETION_SCOPE_INVALID",
            "AGENT_DATA_DELETION_SUBJECT_INVALID",
        }:
            _raise_agent_error(request, 400, code, "Invalid Agent governance request")
        if code == "AGENT_DATA_DELETION_NOT_FOUND":
            _raise_agent_error(request, 404, code, "Agent data deletion request not found")
        if code in {
            "AGENT_LEGAL_HOLD_CLEANUP_ACTIVE",
            "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID",
            "AGENT_DATA_DELETION_EXECUTION_CLAIM_INVALID",
            "AGENT_DATA_DELETION_EXECUTION_FENCE_LOST",
        }:
            _raise_agent_error(
                request,
                409,
                code,
                "Agent data cleanup execution conflicts with the requested governance change",
            )
        if code in {
            "AGENT_TENANT_AGENT_QUOTA_EXCEEDED",
            "AGENT_ACTIVE_PUBLICATION_QUOTA_EXCEEDED",
        }:
            _raise_agent_error(
                request,
                429,
                code,
                "Agent governance quota exceeded; archive unused resources or raise the configured limit",
            )
        _raise_agent_error(request, 503, "AGENT_STORAGE_UNAVAILABLE", "Agent storage unavailable")
    constraint_name = str(
        getattr(exc, "constraint_name", "") or getattr(exc, "constraint", "") or ""
    )
    if (
        getattr(exc, "sqlstate", None) == "23505"
        and constraint_name == "agent_release_requests_pkey"
    ):
        _raise_agent_error(
            request,
            409,
            "AGENT_RELEASE_IDEMPOTENCY_CONFLICT",
            "The Idempotency-Key was already used for a different release request",
        )
    if (
        getattr(exc, "sqlstate", None) == "23505"
        or exc.__class__.__name__ == "UniqueViolationError"
    ):
        _raise_agent_error(request, 409, "AGENT_SLUG_CONFLICT", "Agent slug already exists")
    raise exc


@router.post("", response_model=AgentMutationResponse, status_code=201, responses=ERROR_RESPONSES)
async def create_agent(
    payload: AgentCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentMutationResponse:
    _require_actor(request, user)
    spec = _materialize_agent_default_model(request, payload.spec.model_dump(mode="python"))
    try:
        agent = await _get_repository(request).create_agent(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            name=payload.name,
            slug=payload.slug,
            description=payload.description,
            spec=spec,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentMutationResponse(request_id=_request_id(request), agent=agent)


@router.get("", response_model=AgentPageResponse, responses=ERROR_RESPONSES)
async def list_agents(
    request: Request,
    limit: Annotated[int, Query(ge=1, le=100)] = 25,
    cursor: str | None = None,
    status: str | None = Query(None, pattern=r"^(draft|active|archived)$"),
    owner_id: str | None = None,
    search: str | None = Query(None, max_length=255),
    channel: str | None = Query(None, pattern=r"^(hosted|embed|api)$"),
    user: UserContext = Depends(get_user_context),
) -> AgentPageResponse:
    _require_actor(request, user)
    try:
        page = await _get_repository(request).list_agents(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            limit=limit,
            cursor=cursor,
            status=status,
            owner_id=owner_id,
            search=search,
            channel=channel,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentPageResponse.model_validate(page)


@router.get("/{agent_id}", response_model=AgentDetail, responses=ERROR_RESPONSES)
async def get_agent(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentDetail:
    _require_actor(request, user)
    try:
        agent = await _get_repository(request).get_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentDetail.model_validate(agent)


@router.get(
    "/{agent_id}/analytics",
    response_model=AgentAnalyticsResponse,
    responses=ERROR_RESPONSES,
)
async def get_agent_analytics(
    agent_id: uuid.UUID,
    request: Request,
    agent_version_id: uuid.UUID | None = None,
    publication_id: uuid.UUID | None = None,
    channel: str | None = Query(None, pattern=r"^(preview|hosted|embed|api|builtin)$"),
    started_after: str | None = None,
    started_before: str | None = None,
    status: str | None = None,
    limit: Annotated[int, Query(ge=1, le=200)] = 50,
    offset: Annotated[int, Query(ge=0)] = 0,
    user: UserContext = Depends(get_user_context),
) -> AgentAnalyticsResponse:
    _require_actor(request, user)
    repository = _get_repository(request)
    try:
        agent = await repository.get_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        trace_repository = _get_trace_repository(request)
        filters = {
            "agent_id": str(agent_id),
            "agent_version_id": str(agent_version_id) if agent_version_id else None,
            "publication_id": str(publication_id) if publication_id else None,
            "channel": channel,
            "started_after": started_after,
            "started_before": started_before,
            "status": status,
        }
        metrics = await trace_repository.get_agent_operations_summary(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            agent_version_id=filters["agent_version_id"],
            publication_id=filters["publication_id"],
            channel=channel,
            started_after=started_after,
            started_before=started_before,
        )
        traces, total = await trace_repository.list_traces(
            tenant_id=user.tenant_id,
            trace_family="assistant",
            agent_id=str(agent_id),
            agent_version_id=filters["agent_version_id"],
            publication_id=filters["publication_id"],
            channel=channel,
            status=status,
            started_after=started_after,
            started_before=started_before,
            limit=limit,
            offset=offset,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    safe_traces: list[dict[str, Any]] = []
    for trace in traces:
        item = dict(trace)
        item["input_preview"] = redact_sensitive_text(
            redact_trace_text(item.get("input_preview"), limit=500)
        )
        item["output_preview"] = redact_sensitive_text(
            redact_trace_text(item.get("output_preview"), limit=500)
        )
        item["metadata"] = {}
        item["privacy"] = {}
        safe_traces.append(item)
    return AgentAnalyticsResponse(
        agent_id=str(agent_id),
        caller_role=str(agent.get("caller_role") or "viewer"),
        metrics=metrics,
        traces=safe_traces,
        total=total,
        limit=limit,
        offset=offset,
        filters={key: value for key, value in filters.items() if value is not None},
    )


@router.get(
    "/{agent_id}/audit-events",
    response_model=AgentAuditPageResponse,
    responses=ERROR_RESPONSES,
)
async def get_agent_audit_events(
    agent_id: uuid.UUID,
    request: Request,
    agent_version_id: uuid.UUID | None = None,
    publication_id: uuid.UUID | None = None,
    channel: str | None = Query(None, pattern=r"^(preview|hosted|embed|api|builtin)$"),
    action: str | None = Query(None, min_length=1, max_length=64),
    started_after: str | None = None,
    started_before: str | None = None,
    limit: Annotated[int, Query(ge=1, le=200)] = 50,
    offset: Annotated[int, Query(ge=0)] = 0,
    user: UserContext = Depends(get_user_context),
) -> AgentAuditPageResponse:
    _require_actor(request, user)
    try:
        rows, total = await _get_repository(request).list_agent_audit_events(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            agent_version_id=str(agent_version_id) if agent_version_id else None,
            publication_id=str(publication_id) if publication_id else None,
            channel=channel,
            action=action,
            started_after=started_after,
            started_before=started_before,
            limit=limit,
            offset=offset,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentAuditPageResponse(
        events=[AgentAuditEventResponse.model_validate(row) for row in rows],
        total=total,
        limit=limit,
        offset=offset,
    )


@router.get(
    "/{agent_id}/governance",
    response_model=AgentGovernancePolicyResponse,
    responses=ERROR_RESPONSES,
)
async def get_agent_governance(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentGovernancePolicyResponse:
    _require_actor(request, user)
    try:
        row = await _get_repository(request).get_governance_policy(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentGovernancePolicyResponse.model_validate(row)


@router.put(
    "/{agent_id}/governance",
    response_model=AgentGovernancePolicyResponse,
    responses=ERROR_RESPONSES,
)
async def update_agent_governance(
    agent_id: uuid.UUID,
    payload: AgentGovernancePolicyUpdate,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentGovernancePolicyResponse:
    _require_actor(request, user)
    try:
        row = await _get_repository(request).update_governance_policy(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            changes=payload.model_dump(exclude_none=True),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentGovernancePolicyResponse.model_validate(row)


@router.post(
    "/{agent_id}/governance/cache:invalidate",
    response_model=AgentCacheInvalidationResponse,
    responses=ERROR_RESPONSES,
)
async def invalidate_agent_cache(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentCacheInvalidationResponse:
    _require_actor(request, user)
    try:
        row = await _get_repository(request).invalidate_agent_caches(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentCacheInvalidationResponse(
        request_id=_request_id(request),
        cache_epoch=int(row["cache_epoch"]),
        deleted_cache_rows=int(row["deleted_cache_rows"]),
    )


@router.post(
    "/{agent_id}/governance/credentials:revoke",
    response_model=AgentCredentialRevocationResponse,
    responses=ERROR_RESPONSES,
)
async def revoke_agent_credentials(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentCredentialRevocationResponse:
    _require_actor(request, user)
    try:
        counts = await _get_repository(request).revoke_agent_credentials(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentCredentialRevocationResponse(request_id=_request_id(request), revoked=counts)


@router.post(
    "/{agent_id}/governance/data-deletions",
    response_model=AgentDataDeletionResponse,
    responses=ERROR_RESPONSES,
)
async def delete_agent_runtime_data(
    agent_id: uuid.UUID,
    payload: AgentDataDeletionRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentDataDeletionResponse:
    _require_actor(request, user)
    repository = _get_repository(request)
    try:
        prepared = await repository.prepare_agent_data_deletion(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            scope=payload.scope,
            subject_user_id=payload.subject_user_id,
            idempotency_key=payload.idempotency_key,
        )
        if prepared["status"] not in {"pending", "failed"}:
            return AgentDataDeletionResponse.model_validate(prepared)
        async with repository.claim_agent_data_deletion_execution(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            deletion_id=str(prepared["deletion_id"]),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        ) as claimed:
            if not claimed.get("execution_claimed"):
                return AgentDataDeletionResponse.model_validate(claimed)
            claimed.pop("_execution_claim_token")
            claimed.pop("_execution_generation")
            execution_guard = claimed.pop("_execution_guard")
            freeze_execution_inventory = claimed.pop("_execution_freeze_inventory")
            finish_execution = claimed.pop("_execution_finish")
            prepared = claimed
            object_keys = prepared.get("object_keys") or []
            storage_ok = not object_keys
            if object_keys:
                try:
                    storage = get_file_storage()
                except RuntimeError:
                    storage = None
                storage_ok = storage is not None
                if storage is not None:
                    for storage_key in object_keys:
                        await execution_guard()
                        try:
                            await storage.delete_file(str(storage_key))
                        except Exception:
                            storage_ok = False
                        exists = getattr(storage, "file_exists", None)
                        if not callable(exists):
                            storage_ok = False
                            continue
                        await execution_guard()
                        try:
                            object_exists = await exists(str(storage_key))
                        except Exception:
                            storage_ok = False
                            continue
                        if object_exists is not False:
                            storage_ok = False
            runtime_cleanup_receipt: dict[str, Any] | None = None
            if storage_ok:
                counts = prepared.get("deleted_counts") or {}
                if isinstance(counts, str):
                    counts = json.loads(counts)
                plan = counts.get("runtime_cleanup_plan")
                inventory = counts.get("runtime_cleanup_inventory")
                try:
                    cleanup_client = _get_runtime_cleanup_client(request)
                    if inventory is None:
                        inventory = await cleanup_client.inspect(plan)
                        prepared = await freeze_execution_inventory(
                            inventory=inventory,
                        )
                        frozen_counts = prepared.get("deleted_counts") or {}
                        if isinstance(frozen_counts, str):
                            frozen_counts = json.loads(frozen_counts)
                        plan = frozen_counts.get("runtime_cleanup_plan")
                        inventory = frozen_counts.get("runtime_cleanup_inventory")
                    await execution_guard()
                    runtime_cleanup_receipt = await cleanup_client.execute(
                        plan_value=plan,
                        inventory_value=inventory,
                    )
                except (AgentRuntimeCleanupClientError, TypeError, ValueError):
                    runtime_cleanup_receipt = None
            result = await finish_execution(
                storage_cleanup_succeeded=storage_ok,
                runtime_cleanup_receipt=runtime_cleanup_receipt,
            )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentDataDeletionResponse.model_validate(result)


@router.patch("/{agent_id}", response_model=AgentMutationResponse, responses=ERROR_RESPONSES)
async def update_agent(
    agent_id: uuid.UUID,
    payload: AgentUpdateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentMutationResponse:
    _require_actor(request, user)
    try:
        agent = await _get_repository(request).update_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            changes=payload.model_dump(exclude_unset=True),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentMutationResponse(request_id=_request_id(request), agent=agent)


@router.delete("/{agent_id}", response_model=AgentStatusResponse, responses=ERROR_RESPONSES)
async def delete_agent(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentStatusResponse:
    _require_actor(request, user)
    try:
        await _get_repository(request).soft_delete_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentStatusResponse(request_id=_request_id(request), status="deleted")


@router.post(
    "/{agent_id}/copy",
    response_model=AgentMutationResponse,
    status_code=201,
    responses=ERROR_RESPONSES,
)
async def copy_agent(
    agent_id: uuid.UUID,
    payload: AgentCopyRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentMutationResponse:
    _require_actor(request, user)
    try:
        agent = await _get_repository(request).copy_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            name=payload.name,
            slug=payload.slug,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentMutationResponse(request_id=_request_id(request), agent=agent)


@router.post("/{agent_id}/archive", response_model=AgentMutationResponse, responses=ERROR_RESPONSES)
async def archive_agent(
    agent_id: uuid.UUID,
    payload: AgentArchiveRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentMutationResponse:
    _require_actor(request, user)
    repository = _get_repository(request)
    try:
        await repository.archive_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            disable_publications=payload.disable_publications,
        )
        agent = await repository.get_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentMutationResponse(request_id=_request_id(request), agent=agent)


@router.get("/{agent_id}/draft", response_model=AgentDraftResponse, responses=ERROR_RESPONSES)
async def get_agent_draft(
    agent_id: uuid.UUID,
    request: Request,
    response: Response,
    user: UserContext = Depends(get_user_context),
) -> AgentDraftResponse:
    _require_actor(request, user)
    try:
        draft = await _get_repository(request).get_draft(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    response.headers["ETag"] = f'"{draft["revision"]}"'
    return AgentDraftResponse.model_validate(draft)


@router.put(
    "/{agent_id}/draft", response_model=AgentDraftMutationResponse, responses=ERROR_RESPONSES
)
async def update_agent_draft(
    agent_id: uuid.UUID,
    payload: AgentDraftUpdateRequest,
    request: Request,
    response: Response,
    if_match: Annotated[str | None, Header(alias="If-Match")] = None,
    user: UserContext = Depends(get_user_context),
) -> AgentDraftMutationResponse:
    _require_actor(request, user)
    expected_revision = _parse_etag(request, if_match)
    spec = _materialize_agent_default_model(request, payload.spec.model_dump(mode="python"))
    agent_changes = {
        key: value
        for key, value in payload.model_dump(
            mode="python",
            include={"name", "description"},
            exclude_unset=True,
        ).items()
        if value is not None
    }
    try:
        draft = await _get_repository(request).update_draft(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            expected_revision=expected_revision,
            spec=spec,
            agent_changes=agent_changes,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    response.headers["ETag"] = f'"{draft["revision"]}"'
    return AgentDraftMutationResponse(request_id=_request_id(request), draft=draft)


@router.post(
    "/{agent_id}/validate", response_model=AgentValidationResponse, responses=ERROR_RESPONSES
)
async def validate_agent_draft(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentValidationResponse:
    _require_actor(request, user)
    try:
        result = await _get_repository(request).validate_draft(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentValidationResponse.model_validate(result)


@router.post(
    "/{agent_id}/evals",
    response_model=AgentReleaseEvaluationResponse,
    status_code=201,
    responses=ERROR_RESPONSES,
)
async def run_agent_release_evaluation(
    agent_id: uuid.UUID,
    payload: AgentReleaseEvaluationRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseEvaluationResponse:
    _require_actor(request, user)
    _require_publish_mutation(request)
    repository = _get_repository(request)
    try:
        profile = require_available_release_profile()
        candidate = await _resolve_release_candidate(
            request=request,
            user=user,
            repository=repository,
            agent_id=str(agent_id),
            draft_revision=payload.draft_revision,
            channel=payload.channel,
            auth_mode=payload.auth_mode,
            channel_policy=payload.channel_policy.model_dump(mode="json"),
            dataset_id=str(payload.dataset_id) if payload.dataset_id else None,
        )
        result = await repository.create_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            candidate=candidate,
            profile=profile,
            actor_model_access_levels=_model_access_levels(user),
            model_authorization_revalidator=candidate.get("_model_authorization_revalidator"),
        )
    except AgentReleaseProfileUnavailableError:
        _raise_agent_error(
            request,
            503,
            "AGENT_RELEASE_PROFILE_UNAVAILABLE",
            "The server release profile is not configured",
        )
    except AgentReleaseCandidateError as exc:
        _raise_agent_error(request, 409, str(exc), "Release candidate validation failed")
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseEvaluationResponse.model_validate(result)


@router.post(
    "/{agent_id}/evals/{evaluation_id}/execute",
    response_model=AgentReleaseEvaluationResponse,
    responses=ERROR_RESPONSES,
)
async def execute_agent_release_evaluation(
    agent_id: uuid.UUID,
    evaluation_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseEvaluationResponse:
    _require_actor(request, user)
    _require_publish_mutation(request)
    repository = _get_repository(request)
    started = time.perf_counter()
    try:
        running = await repository.start_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        if not running.get("execution_claimed"):
            return AgentReleaseEvaluationResponse.model_validate(running)
        profile = require_available_release_profile()
        if str(profile["profile_id"]) != str(running["profile_id"]) or str(
            profile["profile_version"]
        ) != str(running["profile_version"]):
            gate = _failed_release_gate(
                profile=profile,
                code="AGENT_RELEASE_PROFILE_STALE",
                validation_duration_ms=(time.perf_counter() - started) * 1000,
            )
            candidate: dict[str, Any] = {}
        else:
            try:
                candidate = await _resolve_release_candidate(
                    request=request,
                    user=user,
                    repository=repository,
                    agent_id=str(agent_id),
                    draft_revision=int(running["draft_revision"]),
                    channel=str(running["channel"]),
                    auth_mode=str(running["auth_mode"]),
                    channel_policy=dict(running.get("channel_policy") or {}),
                    dataset_id=(str(running["dataset_id"]) if running.get("dataset_id") else None),
                )
                expected = {
                    "runtime_fingerprint_hash": str(running["runtime_fingerprint_hash"]),
                    "release_identity_hash": str(running["release_identity_hash"]),
                    "evaluation_identity_hash": str(
                        running.get("evaluation_identity_hash") or running["release_identity_hash"]
                    ),
                    "dataset_manifest_hash": (
                        str(running["dataset_manifest_hash"])
                        if running.get("dataset_manifest_hash")
                        else None
                    ),
                }
                if any(candidate.get(key) != value for key, value in expected.items()):
                    raise AgentReleaseCandidateError("AGENT_EVAL_STALE")
                gate = evaluate_agent_version_candidate(
                    candidate,
                    profile=profile,
                    validation_duration_ms=(time.perf_counter() - started) * 1000,
                )
            except HTTPException as exc:
                detail = exc.detail if isinstance(exc.detail, dict) else {}
                candidate = {}
                gate = _failed_release_gate(
                    profile=profile,
                    code=str(detail.get("code") or "AGENT_RUNTIME_UNAVAILABLE"),
                    validation_duration_ms=(time.perf_counter() - started) * 1000,
                )
            except (AgentReleaseCandidateError, AgentRepositoryError) as exc:
                candidate = {}
                gate = _failed_release_gate(
                    profile=profile,
                    code=str(exc),
                    validation_duration_ms=(time.perf_counter() - started) * 1000,
                )
        result = await repository.complete_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            candidate=candidate,
            gate=gate,
            actor_model_access_levels=_model_access_levels(user),
            model_authorization_revalidator=candidate.get("_model_authorization_revalidator"),
        )
    except AgentReleaseProfileUnavailableError:
        profile = {
            "profile_id": str(
                running.get("profile_id") if "running" in locals() else "unavailable"
            ),
            "profile_version": str(
                running.get("profile_version") if "running" in locals() else "unavailable"
            ),
            "critical_pass_rate": 1.0,
        }
        gate = _failed_release_gate(
            profile=profile,
            code="AGENT_RELEASE_PROFILE_UNAVAILABLE",
            validation_duration_ms=(time.perf_counter() - started) * 1000,
        )
        try:
            result = await repository.complete_release_evaluation(
                tenant_id=user.tenant_id,
                agent_id=str(agent_id),
                evaluation_id=str(evaluation_id),
                user_id=user.user_id,
                is_tenant_admin=_is_tenant_admin(user),
                candidate={},
                gate=gate,
                actor_model_access_levels=_model_access_levels(user),
            )
        except Exception as exc:
            _map_repository_error(request, exc)
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseEvaluationResponse.model_validate(result)


@router.post(
    "/{agent_id}/evals/{evaluation_id}/cancel",
    response_model=AgentReleaseEvaluationResponse,
    responses=ERROR_RESPONSES,
)
async def cancel_agent_release_evaluation(
    agent_id: uuid.UUID,
    evaluation_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseEvaluationResponse:
    _require_actor(request, user)
    _require_publish_mutation(request)
    try:
        result = await _get_repository(request).cancel_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseEvaluationResponse.model_validate(result)


@router.get(
    "/{agent_id}/evals",
    response_model=AgentReleaseEvaluationListResponse,
    responses=ERROR_RESPONSES,
)
async def list_agent_release_evaluations(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseEvaluationListResponse:
    _require_actor(request, user)
    repository = _get_repository(request)
    try:
        evaluations = await repository.list_release_evaluations(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        evaluations = [
            await _decorate_release_evaluation_freshness(
                request=request,
                user=user,
                repository=repository,
                evaluation=evaluation,
            )
            for evaluation in evaluations
        ]
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseEvaluationListResponse(evaluations=evaluations)


@router.get(
    "/{agent_id}/evals/{evaluation_id}",
    response_model=AgentReleaseEvaluationResponse,
    responses=ERROR_RESPONSES,
)
async def get_agent_release_evaluation(
    agent_id: uuid.UUID,
    evaluation_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseEvaluationResponse:
    _require_actor(request, user)
    repository = _get_repository(request)
    try:
        result = await repository.get_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        result = await _decorate_release_evaluation_freshness(
            request=request,
            user=user,
            repository=repository,
            evaluation=result,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseEvaluationResponse.model_validate(result)


@router.get(
    "/{agent_id}/evals/{evaluation_id}/diff",
    response_model=AgentReleaseDiffResponse,
    responses=ERROR_RESPONSES,
)
async def get_agent_release_diff(
    agent_id: uuid.UUID,
    evaluation_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseDiffResponse:
    _require_actor(request, user)
    try:
        result = await _get_repository(request).get_release_diff(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseDiffResponse.model_validate(result)


@router.post(
    "/{agent_id}/publish",
    response_model=AgentReleaseMutationResponse,
    responses=ERROR_RESPONSES,
)
async def publish_agent(
    agent_id: uuid.UUID,
    payload: AgentPublishRequest,
    request: Request,
    idempotency_key: Annotated[str | None, Header(alias="Idempotency-Key")] = None,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseMutationResponse:
    _require_actor(request, user)
    _require_publish_mutation(request)
    key = _parse_idempotency_key(request, idempotency_key)
    repository = _get_repository(request)
    try:
        replay = await repository.replay_release_request(
            tenant_id=user.tenant_id,
            operation="promote",
            idempotency_key=key,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            reason=payload.reason,
            evaluation_id=str(payload.evaluation_id),
            experiment_run_id=(str(payload.experiment_run_id) if payload.experiment_run_id else None),
        )
        if replay is not None:
            return AgentReleaseMutationResponse(request_id=_request_id(request), **replay)
        evaluation = await repository.get_release_evaluation(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(payload.evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            required_role="owner",
        )
        if evaluation.get("stale") or evaluation.get("status") == "stale":
            raise AgentReleaseEvaluationStaleError(int(evaluation["draft_revision"]))
        candidate = await _resolve_release_candidate(
            request=request,
            user=user,
            repository=repository,
            agent_id=str(agent_id),
            draft_revision=int(evaluation["draft_revision"]),
            channel=str(evaluation["channel"]),
            auth_mode=str(evaluation["auth_mode"]),
            channel_policy=dict(evaluation.get("channel_policy") or {}),
            dataset_id=(str(evaluation["dataset_id"]) if evaluation.get("dataset_id") else None),
        )
        selected_version_id = None
        selected_version_snapshot_hash = None
        if payload.experiment_run_id is not None:
            selected_version_id, selected_version_snapshot_hash = await _resolve_selected_run_version(
                request=request,
                user=user,
                repository=repository,
                agent_id=str(agent_id),
                run_id=str(payload.experiment_run_id),
            )
        result = await repository.publish_agent(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            evaluation_id=str(payload.evaluation_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            idempotency_key=key,
            reason=payload.reason,
            experiment_run_id=(str(payload.experiment_run_id) if payload.experiment_run_id else None),
            selected_version_id=selected_version_id,
            selected_version_snapshot_hash=selected_version_snapshot_hash,
            current_candidate=candidate,
            actor_model_access_levels=_model_access_levels(user),
            model_authorization_revalidator=candidate.get("_model_authorization_revalidator"),
        )
    except AgentReleaseCandidateError as exc:
        _raise_agent_error(request, 409, str(exc), "Release candidate validation failed")
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseMutationResponse(request_id=_request_id(request), **result)


@router.get(
    "/{agent_id}/publications",
    response_model=list[AgentPublicationResponse],
    responses=ERROR_RESPONSES,
)
async def list_agent_publications(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> list[AgentPublicationResponse]:
    _require_actor(request, user)
    try:
        rows = await _get_repository(request).list_publications(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return [AgentPublicationResponse.model_validate(row) for row in rows]


@router.get(
    "/{agent_id}/publish-events",
    response_model=list[AgentPublishEventResponse],
    responses=ERROR_RESPONSES,
)
async def list_agent_publish_events(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> list[AgentPublishEventResponse]:
    _require_actor(request, user)
    try:
        rows = await _get_repository(request).list_publish_events(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return [AgentPublishEventResponse.model_validate(row) for row in rows]


@router.post(
    "/{agent_id}/versions",
    response_model=AgentVersionMutationResponse,
    status_code=201,
    responses=ERROR_RESPONSES,
)
async def create_agent_version(
    agent_id: uuid.UUID,
    request: Request,
    if_match: Annotated[str | None, Header(alias="If-Match")] = None,
    user: UserContext = Depends(get_user_context),
) -> AgentVersionMutationResponse:
    _require_actor(request, user)
    expected_revision = _parse_etag(request, if_match)
    try:
        version = await _get_repository(request).create_version(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            expected_revision=expected_revision,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentVersionMutationResponse(request_id=_request_id(request), version=version)


@router.get(
    "/{agent_id}/versions",
    response_model=list[AgentVersionResponse],
    responses=ERROR_RESPONSES,
)
async def list_agent_versions(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> list[AgentVersionResponse]:
    _require_actor(request, user)
    try:
        versions = await _get_repository(request).list_versions(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return [AgentVersionResponse.model_validate(version) for version in versions]


@router.get(
    "/{agent_id}/members",
    response_model=list[AgentMemberResponse],
    responses=ERROR_RESPONSES,
)
async def list_agent_members(
    agent_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> list[AgentMemberResponse]:
    _require_actor(request, user)
    try:
        members = await _get_repository(request).list_members(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return [AgentMemberResponse.model_validate(member) for member in members]


@router.put(
    "/{agent_id}/members/{principal_type}/{principal_id}",
    response_model=AgentMemberMutationResponse,
    responses=ERROR_RESPONSES,
)
async def upsert_agent_member(
    agent_id: uuid.UUID,
    principal_type: str,
    principal_id: str,
    payload: AgentMemberRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentMemberMutationResponse:
    _require_actor(request, user)
    if principal_type not in {"user", "group"}:
        _raise_agent_error(request, 400, "AGENT_PRINCIPAL_INVALID", "Invalid principal type")
    try:
        member = await _get_repository(request).upsert_member(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            principal_type=principal_type,
            principal_id=principal_id,
            role=payload.role,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentMemberMutationResponse(request_id=_request_id(request), member=member)


@router.delete(
    "/{agent_id}/members/{principal_type}/{principal_id}",
    response_model=AgentStatusResponse,
    responses=ERROR_RESPONSES,
)
async def delete_agent_member(
    agent_id: uuid.UUID,
    principal_type: str,
    principal_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentStatusResponse:
    _require_actor(request, user)
    if principal_type not in {"user", "group"}:
        _raise_agent_error(request, 400, "AGENT_PRINCIPAL_INVALID", "Invalid principal type")
    try:
        await _get_repository(request).remove_member(
            tenant_id=user.tenant_id,
            agent_id=str(agent_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            principal_type=principal_type,
            principal_id=principal_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentStatusResponse(request_id=_request_id(request), status="deleted")


@publication_router.post(
    "/{publication_id}/tokens",
    response_model=AgentApiTokenIssueResponse,
    status_code=201,
    responses=ERROR_RESPONSES,
)
async def create_agent_api_token(
    publication_id: uuid.UUID,
    payload: AgentApiTokenCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentApiTokenIssueResponse:
    _require_actor(request, user)
    try:
        raw_token, row = await _get_repository(request).create_api_token(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            user_id=user.user_id,
            name=payload.name,
            scopes=list(payload.scopes),
            expires_at=payload.expires_at,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentApiTokenIssueResponse(
        token=raw_token,
        token_metadata=AgentApiTokenMetadata.model_validate(row),
        request_id=_request_id(request),
    )


@publication_router.get(
    "/{publication_id}/tokens",
    response_model=AgentApiTokenListResponse,
    responses=ERROR_RESPONSES,
)
async def list_agent_api_tokens(
    publication_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentApiTokenListResponse:
    _require_actor(request, user)
    try:
        rows = await _get_repository(request).list_api_tokens(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentApiTokenListResponse(
        tokens=[AgentApiTokenMetadata.model_validate(row) for row in rows],
        request_id=_request_id(request),
    )


@publication_router.post(
    "/{publication_id}/tokens/{token_id}/rotate",
    response_model=AgentApiTokenIssueResponse,
    status_code=201,
    responses=ERROR_RESPONSES,
)
async def rotate_agent_api_token(
    publication_id: uuid.UUID,
    token_id: uuid.UUID,
    payload: AgentApiTokenRotateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentApiTokenIssueResponse:
    _require_actor(request, user)
    try:
        raw_token, row = await _get_repository(request).rotate_api_token(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            token_id=str(token_id),
            user_id=user.user_id,
            name=payload.name,
            scopes=list(payload.scopes) if payload.scopes is not None else None,
            expires_at=payload.expires_at,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentApiTokenIssueResponse(
        token=raw_token,
        token_metadata=AgentApiTokenMetadata.model_validate(row),
        request_id=_request_id(request),
    )


@publication_router.delete(
    "/{publication_id}/tokens/{token_id}",
    response_model=AgentApiTokenMetadata,
    responses=ERROR_RESPONSES,
)
async def revoke_agent_api_token(
    publication_id: uuid.UUID,
    token_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentApiTokenMetadata:
    _require_actor(request, user)
    try:
        row = await _get_repository(request).revoke_api_token(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            token_id=str(token_id),
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentApiTokenMetadata.model_validate(row)


@publication_router.post(
    "/{publication_id}/rollback",
    response_model=AgentReleaseMutationResponse,
    responses=ERROR_RESPONSES,
)
async def rollback_agent_publication(
    publication_id: uuid.UUID,
    payload: AgentRollbackRequest,
    request: Request,
    idempotency_key: Annotated[str | None, Header(alias="Idempotency-Key")] = None,
    user: UserContext = Depends(get_user_context),
) -> AgentReleaseMutationResponse:
    _require_actor(request, user)
    _require_publish_mutation(request)
    key = _parse_idempotency_key(request, idempotency_key)
    repository = _get_repository(request)
    try:
        publication = await repository.get_publication(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            required_role="owner",
        )
        replay = await repository.replay_release_request(
            tenant_id=user.tenant_id,
            operation="rollback",
            idempotency_key=key,
            agent_id=str(publication["agent_id"]),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            reason=payload.reason,
            publication_id=str(publication_id),
            target_version_id=str(payload.target_version_id),
        )
        if replay is not None:
            return AgentReleaseMutationResponse(request_id=_request_id(request), **replay)
        resolution = await repository.resolve_version_runtime(
            tenant_id=user.tenant_id,
            agent_id=str(publication["agent_id"]),
            agent_version_id=str(payload.target_version_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        resolution["publication"] = publication
        from ai_gateway_contracts.agent_runtime import runtime_sha256

        from .agent_runtime import _build_snapshot

        audience_user = _release_audience_user(
            user,
            agent_id=str(publication["agent_id"]),
            channel=str(publication["channel"]),
            auth_mode=str(publication["auth_mode"]),
        )
        snapshot = await _build_snapshot(
            request,
            resolution,
            audience_user,
            channel=str(publication["channel"]),
        )
        snapshot_model = snapshot.get("model") if isinstance(snapshot.get("model"), dict) else {}
        model_authorization = await _resolve_release_model_authorization(
            request=request,
            user=audience_user,
            resolution=resolution,
            model_id=str(snapshot_model.get("id") or ""),
            provider_id=str(snapshot_model.get("provider") or ""),
        )
        version = resolution["version"]
        build_agent_version_candidate(
            resolution={
                **resolution,
                "draft": {
                    "draft_id": version["source_draft_id"],
                    "revision": version["source_draft_revision"],
                    "spec_hash": version["spec_hash"],
                },
            },
            runtime_snapshot=snapshot,
            channel=str(publication["channel"]),
            auth_mode=str(publication["auth_mode"]),
            channel_policy=dict(publication.get("policy") or {}),
            dataset_id=None,
            model_authorization=model_authorization,
        )
        runtime_snapshot_hash = runtime_sha256(snapshot).removeprefix("sha256:")
        runtime_spec_hash = str(snapshot["fingerprints"]["spec"]).removeprefix("sha256:")
        result = await repository.rollback_publication(
            tenant_id=user.tenant_id,
            publication_id=str(publication_id),
            target_version_id=str(payload.target_version_id),
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            idempotency_key=key,
            reason=payload.reason,
            runtime_snapshot_hash=runtime_snapshot_hash,
            runtime_spec_hash=runtime_spec_hash,
            model_authorization=model_authorization,
            actor_model_access_levels=_model_access_levels(user),
            model_authorization_revalidator=_model_revalidator(
                request=request,
                user=audience_user,
                resolution=resolution,
                model_id=str(snapshot_model.get("id") or ""),
                provider_id=str(snapshot_model.get("provider") or ""),
            ),
        )
    except AgentReleaseCandidateError as exc:
        _raise_agent_error(request, 409, str(exc), "Release candidate validation failed")
    except Exception as exc:
        _map_repository_error(request, exc)
    return AgentReleaseMutationResponse(request_id=_request_id(request), **result)


__all__ = ["publication_router", "router"]
