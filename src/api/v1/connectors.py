"""
Connector Management API — OAuth-based third-party integrations.

Supports: Confluence, Outlook, GitHub, Gmail, Google Drive, Slack.

Endpoints:
  GET    /connectors/available          — List configured connectors
  GET    /connectors/mine               — List user's connected services
  GET    /connectors/auth/{provider}    — Initiate OAuth flow (returns redirect URL)
  GET    /connectors/callback/{provider} — OAuth callback (exchanges code for tokens)
  DELETE /connectors/{provider}         — Disconnect a connector
  POST   /connectors/{provider}/search  — Search connector data (e.g., Confluence pages)
"""

from __future__ import annotations

import json
import os
import secrets
import urllib.parse
from collections.abc import Mapping
from datetime import datetime, timedelta, timezone
from typing import Any
from uuid import UUID

import httpx
from ai_gateway_core.logging import get_logger
from ai_gateway_core.security import (
    SafeFetchError,
    decrypt_value,
    encrypt_value,
    is_encrypted,
    safe_form_post,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import RedirectResponse
from pydantic import BaseModel

from ...core.auth.user_resolver import UserContext
from ..deps import AuthContext, get_auth_context, get_user_context
from ..redacted_validation_route import RedactedValidationRoute
from ..schemas.mcp import (
    ConnectorPrincipalCreate,
    ConnectorPrincipalListResponse,
    ConnectorPrincipalMutationResponse,
    ConnectorPrincipalResponse,
    MCPMutationResponse,
)

logger = get_logger(__name__)
router = APIRouter(prefix="/connectors", tags=["Connectors"])
principal_router = APIRouter(route_class=RedactedValidationRoute)
OAUTH_STATE_TTL_SECONDS = 600
OAUTH_STATE_KEY_PREFIX = "oauth:state:"


# ─── Models ───────────────────────────────────────────────────────────

class ConnectorInfo(BaseModel):
    provider: str
    display_name: str
    description: str | None = None
    icon_url: str | None = None
    enabled: bool = True
    connected: bool = False
    status: str | None = None


class ConnectorSearchRequest(BaseModel):
    query: str
    limit: int = 10


@principal_router.post(
    "/{provider}/principals",
    response_model=ConnectorPrincipalMutationResponse,
    status_code=201,
)
async def create_connector_principal(
    provider: str,
    payload: ConnectorPrincipalCreate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """Register an explicit Secret-ref credential principal for Agent runtime."""

    from .mcp import _audit_mutation, _authorize_write, _get_repository, _map_repository_error

    _authorize_write(request, auth, user)
    if provider != "confluence":
        raise HTTPException(422, "Only existing V1 Connector types are supported")
    try:
        principal = await _get_repository(request).create_connector_principal(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            provider=provider,
            **payload.model_dump(),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="create",
        resource_type="connector_principal",
        resource_id=str(principal["grant_id"]),
        summary={
            "provider": provider,
            "principal_type": payload.principal_type,
            "owner_user_id": payload.owner_user_id,
            "allowed_channels": payload.allowed_channels,
        },
    )
    return ConnectorPrincipalMutationResponse(
        principal=ConnectorPrincipalResponse.model_validate(principal),
        request_id=str(getattr(request.state, "request_id", "")),
        audit_ref=audit_ref,
    )


@principal_router.get(
    "/{provider}/principals",
    response_model=ConnectorPrincipalListResponse,
)
async def list_connector_principals(
    provider: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    from .mcp import _authorize_read, _get_repository, _map_repository_error

    _authorize_read(request, auth, user)
    if provider != "confluence":
        raise HTTPException(422, "Only existing V1 Connector types are supported")
    try:
        principals = await _get_repository(request).list_connector_principals(
            tenant_id=user.tenant_id,
            provider=provider,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    rendered = [ConnectorPrincipalResponse.model_validate(item) for item in principals]
    return ConnectorPrincipalListResponse(principals=rendered, total=len(rendered))


@principal_router.delete(
    "/{provider}/principals/{grant_id}",
    response_model=MCPMutationResponse,
)
async def revoke_connector_principal(
    provider: str,
    grant_id: UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    from .mcp import _audit_mutation, _authorize_write, _get_repository, _map_repository_error

    _authorize_write(request, auth, user)
    if provider != "confluence":
        raise HTTPException(422, "Only existing V1 Connector types are supported")
    try:
        await _get_repository(request).revoke_connector_principal(
            tenant_id=user.tenant_id,
            grant_id=grant_id,
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="revoke",
        resource_type="connector_principal",
        resource_id=grant_id,
        summary={"provider": provider},
    )
    return MCPMutationResponse(
        status="revoked",
        request_id=str(getattr(request.state, "request_id", "")),
        audit_ref=audit_ref,
    )


# ─── Helpers ──────────────────────────────────────────────────────────

def _get_db(request: Request):
    return getattr(request.app.state, "database", None)


def _oauth_state_key(state: str) -> str:
    return f"{OAUTH_STATE_KEY_PREFIX}{state}"


def _get_oauth_redis(request: Request | None) -> Any | None:
    if request is None:
        return None
    return getattr(request.app.state, "redis", None)


def _decode_oauth_state(raw: Any) -> dict[str, Any] | None:
    if raw is None:
        return None
    if isinstance(raw, bytes):
        raw = raw.decode("utf-8")
    if isinstance(raw, str):
        return json.loads(raw)
    if isinstance(raw, dict):
        return raw
    return None


def _json_object(raw: Any) -> dict[str, Any]:
    """Decode JSON/JSONB values returned by either configured asyncpg codec."""

    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except (TypeError, json.JSONDecodeError):
            return {}
    return dict(raw) if isinstance(raw, Mapping) else {}


async def _redis_save_json(redis: Any, key: str, value: dict[str, Any], ttl: int) -> None:
    if hasattr(redis, "save"):
        await redis.save(key, value, ttl)
    elif hasattr(redis, "setex"):
        await redis.setex(key, ttl, json.dumps(value, default=str))
    elif hasattr(redis, "set"):
        await redis.set(key, json.dumps(value, default=str), ex=ttl)
    else:
        raise RuntimeError("redis client does not support save/set")


async def _redis_consume_json(redis: Any, key: str) -> dict[str, Any] | None:
    if hasattr(redis, "getdel"):
        return _decode_oauth_state(await redis.getdel(key))
    raise RuntimeError("redis client does not support atomic getdel")


async def _store_oauth_state(
    request: Request,
    *,
    state: str,
    tenant_id: str,
    user_id: str,
    provider: str,
    nonce: str,
) -> None:
    redis = _get_oauth_redis(request)
    if redis is None:
        raise HTTPException(503, "OAuth state store unavailable")
    entry = {
        "tenant_id": tenant_id,
        "user_id": user_id,
        "provider": provider,
        "nonce": nonce,
        "expires_at": (
            datetime.now(timezone.utc) + timedelta(seconds=OAUTH_STATE_TTL_SECONDS)
        ).isoformat(),
    }
    try:
        await _redis_save_json(redis, _oauth_state_key(state), entry, OAUTH_STATE_TTL_SECONDS)
    except Exception as exc:
        logger.warning("OAuth state store failed")
        raise HTTPException(503, "OAuth state store unavailable") from exc


async def _consume_oauth_state(request: Request | None, state: str) -> dict[str, Any]:
    redis = _get_oauth_redis(request)
    if redis is None:
        raise HTTPException(400, "Invalid or expired state parameter")
    key = _oauth_state_key(state)
    try:
        entry = await _redis_consume_json(redis, key)
    except HTTPException:
        raise
    except Exception as exc:
        logger.warning("OAuth state consume failed")
        raise HTTPException(503, "OAuth state store unavailable") from exc
    if not entry:
        raise HTTPException(400, "Invalid or expired state parameter")
    expires_at = entry.get("expires_at")
    if isinstance(expires_at, str):
        expires_at = datetime.fromisoformat(expires_at)
    if not isinstance(expires_at, datetime):
        raise HTTPException(400, "Invalid or expired state parameter")
    if expires_at <= datetime.now(timezone.utc):
        raise HTTPException(400, "Invalid or expired state parameter")
    return entry


async def _get_connector_config(db, provider: str, tenant_id: str = "") -> dict | None:
    row = await db.fetchrow(
        """SELECT * FROM (
               SELECT * FROM connector_configs
               WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '')
               ORDER BY (tenant_id = $2) DESC LIMIT 1
           ) AS effective WHERE enabled = true""",
        provider, tenant_id,
    )
    return dict(row) if row else None


def _decrypt_connector_secret(request: Request, config: dict[str, Any]) -> dict[str, Any]:
    """Decrypt new ``enc:`` secrets while preserving legacy plaintext rows."""

    stored = str(config.get("client_secret") or "")
    if not is_encrypted(stored):
        return config
    injected = getattr(request.app.state, "connector_encryption_key", None)
    encryption_key = (
        injected
        if isinstance(injected, str)
        else os.environ.get("GATEWAY_ENCRYPTION_KEY", "")
    )
    if not encryption_key:
        raise HTTPException(503, "Connector secret decryption is not configured")
    decrypted = decrypt_value(stored, encryption_key)
    if not decrypted or is_encrypted(decrypted):
        raise HTTPException(503, "Connector secret cannot be decrypted")
    return {**config, "client_secret": decrypted}


async def _get_user_connector(db, tenant_id: str, user_id: str, provider: str) -> dict | None:
    row = await db.fetchrow(
        """SELECT * FROM user_connectors
           WHERE tenant_id = $1 AND user_id = $2 AND provider = $3""",
        tenant_id, user_id, provider,
    )
    return dict(row) if row else None


def _connector_user_token_key(request: Request) -> str:
    injected = getattr(request.app.state, "connector_encryption_key", None)
    if isinstance(injected, str) and injected:
        return injected
    return os.environ.get("GATEWAY_ENCRYPTION_KEY", "")


def _encrypt_user_token(request: Request, token: str | None) -> str | None:
    if not token:
        return token
    if is_encrypted(token):
        return token
    key = _connector_user_token_key(request)
    if not key:
        raise HTTPException(503, "Connector token encryption is not configured")
    encrypted = encrypt_value(token, key)
    if not is_encrypted(encrypted):
        raise HTTPException(500, "Connector token encryption failed")
    return encrypted


def _decrypt_user_token(request: Request, token: str | None) -> str | None:
    if not token or not is_encrypted(token):
        return token
    key = _connector_user_token_key(request)
    if not key:
        raise HTTPException(503, "Connector token decryption is not configured")
    decrypted = decrypt_value(token, key)
    if not decrypted or is_encrypted(decrypted):
        raise HTTPException(503, "Connector token cannot be decrypted")
    return decrypted


def _decrypt_user_connector(request: Request, connector: dict[str, Any]) -> dict[str, Any]:
    return {
        **connector,
        "access_token": _decrypt_user_token(request, connector.get("access_token")),
        "refresh_token": _decrypt_user_token(request, connector.get("refresh_token")),
    }


async def _save_user_connector(
    db, request: Request, tenant_id: str, user_id: str, provider: str,
    access_token: str, refresh_token: str | None,
    expires_at: datetime | None, scopes: str,
    metadata: dict | None = None,
):
    stored_access = _encrypt_user_token(request, access_token)
    stored_refresh = _encrypt_user_token(request, refresh_token)
    await db.execute(
        """INSERT INTO user_connectors (tenant_id, user_id, provider, access_token, refresh_token,
           token_expires_at, token_scopes, provider_metadata, status, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'connected', NOW())
           ON CONFLICT (tenant_id, user_id, provider)
           DO UPDATE SET access_token = $4, refresh_token = $5, token_expires_at = $6,
                         token_scopes = $7, provider_metadata = COALESCE($8, user_connectors.provider_metadata),
                         status = 'connected', last_error = NULL, updated_at = NOW()""",
        tenant_id, user_id, provider, stored_access, stored_refresh,
        expires_at, scopes, metadata,
    )


async def _refresh_token_if_needed(db, request: Request, connector: dict, config: dict) -> str:
    """Refresh OAuth token if expired. Returns valid access_token."""
    connector = _decrypt_user_connector(request, connector)
    if connector.get("token_expires_at") and connector["token_expires_at"] < datetime.now(timezone.utc):
        if not connector.get("refresh_token"):
            raise HTTPException(400, "Token expired and no refresh token available. Reconnect.")

        try:
            resp = await safe_form_post(config["token_url"], data={
                "grant_type": "refresh_token",
                "refresh_token": connector["refresh_token"],
                "client_id": config["client_id"],
                "client_secret": config.get("client_secret", ""),
            })
        except SafeFetchError as exc:
            await db.execute(
                "UPDATE user_connectors SET status = 'expired', last_error = $1, updated_at = NOW() WHERE id = $2",
                "Refresh failed", connector["id"],
            )
            raise HTTPException(401, "Token refresh failed. Please reconnect.") from exc
        if resp.status_code != 200:
            await db.execute(
                "UPDATE user_connectors SET status = 'expired', last_error = $1, updated_at = NOW() WHERE id = $2",
                "Refresh failed", connector["id"],
            )
            raise HTTPException(401, "Token refresh failed. Please reconnect.")

        data = resp.json()
        expires_at = datetime.now(timezone.utc) + timedelta(seconds=data.get("expires_in", 3600))
        stored_access = _encrypt_user_token(request, data["access_token"])
        stored_refresh = (
            _encrypt_user_token(request, data.get("refresh_token"))
            if data.get("refresh_token")
            else None
        )
        await db.execute(
            """UPDATE user_connectors SET access_token = $1, refresh_token = COALESCE($2, refresh_token),
               token_expires_at = $3, status = 'connected', updated_at = NOW() WHERE id = $4""",
            stored_access, stored_refresh, expires_at, connector["id"],
        )
        return data["access_token"]

    return connector["access_token"]


async def resolve_catalog_connector_credential(
    request: Request,
    *,
    tenant_id: str,
    user_id: str,
    provider: str,
) -> dict[str, Any]:
    """Resolve one connected OAuth credential for an authorized catalog call.

    This is intentionally the same path used by the V1 connector search API:
    tenant/user predicates, connected status, enabled provider config,
    Gateway-owned decryption, and just-in-time refresh.  Callers receive the
    live token only in-process; it is never suitable for a Runtime snapshot.
    """

    db = _get_db(request)
    if db is None or getattr(db, "enabled", True) is not True:
        raise RuntimeError("connector credential backend unavailable")
    connector = await _get_user_connector(db, tenant_id, user_id, provider)
    if not connector or connector.get("status") != "connected":
        raise PermissionError("connected connector unavailable")
    config = await _get_connector_config(db, provider, tenant_id)
    if not config:
        raise PermissionError("connector configuration unavailable")
    config = _decrypt_connector_secret(request, config)
    access_token = await _refresh_token_if_needed(db, request, connector, config)
    metadata = connector.get("provider_metadata") or {}
    if isinstance(metadata, str):
        try:
            metadata = json.loads(metadata)
        except json.JSONDecodeError:
            metadata = {}
    if not isinstance(metadata, dict) or not access_token:
        raise RuntimeError("connector credential unavailable")
    return {"access_token": str(access_token), "provider_metadata": metadata}


# ─── Endpoints ────────────────────────────────────────────────────────

@router.get("/available")
async def list_available_connectors(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """List all configured connectors with user's connection status."""
    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    configs = await db.fetch(
        """SELECT * FROM (
               SELECT DISTINCT ON (provider) * FROM connector_configs
               WHERE tenant_id = $1 OR tenant_id = ''
               ORDER BY provider, (tenant_id = $1) DESC
           ) AS effective WHERE enabled = true""",
        user.tenant_id,
    )
    user_conns = await db.fetch(
        "SELECT provider, status FROM user_connectors WHERE tenant_id = $1 AND user_id = $2",
        user.tenant_id, user.user_id,
    )
    conn_map = {r["provider"]: r["status"] for r in user_conns}

    return [
        ConnectorInfo(
            provider=c["provider"],
            display_name=c["display_name"],
            description=c.get("description"),
            icon_url=c.get("icon_url"),
            enabled=c["enabled"],
            connected=c["provider"] in conn_map and conn_map[c["provider"]] == "connected",
            status=conn_map.get(c["provider"]),
        )
        for c in configs
    ]


@router.get("/mine")
async def list_my_connectors(request: Request, user: UserContext = Depends(get_user_context)):
    """List user's connected services."""
    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    rows = await db.fetch(
        """SELECT provider, display_name, status, last_used_at, provider_metadata, created_at
           FROM user_connectors WHERE tenant_id = $1 AND user_id = $2 AND status != 'revoked'""",
        user.tenant_id, user.user_id,
    )
    return [dict(r) for r in rows]


@router.get("/auth/{provider}")
async def initiate_oauth(
    provider: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Initiate OAuth flow — returns the authorization URL to redirect the user."""
    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    config = await _get_connector_config(db, provider, user.tenant_id)
    if not config:
        raise HTTPException(404, f"Connector not configured: {provider}")
    if not config.get("client_id"):
        raise HTTPException(400, f"OAuth not configured for {provider}. Admin must set client_id.")

    nonce = secrets.token_urlsafe(16)
    state = f"{user.tenant_id}:{user.user_id}:{provider}:{nonce}"
    await _store_oauth_state(
        request,
        state=state,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        provider=provider,
        nonce=nonce,
    )

    # Build redirect URI
    base_url = str(request.base_url).rstrip("/")
    redirect_uri = config.get("redirect_uri") or f"{base_url}/api/v1/connectors/callback/{provider}"

    extra_config = _json_object(config.get("extra_config"))
    params = {
        "client_id": config["client_id"],
        "redirect_uri": redirect_uri,
        "scope": config["scopes"],
        "response_type": "code",
        "state": state,
    }
    # Provider-specific params
    if provider == "confluence":
        params["audience"] = extra_config.get("audience", "api.atlassian.com")
        params["prompt"] = "consent"
    if provider in ("outlook", "gmail", "google_drive"):
        params["access_type"] = "offline"
        params["prompt"] = "consent"

    auth_url = f"{config['auth_url']}?{urllib.parse.urlencode(params)}"
    return {"auth_url": auth_url, "state": state}


def _console_public_base_url() -> str:
    """Return the explicitly configured console origin or a safe relative fallback.

    CORS is an authorization allowlist, not a routing preference.  Selecting its
    first entry made OAuth completion depend on configuration order and could
    redirect a user to a different allowed deployment.  Keep callback routing
    explicit and fail closed to a same-origin relative path when it is absent or
    malformed.
    """

    configured = os.environ.get("CONSOLE_PUBLIC_BASE_URL", "").strip()
    if not configured:
        return ""
    parsed = urllib.parse.urlsplit(configured)
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
    ):
        logger.warning("Ignoring invalid CONSOLE_PUBLIC_BASE_URL")
        return ""
    return f"{parsed.scheme}://{parsed.netloc}"


def _oauth_console_redirect(provider: str, status: str = "connected") -> RedirectResponse:
    origin = _console_public_base_url()
    path = (
        f"/connections?connected={urllib.parse.quote(provider)}"
        f"&status={urllib.parse.quote(status)}"
    )
    target = f"{origin.rstrip('/')}{path}" if origin else path
    return RedirectResponse(url=target, status_code=302)


@router.get(
    "/callback/{provider}",
    status_code=302,
    response_class=RedirectResponse,
    responses={302: {
        "description": "Validated OAuth success or denial redirects to the personal connections page.",
        "headers": {"Location": {"required": True, "schema": {"type": "string"}}},
    }},
)
async def oauth_callback(
    provider: str,
    request: Request,
    code: str | None = None,
    state: str = Query(""),
    error: str | None = None,
):
    """OAuth callback — exchanges a code or returns a validated authorization failure."""
    if code is None and error is None:
        raise HTTPException(422, [{"type": "missing", "loc": ["query", "code"], "msg": "Field required", "input": None}])
    if error is not None and (code is not None or not error):
        raise HTTPException(400, "Invalid OAuth callback")
    # Parse and validate state parameter. The IdP redirects the browser here
    # without a Bearer token; the one-time Redis state is the CSRF check.
    parts = state.split(":")
    if len(parts) < 4:
        raise HTTPException(400, "Invalid state parameter")
    tenant_id, user_id, state_provider, nonce = parts[0], parts[1], parts[2], parts[3]

    if state_provider != provider:
        raise HTTPException(400, "State provider mismatch")
    if not tenant_id or not user_id or not nonce:
        raise HTTPException(400, "Incomplete state parameter")
    issued_state = await _consume_oauth_state(request, state)
    if (
        issued_state["tenant_id"] != tenant_id
        or issued_state["user_id"] != user_id
        or issued_state["provider"] != provider
        or issued_state["nonce"] != nonce
    ):
        raise HTTPException(400, "Invalid state parameter")

    if error is not None:
        # Never reflect provider error text or read/decrypt credentials on denial.
        origin = _console_public_base_url()
        return RedirectResponse(
            url=f"{origin}/connections?status=authorization_failed", status_code=302,
        )

    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    config = await _get_connector_config(db, provider, tenant_id)
    if not config:
        raise HTTPException(404, f"Connector not configured: {provider}")
    config = _decrypt_connector_secret(request, config)

    base_url = str(request.base_url).rstrip("/") if request else ""
    redirect_uri = config.get("redirect_uri") or f"{base_url}/api/v1/connectors/callback/{provider}"

    # Exchange code for tokens
    token_data = {
        "grant_type": "authorization_code",
        "client_id": config["client_id"],
        "code": code,
        "redirect_uri": redirect_uri,
    }
    if config.get("client_secret"):
        token_data["client_secret"] = config["client_secret"]

    headers = {"Accept": "application/json"}

    try:
        resp = await safe_form_post(
            config["token_url"],
            data=token_data,
            headers=headers,
        )
    except SafeFetchError as exc:
        logger.warning("OAuth token exchange destination rejected: provider=%s", provider)
        raise HTTPException(400, "Token exchange failed") from exc

    if resp.status_code != 200:
        logger.error(
            "OAuth token exchange failed: provider=%s status=%s",
            provider,
            resp.status_code,
        )
        raise HTTPException(400, "Token exchange failed")

    data = resp.json()
    access_token = data.get("access_token")
    refresh_token = data.get("refresh_token")
    expires_in = data.get("expires_in", 3600)

    if not access_token:
        raise HTTPException(400, "No access_token in response")

    from datetime import timedelta
    expires_at = datetime.now(timezone.utc) + timedelta(seconds=expires_in)

    # Provider-specific metadata
    metadata: dict[str, Any] = {}
    if provider == "confluence":
        # Get accessible resources (cloud ID)
        async with httpx.AsyncClient() as client:
            res = await client.get(
                "https://api.atlassian.com/oauth/token/accessible-resources",
                headers={"Authorization": f"Bearer {access_token}"},
            )
            if res.status_code == 200:
                resources = res.json()
                if resources:
                    metadata["cloud_id"] = resources[0]["id"]
                    metadata["site_url"] = resources[0].get("url", "")
                    metadata["site_name"] = resources[0].get("name", "")

    await _save_user_connector(
        db, request, tenant_id, user_id, provider,
        access_token, refresh_token, expires_at,
        config["scopes"], metadata,
    )

    logger.info(f"Connector connected: {provider} for user {user_id}")
    return _oauth_console_redirect(provider)


@router.delete("/{provider}")
async def disconnect_connector(
    provider: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Disconnect a connector — revokes tokens."""
    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    await db.execute(
        """UPDATE user_connectors SET status = 'revoked', access_token = NULL,
           refresh_token = NULL, updated_at = NOW()
           WHERE tenant_id = $1 AND user_id = $2 AND provider = $3""",
        user.tenant_id, user.user_id, provider,
    )
    # Stop MCP server if running
    try:
        from ai_gateway_core.connectors import get_connector_mcp_service
        mcp = get_connector_mcp_service()
        await mcp.stop_connector(user.tenant_id, provider)
    except Exception:
        pass

    return {"status": "disconnected", "provider": provider}


@router.post("/{provider}/activate")
async def activate_connector_mcp(
    provider: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Activate MCP server for a connected service.

    Spawns the appropriate MCP server process (e.g., mcp-atlassian)
    with the user's stored credentials. AI tools become available after this.
    """
    if provider != "confluence":
        raise HTTPException(400, f"MCP activation not supported for: {provider}")

    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")

    row = await _get_user_connector(db, user.tenant_id, user.user_id, provider)
    if not row or row.get("status") != "connected":
        raise HTTPException(400, "No active Confluence connection. Connect first via Settings.")
    row = _decrypt_user_connector(request, row)
    metadata = row.get("provider_metadata") or {}
    if isinstance(metadata, str):
        metadata = json.loads(metadata)
    site_url = str(metadata.get("site_url") or metadata.get("domain") or "")
    domain = urllib.parse.urlsplit(site_url).netloc or site_url.replace("https://", "").replace("http://", "").split("/")[0]
    email = str(metadata.get("email") or "")
    api_token = str(row.get("access_token") or "")
    if not domain or not api_token:
        raise HTTPException(400, "Connector credentials are incomplete. Reconnect.")
    if not email:
        email = f"{user.user_id}@oauth.local"

    from ai_gateway_core.connectors import get_connector_mcp_service
    mcp = get_connector_mcp_service()

    try:
        tools = await mcp.start_confluence(
            tenant_id=user.tenant_id,
            domain=domain,
            email=email,
            api_token=api_token,
        )
        # start_confluence returns list[dict[str, str]] — not ToolDefinition objects
        return {
            "status": "activated",
            "provider": provider,
            "tools": tools,
            "tool_count": len(tools),
        }
    except RuntimeError as e:
        raise HTTPException(500, str(e))


@router.get("/{provider}/mcp-status")
async def connector_mcp_status(
    provider: str,
    user: UserContext = Depends(get_user_context),
):
    """Check if MCP server is running for a provider."""
    from ai_gateway_core.connectors import get_connector_mcp_service
    mcp = get_connector_mcp_service()

    connected = mcp.is_connected(user.tenant_id, provider)
    tools = mcp.get_tools(user.tenant_id, provider) if connected else []

    # get_tools returns list[dict[str, str]] — not ToolDefinition objects
    return {
        "provider": provider,
        "mcp_active": connected,
        "tools": tools,
        "tool_count": len(tools),
    }


@router.post("/{provider}/search")
async def search_connector(
    provider: str,
    body: ConnectorSearchRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Search connector data (e.g., Confluence pages, emails)."""
    db = _get_db(request)
    connector = await _get_user_connector(db, user.tenant_id, user.user_id, provider)
    if not connector or connector["status"] != "connected":
        raise HTTPException(400, f"Not connected to {provider}. Connect first.")

    config = await _get_connector_config(db, provider, user.tenant_id)
    if not config:
        raise HTTPException(404, f"Connector not configured: {provider}")
    config = _decrypt_connector_secret(request, config)

    access_token = await _refresh_token_if_needed(db, request, connector, config)
    metadata = connector.get("provider_metadata") or {}

    # Provider-specific search
    if provider == "confluence":
        return await _search_confluence(access_token, metadata, body.query, body.limit)
    elif provider == "outlook":
        return await _search_outlook(access_token, body.query, body.limit)
    elif provider == "github":
        return await _search_github(access_token, body.query, body.limit)
    elif provider == "gmail":
        return await _search_gmail(access_token, body.query, body.limit)
    else:
        raise HTTPException(400, f"Search not supported for {provider}")


# ─── Provider-specific search implementations ─────────────────────────

async def _search_confluence(token: str, metadata: dict, query: str, limit: int) -> list[dict]:
    cloud_id = metadata.get("cloud_id", "")
    if not cloud_id:
        raise HTTPException(400, "Confluence cloud_id not found. Reconnect.")

    # Escape CQL special characters to prevent injection
    safe_query = query.replace("\\", "\\\\").replace('"', '\\"')
    cql = f'type=page AND text~"{safe_query}"'
    url = f"https://api.atlassian.com/ex/confluence/{cloud_id}/wiki/rest/api/content/search"

    async with httpx.AsyncClient() as client:
        resp = await client.get(url, params={"cql": cql, "limit": limit, "expand": "body.view,space"},
                                headers={"Authorization": f"Bearer {token}"})
    if resp.status_code != 200:
        raise HTTPException(resp.status_code, "Confluence search failed")

    data = resp.json()
    results = []
    for page in data.get("results", []):
        body_html = page.get("body", {}).get("view", {}).get("value", "")
        # Strip HTML tags for text preview
        import re
        text = re.sub(r"<[^>]+>", "", body_html)[:500]
        results.append({
            "id": page["id"],
            "title": page["title"],
            "space": page.get("space", {}).get("name", ""),
            "url": f"{page.get('_links', {}).get('base', '')}{page.get('_links', {}).get('webui', '')}",
            "excerpt": text,
            "type": "confluence_page",
        })
    return results


async def _search_outlook(token: str, query: str, limit: int) -> list[dict]:
    url = "https://graph.microsoft.com/v1.0/me/messages"
    params = {"$search": f'"{query}"', "$top": limit, "$select": "subject,from,receivedDateTime,bodyPreview"}

    async with httpx.AsyncClient() as client:
        resp = await client.get(url, params=params, headers={"Authorization": f"Bearer {token}"})
    if resp.status_code != 200:
        raise HTTPException(resp.status_code, "Outlook search failed")

    data = resp.json()
    return [
        {
            "id": msg["id"],
            "title": msg.get("subject", ""),
            "from": msg.get("from", {}).get("emailAddress", {}).get("address", ""),
            "date": msg.get("receivedDateTime", ""),
            "excerpt": msg.get("bodyPreview", "")[:300],
            "type": "email",
        }
        for msg in data.get("value", [])
    ]


async def _search_github(token: str, query: str, limit: int) -> list[dict]:
    url = "https://api.github.com/search/code"
    async with httpx.AsyncClient() as client:
        resp = await client.get(url, params={"q": query, "per_page": limit},
                                headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"})
    if resp.status_code != 200:
        raise HTTPException(resp.status_code, "GitHub search failed")

    data = resp.json()
    return [
        {
            "id": item.get("sha", ""),
            "title": item.get("name", ""),
            "path": item.get("path", ""),
            "repo": item.get("repository", {}).get("full_name", ""),
            "url": item.get("html_url", ""),
            "type": "code",
        }
        for item in data.get("items", [])[:limit]
    ]


async def _search_gmail(token: str, query: str, limit: int) -> list[dict]:
    url = "https://gmail.googleapis.com/gmail/v1/users/me/messages"
    async with httpx.AsyncClient() as client:
        resp = await client.get(url, params={"q": query, "maxResults": limit},
                                headers={"Authorization": f"Bearer {token}"})
    if resp.status_code != 200:
        raise HTTPException(resp.status_code, "Gmail search failed")

    messages = resp.json().get("messages", [])
    results = []
    for msg in messages[:limit]:
        async with httpx.AsyncClient() as client:
            detail = await client.get(
                f"https://gmail.googleapis.com/gmail/v1/users/me/messages/{msg['id']}",
                params={"format": "metadata", "metadataHeaders": "Subject,From,Date"},
                headers={"Authorization": f"Bearer {token}"},
            )
        if detail.status_code == 200:
            headers = {h["name"]: h["value"] for h in detail.json().get("payload", {}).get("headers", [])}
            results.append({
                "id": msg["id"],
                "title": headers.get("Subject", ""),
                "from": headers.get("From", ""),
                "date": headers.get("Date", ""),
                "excerpt": detail.json().get("snippet", ""),
                "type": "email",
            })
    return results


# Credential-bearing Agent principal routes use a route class that removes
# Pydantic's echoed input values from validation errors. Existing Connector
# paths keep their legacy response contract.
router.include_router(principal_router)
