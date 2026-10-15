"""Public Hosted and origin-bound Embed delivery for Agent Publications."""

from __future__ import annotations

import base64
import hashlib
import hmac
import html
import json
import os
import secrets
import time
import uuid
from dataclasses import replace
from typing import Any
from urllib.parse import urlsplit

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentRepositoryError,
)
from fastapi import APIRouter, Depends, File, Request, UploadFile
from fastapi.responses import HTMLResponse

from ...core.auth.user_resolver import UserContext
from ...core.client_ip import get_client_ip_from_request
from ..deps import get_user_context
from ..schemas.agent_runtime import (
    AgentPublicChatRequest,
    AgentPublicConfigResponse,
    AgentPublicSessionRequest,
    AgentRuntimeAttachmentUploadResponse,
    AgentRuntimeFeedbackRequest,
    AgentRuntimeFeedbackResponse,
    AgentRuntimeSessionResponse,
)
from ._agent_runtime_headers import reject_client_agent_forgery
from .agent_runtime import (
    _assert_attachments_allowed,
    _assert_existing_pin,
    _bind_session,
    _build_snapshot,
    _enforce_channel_limits,
    _existing_session,
    _is_tenant_admin,
    _map_repository_error,
    _raise_runtime_error,
    _repository,
    _request_id,
    _resolve_runtime_attachments,
    _runtime_body,
    _start_runtime_stream,
    _store_runtime_attachment,
)

router = APIRouter(prefix="/public/agents", tags=["Agent Studio Public Runtime"])
document_router = APIRouter(tags=["Agent Studio Embed"])

_EMBED_TOKEN_TTL_SECONDS = 300
_EMBED_PROTOCOL_VERSION = "agent-embed/v1"


def _b64encode(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).decode("ascii").rstrip("=")


def _b64decode(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def _embed_secret(request: Request) -> bytes:
    value = os.getenv("AGENT_RUNTIME_TOKEN_SIGNING_KEY", "").strip() or os.getenv(
        "GATEWAY_ENCRYPTION_KEY", ""
    ).strip()
    if not value:
        _raise_runtime_error(
            request,
            503,
            "AGENT_EMBED_SIGNING_UNAVAILABLE",
            "Embed signing is unavailable",
        )
    return value.encode("utf-8")


def _normalized_origin(value: str) -> str | None:
    try:
        parsed = urlsplit(value.strip())
    except ValueError:
        return None
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or parsed.username
        or parsed.password
    ):
        return None
    return f"{parsed.scheme}://{parsed.netloc}".rstrip("/")


def _parent_origin(request: Request) -> str | None:
    origin = _normalized_origin(request.headers.get("Origin", ""))
    if origin:
        return origin
    return _normalized_origin(request.headers.get("Referer", ""))


def _allowed_origins(descriptor: dict[str, Any]) -> list[str]:
    policy = descriptor.get("policy") if isinstance(descriptor.get("policy"), dict) else {}
    normalized = sorted(
        {
            origin
            for raw in policy.get("allowed_origins", [])
            if isinstance(raw, str) and (origin := _normalized_origin(raw)) is not None
        }
    )
    if any("*" in origin for origin in normalized):
        return []
    return normalized


