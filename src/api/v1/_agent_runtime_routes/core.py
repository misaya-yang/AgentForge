"""Shared request/auth/repository plumbing for the Agent Studio Runtime routes.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; the facade
keeps time-limited re-exports for pre-split import paths.
"""

from __future__ import annotations

import os
import uuid
from typing import Any

from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentRepositoryError,
    AgentRuntimeUnavailableError,
    DatabaseAgentRepository,
)
from fastapi import HTTPException, Request

from ....core.auth.user_resolver import UserContext


def _runtime_enabled() -> bool:
    return os.getenv("AGENT_STUDIO_RUNTIME_ENABLED", "true").strip().lower() in {
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


def _raise_runtime_error(
    request: Request,
    status_code: int,
    code: str,
    message: str,
) -> None:
    raise HTTPException(
        status_code=status_code,
        detail={"code": code, "message": message, "request_id": _request_id(request)},
    )


def _require_actor(request: Request, user: UserContext) -> None:
    if not _runtime_enabled():
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_DISABLED",
            "Agent runtime is disabled",
        )
    if not user.is_authenticated or not user.user_id:
        _raise_runtime_error(
            request,
            401,
            "AUTHENTICATION_REQUIRED",
            "Authentication required",
        )
    if not user.tenant_id or user.tenant_id == "public":
        _raise_runtime_error(request, 403, "TENANT_REQUIRED", "Tenant identity required")


def _is_tenant_admin(user: UserContext) -> bool:
    roles = {str(role).lower() for role in (user.roles or [])}
    return bool(roles & {"admin", "tenant_admin"}) or str(user.tier).lower() == "admin"


def _repository(request: Request) -> Any:
    repository = getattr(request.app.state, "agent_repository", None)
    if repository is not None:
        return repository
    database = getattr(request.app.state, "database", None)
    if database is None:
        _raise_runtime_error(
            request,
            503,
            "AGENT_STORAGE_UNAVAILABLE",
            "Agent storage unavailable",
        )
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


def _session_manager(request: Request) -> Any:
    manager = getattr(request.app.state, "session_manager", None)
    if manager is None or not hasattr(manager, "bind_agent_runtime"):
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_SESSION_STORAGE_UNAVAILABLE",
            "Agent session storage unavailable",
        )
    return manager


def _prefixed_hash(value: Any) -> str:
    raw = str(value or "")
    return raw if raw.startswith("sha256:") else f"sha256:{raw}"


def _bearer_token(request: Request) -> str:
    authorization = request.headers.get("Authorization", "")
    if not authorization.startswith("Bearer "):
        _raise_runtime_error(
            request,
            401,
            "AGENT_RUNTIME_TOKEN_REQUIRED",
            "Runtime API token required",
        )
    token = authorization[7:].strip()
    if not token.startswith("agt_"):
        _raise_runtime_error(request, 401, "AGENT_RUNTIME_TOKEN_INVALID", "Invalid Runtime API token")
    return token


def _token_user(resolution: dict[str, Any], request: Request) -> UserContext:
    token = resolution["api_token"]
    return UserContext(
        user_id=f"agent-token:{token['token_id']}",
        tenant_id=str(resolution["agent"]["tenant_id"]),
        tier="service",
        is_authenticated=True,
        ip=str(getattr(request.client, "host", "") or ""),
        roles=["agent_runtime"],
        user_type="service",
    )


def _runtime_body(
    *,
    message: str,
    session_id: str,
    attachments: list[dict[str, Any]],
    resume_run_id: str | None = None,
    resume_approval_id: str | None = None,
) -> dict[str, Any]:
    body = {
        "message": message,
        "session_id": session_id,
        "history": None,
        "attachments": attachments,
    }
    if resume_run_id is not None:
        body.update(
            {
                "resume_run_id": resume_run_id,
                "resume_approval_id": resume_approval_id,
            }
        )
    return body


def _map_repository_error(request: Request, exc: Exception) -> None:
    if isinstance(exc, AgentNotFoundError):
        _raise_runtime_error(request, 404, "AGENT_NOT_FOUND", "Agent not found")
    if isinstance(exc, AgentRuntimeUnavailableError):
        if exc.code in {"AGENT_RUNTIME_TOKEN_INVALID", "PUBLICATION_AUTHENTICATION_REQUIRED"}:
            status = 401
        elif exc.code in {"AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN", "PUBLICATION_ACCESS_DENIED"}:
            status = 403 if exc.code.endswith("SCOPE_FORBIDDEN") else 404
        elif exc.code in {"AGENT_RUNTIME_SESSION_NOT_FOUND"}:
            status = 404
        elif exc.code in {
            "PUBLICATION_DISABLED",
            "AGENT_VERSION_REVOKED",
            "AGENT_RUNTIME_IDEMPOTENCY_CONFLICT",
            "AGENT_RUNTIME_IDEMPOTENCY_STATE_INVALID",
            "AGENT_RUNTIME_ATTACHMENT_DUPLICATE",
            "PUBLICATION_CHANNEL_MISMATCH",
        }:
            status = 409
        elif exc.code == "AGENT_RUNTIME_ATTACHMENT_NOT_FOUND":
            status = 404
        elif exc.code == "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED":
            _raise_runtime_error(
                request,
                413,
                exc.code,
                "Agent attachment storage quota exceeded; delete attachments or raise the governance limit",
            )
        elif exc.code in {
            "AGENT_RUNTIME_CONCURRENCY_QUOTA_EXCEEDED",
            "AGENT_RUNTIME_TOKEN_QUOTA_EXCEEDED",
            "AGENT_RUNTIME_MCP_QUOTA_EXCEEDED",
        }:
            _raise_runtime_error(
                request,
                429,
                exc.code,
                "Agent runtime quota exceeded; retry after usage falls or raise the governance limit",
            )
        else:
            status = 422
        _raise_runtime_error(request, status, exc.code, "Agent runtime is unavailable")
    if isinstance(exc, AgentRepositoryError):
        _raise_runtime_error(
            request,
            503,
            "AGENT_STORAGE_UNAVAILABLE",
            "Agent storage unavailable",
        )
    raise exc


async def _resolve_api_caller(
    request: Request,
    *,
    publication_id: str,
    required_scopes: list[str],
    pinned_version_id: str | None = None,
) -> tuple[dict[str, Any], UserContext]:
    raw_token = _bearer_token(request)
    try:
        resolution = await _repository(request).resolve_api_token_runtime(
            raw_token=raw_token,
            publication_id=publication_id,
            required_scopes=required_scopes,
            pinned_version_id=pinned_version_id,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    return resolution, _token_user(resolution, request)