def _issue_embed_token(
    request: Request,
    *,
    public_id: str,
    origin: str,
    nonce: str,
) -> str:
    abuse_identity = hmac.new(
        _embed_secret(request),
        f"{public_id}\n{origin}\n{get_client_ip_from_request(request)}".encode(),
        hashlib.sha256,
    ).digest()[:18]
    payload = {
        "v": _EMBED_PROTOCOL_VERSION,
        "public_id": public_id,
        "origin": origin,
        "nonce": nonce,
        "sub": _b64encode(abuse_identity),
        "exp": int(time.time()) + _EMBED_TOKEN_TTL_SECONDS,
    }
    encoded = _b64encode(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    signature = hmac.new(_embed_secret(request), encoded.encode("ascii"), hashlib.sha256).digest()
    return f"e1.{encoded}.{_b64encode(signature)}"


def _verify_embed_token(request: Request, *, token: str | None, public_id: str) -> dict[str, Any]:
    try:
        prefix, encoded, signature = str(token or "").split(".", 2)
        expected = hmac.new(
            _embed_secret(request), encoded.encode("ascii"), hashlib.sha256
        ).digest()
        if prefix != "e1" or not hmac.compare_digest(_b64decode(signature), expected):
            raise ValueError
        payload = json.loads(_b64decode(encoded))
        presented_origin = _normalized_origin(
            request.headers.get("X-Agent-Embed-Origin", "")
        )
        session_nonce = request.cookies.get("ag_embed_session", "")
        if (
            payload.get("v") != _EMBED_PROTOCOL_VERSION
            or payload.get("public_id") != public_id
            or int(payload.get("exp", 0)) < int(time.time())
            or _normalized_origin(str(payload.get("origin") or "")) != payload.get("origin")
            or presented_origin != payload.get("origin")
            or not payload.get("nonce")
            or session_nonce != payload.get("nonce")
            or not str(payload.get("sub") or "")
        ):
            raise ValueError
        return payload
    except (ValueError, TypeError, KeyError, json.JSONDecodeError):
        _raise_runtime_error(
            request,
            401,
            "AGENT_EMBED_TOKEN_INVALID",
            "Embed session authorization expired or invalid",
        )


async def _public_resolution(
    request: Request,
    user: UserContext,
    *,
    public_id: str,
    channel: str,
    embed_token: str | None,
    pinned_version_id: str | None = None,
) -> tuple[dict[str, Any], UserContext]:
    browser_token_authorized = False
    caller = user
    if channel == "embed":
        claim = _verify_embed_token(request, token=embed_token, public_id=public_id)
        browser_token_authorized = True
        if not user.is_authenticated:
            caller = UserContext(
                user_id=f"agent-embed:{claim['sub']}",
                tenant_id="public",
                tier="anonymous",
                is_authenticated=False,
                ip=str(getattr(request.client, "host", "") or ""),
                roles=["guest"],
                user_type="anonymous",
            )
    try:
        resolution = await _repository(request).resolve_public_channel_runtime(
            public_id=public_id,
            channel=channel,
            caller_tenant_id=caller.tenant_id,
            user_id=caller.user_id,
            authenticated=caller.is_authenticated,
            is_tenant_admin=_is_tenant_admin(caller),
            pinned_version_id=pinned_version_id,
            browser_token_authorized=browser_token_authorized,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    # Once public access is accepted, scope anonymous storage to the real tenant
    # while retaining an opaque, non-PII principal owned by this browser grant.
    if not caller.is_authenticated:
        caller = replace(caller, tenant_id=str(resolution["agent"]["tenant_id"]))
    return resolution, caller


@document_router.get("/embed/agents/{public_id}", response_class=HTMLResponse)
async def agent_embed_document(public_id: str, request: Request) -> HTMLResponse:
    try:
        descriptor = await _repository(request).get_publication_channel(public_id=public_id)
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    if descriptor.get("channel") != "embed":
        _raise_runtime_error(
            request,
            404,
            "PUBLICATION_CHANNEL_MISMATCH",
            "Embed Publication not found",
        )
    allowed = _allowed_origins(descriptor)
    parent_origin = _parent_origin(request)
    if not parent_origin or parent_origin not in allowed:
        _raise_runtime_error(
            request,
            403,
            "AGENT_EMBED_ORIGIN_FORBIDDEN",
            "Parent origin is not allowed",
        )
    nonce = secrets.token_urlsafe(32)
    token = _issue_embed_token(
        request,
        public_id=public_id,
        origin=parent_origin,
        nonce=nonce,
    )
    body = (
        "<!doctype html><html><head><meta charset=\"utf-8\">"
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">"
        "<link rel=\"stylesheet\" href=\"/agent-embed.css\">"
        f"<title>{html.escape(str(descriptor.get('name') or 'Agent'))}</title></head>"
        f"<body><main id=\"agent-embed-root\" data-public-id=\"{html.escape(public_id)}\" "
        f"data-parent-origin=\"{html.escape(parent_origin)}\" "
        f"data-embed-token=\"{html.escape(token)}\"></main>"
        "<script type=\"module\" src=\"/agent-embed.js\"></script></body></html>"
    )
    response = HTMLResponse(body)
    forwarded_proto = request.headers.get("x-forwarded-proto", "").split(",")[0].strip().lower()
    secure_cookie = forwarded_proto == "https" or request.url.scheme == "https"
    response.set_cookie(
        "ag_embed_session",
        nonce,
        max_age=_EMBED_TOKEN_TTL_SECONDS,
        httponly=True,
        secure=secure_cookie,
        samesite="none" if secure_cookie else "lax",
        path="/",
    )
    ancestors = " ".join(allowed)
    response.headers["Content-Security-Policy"] = (
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; "
        f"connect-src 'self'; font-src 'self'; frame-ancestors {ancestors}; base-uri 'none'; "
        "form-action 'none'; object-src 'none'"
    )
    response.headers["Cache-Control"] = "no-store"
    response.headers["Referrer-Policy"] = "no-referrer"
    response.headers["Permissions-Policy"] = "camera=(), microphone=(), geolocation=()"
    # Deliberately no X-Frame-Options. The exact CSP frame-ancestors directive
    # is the authoritative Embed framing policy.
    return response


@router.get("/{public_id}", response_model=AgentPublicConfigResponse)
async def get_public_agent_config(
    public_id: str,
    request: Request,
    channel: str = "hosted",
    user: UserContext = Depends(get_user_context),
) -> AgentPublicConfigResponse:
    if channel not in {"hosted", "embed"}:
        _raise_runtime_error(request, 422, "PUBLICATION_CHANNEL_INVALID", "Invalid channel")
    resolution, caller = await _public_resolution(
        request,
        user,
        public_id=public_id,
        channel=channel,
        embed_token=request.headers.get("X-Agent-Embed-Token"),
    )
    publication = resolution["publication"]
    version = resolution.get("version") or {}
    snapshot = await _build_snapshot(request, resolution, caller, channel=channel)
    identity = resolution["spec"].get("identity")
    identity = identity if isinstance(identity, dict) else {}
    policy = publication.get("policy") if isinstance(publication.get("policy"), dict) else {}
    return AgentPublicConfigResponse(
        public_id=public_id,
        publication_id=str(publication["publication_id"]),
        channel=channel,
        auth_mode=str(publication["auth_mode"]),
        name=str(resolution["agent"]["name"]),
        description=str(resolution["agent"].get("description") or ""),
        identity={
            key: identity[key]
            for key in ("icon_url", "theme_color", "welcome_message", "suggested_prompts")
            if identity.get(key) is not None
        },
        attachments=bool(policy.get("attachments", False)),
        version_number=int(version.get("version_number") or 1),
        capability_count=len(snapshot.get("capabilities") or []),
        knowledge_count=len((snapshot.get("knowledge") or {}).get("datasets") or []),
        release_gate_verified=bool(version.get("release_evaluation_id")),
        published_at=publication.get("updated_at") or publication.get("created_at"),
        request_id=_request_id(request),
    )


@router.post(
    "/{public_id}/attachments",
    response_model=AgentRuntimeAttachmentUploadResponse,
    status_code=201,
)
async def upload_public_agent_attachment(
    public_id: str,
    request: Request,
    channel: str = "hosted",
    file: UploadFile = File(...),
    user: UserContext = Depends(get_user_context),
) -> AgentRuntimeAttachmentUploadResponse:
    reject_client_agent_forgery(request)
    if channel not in {"hosted", "embed"}:
        _raise_runtime_error(request, 422, "PUBLICATION_CHANNEL_INVALID", "Invalid channel")
    resolution, caller = await _public_resolution(
        request,
        user,
        public_id=public_id,
        channel=channel,
        embed_token=request.headers.get("X-Agent-Embed-Token"),
    )
    await _enforce_channel_limits(
        request,
        publication=resolution["publication"],
        principal_id=caller.user_id,
    )
    snapshot = await _build_snapshot(request, resolution, caller, channel=channel)
    _assert_attachments_allowed(request, snapshot, [file])
    return await _store_runtime_attachment(
        request,
        caller,
        publication_id=str(resolution["publication"]["publication_id"]),
        channel=channel,
        file=file,
    )


@router.post(
    "/{public_id}/sessions",
    response_model=AgentRuntimeSessionResponse,
    status_code=201,
)
async def create_public_agent_session(
    public_id: str,
    payload: AgentPublicSessionRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentRuntimeSessionResponse:
    reject_client_agent_forgery(request)
    resolution, caller = await _public_resolution(
        request,
        user,
        public_id=public_id,
        channel=payload.channel,
        embed_token=request.headers.get("X-Agent-Embed-Token") or payload.embed_token,
    )
    await _enforce_channel_limits(
        request,
        publication=resolution["publication"],
        principal_id=caller.user_id,
    )
    snapshot = await _build_snapshot(request, resolution, caller, channel=payload.channel)
    session_id = str(uuid.uuid4())
    await _bind_session(
        request,
        caller,
        session_id=session_id,
        snapshot=snapshot,
        draft_revision=None,
    )
    return AgentRuntimeSessionResponse(
        session_id=session_id,
        agent_id=snapshot["agent_id"],
        agent_version_id=snapshot["agent_version_id"],
        publication_id=snapshot["publication"]["id"],
        channel=payload.channel,
        runtime_fingerprint=runtime_sha256(snapshot),
        request_id=_request_id(request),
    )


@router.post("/{public_id}/chat/stream")
async def public_agent_chat_stream(
    public_id: str,
    payload: AgentPublicChatRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    reject_client_agent_forgery(request)
    resolution, caller = await _public_resolution(
        request,
        user,
        public_id=public_id,
        channel=payload.channel,
        embed_token=request.headers.get("X-Agent-Embed-Token") or payload.embed_token,
    )
    existing = await _existing_session(request, payload.session_id)
    if existing:
        _assert_existing_pin(
            request,
            caller,
            existing,
            agent_id=str(resolution["agent"]["agent_id"]),
            agent_version_id=None,
            publication_id=str(resolution["publication"]["publication_id"]),
            channel=payload.channel,
            draft_revision=None,
        )
        if existing.agent_version_id != resolution["version"]["agent_version_id"]:
            resolution, caller = await _public_resolution(
                request,
                user,
                public_id=public_id,
                channel=payload.channel,
                embed_token=request.headers.get("X-Agent-Embed-Token") or payload.embed_token,
                pinned_version_id=existing.agent_version_id,
            )
    await _enforce_channel_limits(
        request,
        publication=resolution["publication"],
        principal_id=caller.user_id,
    )
    snapshot = await _build_snapshot(request, resolution, caller, channel=payload.channel)
    _assert_attachments_allowed(request, snapshot, payload.attachments)
    resolved_attachments = await _resolve_runtime_attachments(
        request,
        caller,
        publication_id=str(resolution["publication"]["publication_id"]),
        channel=payload.channel,
        attachments=payload.attachments,
    )
    session_id = payload.session_id or str(uuid.uuid4())
    await _bind_session(
        request,
        caller,
        session_id=session_id,
        snapshot=snapshot,
        draft_revision=None,
    )
    return await _start_runtime_stream(
        request,
        caller,
        body=_runtime_body(
            message=payload.message,
            session_id=session_id,
            attachments=resolved_attachments,
            resume_run_id=payload.resume_run_id,
            resume_approval_id=payload.resume_approval_id,
        ),
        snapshot=snapshot,
    )


@router.post("/{public_id}/feedback", response_model=AgentRuntimeFeedbackResponse)
async def public_agent_feedback(
    public_id: str,
    payload: AgentRuntimeFeedbackRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentRuntimeFeedbackResponse:
    if payload.channel not in {"hosted", "embed"}:
        _raise_runtime_error(request, 422, "PUBLICATION_CHANNEL_INVALID", "Invalid channel")
    resolution, caller = await _public_resolution(
        request,
        user,
        public_id=public_id,
        channel=payload.channel,
        embed_token=request.headers.get("X-Agent-Embed-Token") or payload.embed_token,
    )
    existing = await _existing_session(request, payload.session_id)
    if not existing:
        _raise_runtime_error(
            request, 404, "AGENT_RUNTIME_SESSION_NOT_FOUND", "Agent runtime session not found"
        )
    _assert_existing_pin(
        request,
        caller,
        existing,
        agent_id=str(resolution["agent"]["agent_id"]),
        agent_version_id=existing.agent_version_id,
        publication_id=str(resolution["publication"]["publication_id"]),
        channel=payload.channel,
        draft_revision=None,
    )
    try:
        row = await _repository(request).record_runtime_feedback(
            tenant_id=caller.tenant_id,
            publication_id=str(resolution["publication"]["publication_id"]),
            agent_version_id=existing.agent_version_id,
            session_id=payload.session_id,
            principal_id=caller.user_id,
            channel=payload.channel,
            rating=payload.rating,
            comment=payload.comment,
        )
    except AgentRepositoryError as exc:
        _map_repository_error(request, exc)
    return AgentRuntimeFeedbackResponse(
        feedback_id=str(row["feedback_id"]),
        session_id=payload.session_id,
        rating=payload.rating,
        request_id=_request_id(request),
    )


__all__ = ["document_router", "router"]
