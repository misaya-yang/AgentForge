"""Tenant-bound read-only Confluence broker for the Rust capability worker.

The worker never receives a Confluence token.  This route is the only place
where the legacy connection record is resolved and the upstream Basic auth
header is created; the response contains only the same model-facing result
and error fields as the Assistant ``confluence_read`` tool.
"""

from __future__ import annotations

import asyncio
import base64
import hmac
import html
import ipaddress
import json
import os
import re
import socket
import time
import urllib.parse
import uuid
from html.parser import HTMLParser
from typing import Any, Literal

from ai_gateway_contracts.capability_proof import (
    CapabilityProofError,
    canonical_body_hash,
    verify_capability_proof,
)
from ai_gateway_core.connectors.confluence_format import (
    escape_storage_text,
    markdown_to_storage,
)
from ai_gateway_core.persistence.repositories.mcp_repository import (
    MCPAuthorizationError,
    MCPRepositoryError,
)
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, model_validator

router = APIRouter(
    prefix="/internal/v2/agent-capabilities",
    tags=["internal-agent-capabilities"],
)

_DOMAIN = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$")
_PAGE_ID = re.compile(r"^\d{1,20}$")
_SPACE_KEY = re.compile(r"^[A-Za-z0-9]{1,255}$")
_ENV_NAME = re.compile(r"^[A-Z][A-Z0-9_]*$")
_MAX_RESPONSE_BYTES = 2_000_000
_MAX_CONTENT_CHARS = 20_000
_PENDING_RECEIPT_SCHEMA = "ai-platform/capability-broker-dispatch/v1"
_DURABLE_RECEIPT_SCHEMA = "ai-platform/durable-capability-receipt/v1"
_ANTI_HALLUCINATION_NOTE = (
    " | ⚠️ This call FAILED. Do not tell the user the action succeeded — "
    "explain what went wrong and what they should do next."
)


class ConfiguredEnvironmentSecretResolver:
    """Resolve opaque Secret Store refs through an operator allowlist.

    The database stores only ``secret_ref``.  The mapping names an environment
    variable, never a credential value, and is deliberately kept on the
    Gateway process so the Runtime/Worker never receives either one.
    """

    def __init__(self, reference_map: dict[str, str] | None = None) -> None:
        self._reference_map = {
            str(reference): str(env_name)
            for reference, env_name in (reference_map or {}).items()
            if isinstance(reference, str)
            and isinstance(env_name, str)
            and _ENV_NAME.fullmatch(env_name)
        }

    @classmethod
    def from_env(cls) -> ConfiguredEnvironmentSecretResolver:
        raw = os.getenv("MCP_SECRET_REF_MAP", "").strip()
        if not raw:
            return cls({})
        try:
            value = json.loads(raw)
        except json.JSONDecodeError:
            return cls({})
        return cls(value if isinstance(value, dict) else {})

    @property
    def ready(self) -> bool:
        """Whether at least one allowlisted ref has a configured value."""

        return any(os.getenv(env_name, "") for env_name in self._reference_map.values())

    async def resolve(self, secret_ref: str) -> str:
        env_name = self._reference_map.get(secret_ref)
        value = os.getenv(env_name, "") if env_name else ""
        if not value:
            raise RuntimeError("MCP_SECRET_UNAVAILABLE")
        return value

    def __repr__(self) -> str:
        return f"ConfiguredEnvironmentSecretResolver(refs={len(self._reference_map)})"


class _PinnedResolver:
    """Resolve exactly the addresses checked by ``_public_addresses``.

    aiohttp still sends the original hostname in the URL, preserving TLS SNI
    and the HTTP Host header, while the connector never performs a second DNS
    lookup that could be swapped after validation.
    """

    def __init__(self, hostname: str, addresses: list[tuple[str, int]]) -> None:
        self._hostname = hostname
        self._addresses = tuple(addresses)

    async def resolve(
        self, host: str, port: int = 0, family: int = socket.AF_UNSPEC
    ) -> list[dict[str, Any]]:
        if host.lower().rstrip(".") != self._hostname.lower().rstrip("."):
            raise OSError("destination is not pinned")
        resolved: list[dict[str, Any]] = []
        for address, checked_port in self._addresses:
            ip = ipaddress.ip_address(address)
            if not ip.is_global:
                raise OSError("pinned address is not public")
            ip_family = socket.AF_INET6 if ip.version == 6 else socket.AF_INET
            if family not in (socket.AF_UNSPEC, ip_family):
                continue
            resolved.append(
                {
                    "hostname": self._hostname,
                    "host": address,
                    "port": checked_port if checked_port else port,
                    "family": socket.AF_INET6 if ip.version == 6 else socket.AF_INET,
                    "proto": 6,
                    "flags": 0,
                }
            )
        if not resolved:
            raise OSError("no pinned address matches requested family")
        return resolved

    async def close(self) -> None:
        return None


class ConfluenceReadRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    action: Literal["search", "read_page", "list_spaces", "get_space", "list_children"]
    query: str | None = Field(default=None, max_length=4096)
    page_id: str | None = Field(default=None, max_length=64)
    title: str | None = Field(default=None, max_length=4096)
    url: str | None = Field(default=None, max_length=4096)
    space_key: str | None = Field(default=None, max_length=255)
    space_id: str | None = Field(default=None, max_length=64)
    keys: list[str] | None = Field(default=None, max_length=250)
    space_type: Literal["global", "personal", "collaboration", "knowledge_base"] | None = None
    labels: list[str] | None = Field(default=None, max_length=250)
    fields: list[Literal["title", "text"]] | None = Field(default=None, max_length=2)
    author: str | None = Field(default=None, max_length=1024)
    updated_since: str | None = Field(default=None, max_length=64)
    under_page_id: str | None = Field(default=None, max_length=64)
    cql: str | None = Field(default=None, max_length=4096)
    limit: int | None = Field(default=None, ge=1, le=250)


class ConfluenceConnectorBinding(BaseModel):
    """The only connector identity the Worker may forward to this broker."""

    model_config = ConfigDict(extra="forbid")

    binding_type: Literal["grant", "catalog"]
    provider: Literal["confluence"]
    tool_name: Literal["confluence_read"]
    principal_type: Literal["service_account", "user_delegated"] | None = None
    grant_id: str | None = Field(default=None, min_length=1, max_length=64)
    channel: Literal[
        "preview",
        "hosted",
        "hosted_private",
        "hosted_public",
        "embed",
        "api",
        "builtin",
    ]

    @model_validator(mode="after")
    def _validate_grant_shape(self) -> ConfluenceConnectorBinding:
        if self.binding_type == "catalog":
            if self.principal_type is not None or self.grant_id is not None:
                raise ValueError("catalog connector binding cannot carry a grant")
        elif self.principal_type is None or self.grant_id is None:
            raise ValueError("grant connector binding requires principal and grant")
        if self.grant_id is not None:
            try:
                uuid.UUID(self.grant_id)
            except (ValueError, AttributeError, TypeError):
                raise ValueError("grant connector binding id is invalid") from None
        return self


class ConfluenceReadEnvelope(BaseModel):
    model_config = ConfigDict(extra="forbid")

    arguments: ConfluenceReadRequest
    binding: ConfluenceConnectorBinding


class ConfluenceWriteRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    action: Literal[
        "create_page",
        "update_page",
        "find_replace",
        "move_page",
        "comment",
        "delete_page",
    ]
    page_id: str | None = Field(default=None, max_length=64)
    space_key: str | None = Field(default=None, max_length=255)
    title: str | None = Field(default=None, max_length=4096)
    content: str | None = Field(default=None, max_length=100_000)
    parent_id: str | None = Field(default=None, max_length=64)
    target_parent_id: str | None = Field(default=None, max_length=64)
    find: str | None = Field(default=None, max_length=20_000)
    replace: str | None = Field(default=None, max_length=20_000)
    raw_html: bool = False
    body: str | None = Field(default=None, max_length=20_000)


class ConfluenceWriteBinding(BaseModel):
    model_config = ConfigDict(extra="forbid")

    binding_type: Literal["grant", "catalog"]
    provider: Literal["confluence"]
    tool_name: Literal["confluence_write"]
    principal_type: Literal["service_account", "user_delegated"] | None = None
    grant_id: str | None = Field(default=None, min_length=1, max_length=64)
    channel: Literal[
        "preview",
        "hosted",
        "hosted_private",
        "hosted_public",
        "embed",
        "api",
        "builtin",
    ]

    @model_validator(mode="after")
    def _validate_grant_shape(self) -> ConfluenceWriteBinding:
        if self.binding_type == "catalog":
            if self.principal_type is not None or self.grant_id is not None:
                raise ValueError("catalog connector binding cannot carry a grant")
        elif self.principal_type is None or self.grant_id is None:
            raise ValueError("grant connector binding requires principal and grant")
        if self.grant_id is not None:
            try:
                uuid.UUID(self.grant_id)
            except (ValueError, AttributeError, TypeError):
                raise ValueError("grant connector binding id is invalid") from None
        return self


class ConfluenceWriteEnvelope(BaseModel):
    model_config = ConfigDict(extra="forbid")

    arguments: ConfluenceWriteRequest
    arguments_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    binding: ConfluenceWriteBinding

    @model_validator(mode="before")
    @classmethod
    def _validate_arguments_hash(cls, value: Any) -> Any:
        if isinstance(value, dict) and isinstance(value.get("arguments"), dict):
            expected = f"sha256:{canonical_body_hash(value['arguments'])}"
            if value.get("arguments_hash") != expected:
                raise ValueError("arguments hash mismatch")
        return value


class _TextExtractor(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self._skip = 0

    def handle_starttag(self, tag: str, _attrs: list[tuple[str, str | None]]) -> None:
        if tag.lower() in {"script", "style"}:
            self._skip += 1
        elif not self._skip and tag.lower() in {"br", "p", "div", "li", "h1", "h2", "h3"}:
            self.parts.append("\n")

    def handle_endtag(self, tag: str) -> None:
        if tag.lower() in {"script", "style"} and self._skip:
            self._skip -= 1
        elif not self._skip and tag.lower() in {"p", "div", "li", "h1", "h2", "h3"}:
            self.parts.append("\n")

    def handle_data(self, data: str) -> None:
        if not self._skip:
            self.parts.append(data)


def _html_text(value: str) -> str:
    parser = _TextExtractor()
    parser.feed(value)
    text = html.unescape("".join(parser.parts))
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text).strip()
    return text[:_MAX_CONTENT_CHARS] + (
        "\n…[truncated at 20K chars]" if len(text) > _MAX_CONTENT_CHARS else ""
    )


def _auth_error(status: int, action: str) -> str:
    if status == 401:
        msg = f"Authentication failed for action={action!r} (HTTP 401). The stored API token is invalid or expired — ask the user to re-connect Confluence in the integrations panel."
    elif status == 403:
        msg = f"Permission denied for action={action!r} (HTTP 403). The connected account does not have access to this page or space."
    elif status == 404:
        msg = f"Resource not found for action={action!r} (HTTP 404). Verify the supplied page or space identifier."
    elif status == 429:
        msg = f"Rate limited for action={action!r} (HTTP 429). Back off and try again in a moment."
    else:
        msg = f"Confluence API error (HTTP {status}) for action={action!r}."
    return msg + _ANTI_HALLUCINATION_NOTE


def _confluence_api_root(target: str) -> tuple[str, str]:
    """Split a validated target prefix into its pinned host and API root."""

    host, _, prefix = target.partition("/")
    if not _DOMAIN.fullmatch(host):
        raise HTTPException(status_code=503, detail="confluence destination unavailable")
    prefix = prefix.strip("/")
    root = f"https://{host}/"
    if prefix:
        root += f"{prefix}/"
    return host, f"{root}wiki/"


def _format_search(data: dict[str, Any], query: str) -> tuple[str, dict[str, Any]]:
    hits = data.get("results") or []
    cql = str(data.get("_cql") or "")
    strategy = str(data.get("_strategy") or "?")
    if not hits:
        return f"No pages matched. CQL executed: `{cql}`", {
            "count": 0,
            "query": query,
            "cql_used": cql,
            "strategy": strategy,
        }
    lines = [f"Found {len(hits)} Confluence page(s) (strategy={strategy}, cql=`{cql}`):"]
    for page in hits:
        ancestors = page.get("ancestors") or []
        parent = ancestors[-1] if ancestors else {}
        parent_line = (
            f"Parent: {parent.get('title', '')} (id: {parent.get('id', '')})"
            if parent
            else "Parent: (top-level — space homepage)"
        )
        lines.append(
            f"\n**{page.get('title', '')}** (Space: {page.get('space', {}).get('name', '')}, key: {page.get('space', {}).get('key', '')})\n"
            f"ID: {page.get('id', '')} | URL: {page.get('_links', {}).get('webui', '')}\n{parent_line}\n"
            f"{_html_text(str(page.get('body', {}).get('view', {}).get('value', '')))[:800]}"
        )
    return "\n---\n".join(lines), {
        "count": len(hits),
        "query": query,
        "cql_used": cql,
        "strategy": strategy,
    }


def _format_spaces(spaces: list[dict[str, Any]], query: str | None) -> tuple[str, dict[str, Any]]:
    if not spaces:
        return (
            f"No Confluence spaces matched query={query!r}. Try listing without a query, or different keywords.",
            {"count": 0, "query": query, "shown": 0},
        )
    lines = [
        f"Found {len(spaces)} Confluence space(s)" + (f" matching '{query}'" if query else "") + ":"
    ]
    for space in spaces[:25]:
        lines.append(
            f"- **{space.get('name', '')}** (key: `{space.get('key', '')}`, id: {space.get('id', '')}, type: {space.get('type', '')})\n  URL: {space.get('_links', {}).get('webui', '')}"
        )
    return "\n".join(lines), {"count": len(spaces), "query": query, "shown": min(len(spaces), 25)}


def _format_children(children: list[dict[str, Any]], page_id: str) -> tuple[str, dict[str, Any]]:
    if not children:
        return f"Page {page_id} has no direct child pages.", {"count": 0, "page_id": page_id}
    lines = [f"Found {len(children)} child page(s) of {page_id}:"]
    for child in children:
        lines.append(
            f"- **{child.get('title', '')}** (id: {child.get('id', '')})\n  URL: {child.get('_links', {}).get('webui', '')}"
        )
    return "\n".join(lines), {"count": len(children), "page_id": page_id}


def _validate_scope(value: str | None) -> str:
    if not value or len(value) > 255 or any(ord(char) < 32 for char in value):
        raise HTTPException(status_code=403, detail="scope is invalid")
    return value


async def _authorize(
    request: Request,
    payload: dict[str, Any],
    internal_token: str | None,
    tenant_id: str | None,
    user_id: str | None,
    session_id: str | None,
    proof: str | None,
    execution_id: str | None,
    run_id: str | None,
    capability_path: str = "/internal/v2/agent-capabilities/confluence/read",
) -> tuple[str, str, str]:
    expected = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected or not internal_token or not hmac.compare_digest(internal_token, expected):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    tenant, user, session = (
        _validate_scope(tenant_id),
        _validate_scope(user_id),
        _validate_scope(session_id),
    )
    proof_secret = os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
    if not proof_secret or not proof or not execution_id or not run_id:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        verify_capability_proof(
            proof_secret,
            proof,
            method="POST",
            path=capability_path,
            body=payload,
            tenant_id=tenant,
            user_id=user,
            session_id=session,
            execution_id=execution_id,
            run_id=run_id,
        )
    except CapabilityProofError:
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    return tenant, user, session


async def _public_addresses(domain: str) -> list[tuple[str, int]]:
    try:
        addresses = await asyncio.to_thread(
            socket.getaddrinfo, domain, 443, type=socket.SOCK_STREAM
        )
    except OSError:
        raise HTTPException(status_code=503, detail="confluence destination unavailable") from None
    unique = sorted({(item[4][0], item[4][1]) for item in addresses})
    if not unique or any(not ipaddress.ip_address(host).is_global for host, _ in unique):
        raise HTTPException(status_code=403, detail="confluence destination blocked")
    return unique


async def _connection(
    request: Request,
    tenant_id: str,
    *,
    user_id: str = "",
    binding: ConfluenceConnectorBinding | ConfluenceWriteBinding | None = None,
) -> tuple[str, str, list[tuple[str, int]]]:
    database = getattr(request.app.state, "database", None)
    if (
        database is None
        or getattr(database, "enabled", False) is not True
        or getattr(database, "_pool", None) is None
    ):
        raise HTTPException(status_code=503, detail="confluence connection unavailable")
    if binding is None:
        raise HTTPException(status_code=403, detail="confluence connector binding required")
    repository = getattr(request.app.state, "mcp_repository", None)
    if repository is None:
        raise HTTPException(
            status_code=503, detail="confluence connector authorization unavailable"
        )
    authenticated = binding.channel not in {"hosted_public", "embed"}
    try:
        if binding.binding_type == "grant":
            row = await repository.authorize_connector_tool(
                tenant_id=tenant_id,
                user_id=user_id,
                authenticated=authenticated,
                provider=binding.provider,
                tool_name=binding.tool_name,
                principal_type=str(binding.principal_type or ""),
                grant_id=str(binding.grant_id or ""),
                channel=binding.channel,
            )
        else:
            row = await repository.authorize_connector_catalog(
                tenant_id=tenant_id,
                user_id=user_id,
                authenticated=authenticated,
                provider=binding.provider,
                tool_name=binding.tool_name,
                channel=binding.channel,
            )
    except MCPAuthorizationError:
        raise HTTPException(
            status_code=403, detail="confluence connector authorization denied"
        ) from None
    except MCPRepositoryError:
        raise HTTPException(
            status_code=503, detail="confluence connector authorization unavailable"
        ) from None
    except Exception:
        raise HTTPException(
            status_code=503, detail="confluence connector authorization unavailable"
        ) from None
    if binding.binding_type == "catalog":
        try:
            from ..v1.connectors import resolve_catalog_connector_credential

            resolved = await resolve_catalog_connector_credential(
                request,
                tenant_id=tenant_id,
                user_id=user_id,
                provider=binding.provider,
            )
        except PermissionError:
            raise HTTPException(
                status_code=403, detail="confluence connector authorization denied"
            ) from None
        except Exception:
            raise HTTPException(status_code=503, detail="confluence credential unavailable") from None
        token = str(resolved.get("access_token") or "")
        metadata = resolved.get("provider_metadata")
        metadata = metadata if isinstance(metadata, dict) else {}
        cloud_id = str(metadata.get("cloud_id") or "").strip()
        site_url = str(metadata.get("site_url") or metadata.get("domain") or "").strip()
        parsed = urllib.parse.urlsplit(site_url if "://" in site_url else f"https://{site_url}")
        site_domain = parsed.hostname or ""
        if not token:
            raise HTTPException(status_code=503, detail="confluence credential unavailable")
        if cloud_id:
            if not re.fullmatch(r"[A-Za-z0-9._:-]{1,255}", cloud_id):
                raise HTTPException(status_code=503, detail="confluence credential unavailable")
            domain = f"api.atlassian.com/ex/confluence/{cloud_id}"
            address_domain = "api.atlassian.com"
        else:
            if not _DOMAIN.fullmatch(site_domain):
                raise HTTPException(status_code=503, detail="confluence credential unavailable")
            domain = site_domain
            address_domain = site_domain
        addresses = await _public_addresses(address_domain)
        return domain, "Bearer " + token, addresses
    row_get = getattr(row, "get", None)
    secret_ref = str(row_get("secret_ref") or "") if callable(row_get) else ""
    metadata = row_get("connection_metadata") if callable(row_get) else None
    metadata = metadata if isinstance(metadata, dict) else {}
    if not secret_ref:
        raise HTTPException(status_code=503, detail="confluence credential unavailable")
    resolver = getattr(request.app.state, "mcp_secret_resolver", None)
    resolve_secret = getattr(resolver, "resolve", None)
    if not callable(resolve_secret):
        raise HTTPException(status_code=503, detail="confluence secret resolver unavailable")
    try:
        token = await resolve_secret(secret_ref)
    except Exception:
        raise HTTPException(status_code=503, detail="confluence credential unavailable") from None
    domain = str(metadata.get("domain") or "").strip()
    email = str(metadata.get("email") or "").strip()
    if not _DOMAIN.fullmatch(domain) or not email or not token:
        raise HTTPException(status_code=503, detail="confluence credential unavailable")
    addresses = await _public_addresses(domain)
    return domain, "Basic " + base64.b64encode(f"{email}:{token}".encode()).decode(), addresses


async def _upstream(
    domain: str,
    auth: str,
    addresses: list[tuple[str, int]],
    path: str,
    *,
    params: dict[str, Any],
) -> tuple[int, dict[str, Any]]:
    try:
        import aiohttp
    except ImportError:
        raise HTTPException(
            status_code=503, detail="confluence HTTP adapter is unavailable"
        ) from None
    host, root = _confluence_api_root(domain)
    base = f"{root}api/v2/" if path.startswith("v2/") else f"{root}rest/api/"
    normalized_path = path.removeprefix("v2/") if path.startswith("v2/") else path
    resolver = _PinnedResolver(host, addresses)
    connector = aiohttp.TCPConnector(
        resolver=resolver,
        use_dns_cache=False,
        ssl=True,
        enable_cleanup_closed=True,
    )
    timeout = aiohttp.ClientTimeout(total=15.0, connect=5.0, sock_read=15.0)
    async with aiohttp.ClientSession(
        connector=connector,
        timeout=timeout,
        trust_env=False,
    ) as client:
        try:
            response = await client.get(
                f"{base}{normalized_path.lstrip('/')}",
                params=params,
                headers={"Authorization": auth, "Accept": "application/json"},
                allow_redirects=False,
            )
        except (aiohttp.ClientError, asyncio.TimeoutError):
            raise HTTPException(status_code=502, detail="confluence upstream unavailable") from None
        if response.status >= 400:
            raise HTTPException(
                status_code=502, detail=_auth_error(response.status, "confluence_read")
            )
        raw = await _read_bounded_body(response)
        try:
            return response.status, json.loads(raw)
        except ValueError:
            raise HTTPException(status_code=502, detail="confluence response invalid") from None


def _storage_text(value: str, *, raw_html: bool = False) -> str:
    """Encode user text as Confluence storage XHTML.

    The Gateway never treats model-provided markup as storage markup unless
    the explicit ``raw_html`` escape hatch is set.  This mirrors the legacy
    client while keeping the broker independent from the Python Assistant.
    """

    if raw_html:
        return value
    return markdown_to_storage(value)


def _external_url(data: dict[str, Any]) -> str:
    links = data.get("_links") if isinstance(data.get("_links"), dict) else {}
    value = str(links.get("base") or "") + str(links.get("webui") or "")
    return value if value.startswith("https://") else ""


async def _upstream_write(
    domain: str,
    auth: str,
    addresses: list[tuple[str, int]],
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
    *,
    params: dict[str, Any] | None = None,
) -> tuple[int, dict[str, Any]]:
    """Perform exactly one pinned write request; never retry side effects."""

    try:
        import aiohttp
    except ImportError:
        raise HTTPException(status_code=503, detail="confluence HTTP adapter is unavailable") from None
    host, root = _confluence_api_root(domain)
    connector = aiohttp.TCPConnector(
        resolver=_PinnedResolver(host, addresses),
        use_dns_cache=False,
        ssl=True,
        enable_cleanup_closed=True,
    )
    timeout = aiohttp.ClientTimeout(total=15.0, connect=5.0, sock_read=15.0)
    async with aiohttp.ClientSession(
        connector=connector,
        timeout=timeout,
        trust_env=False,
    ) as client:
        try:
            response = await client.request(
                method.upper(),
                f"{root}rest/api/{path.lstrip('/')}",
                json=body,
                params=params,
                headers={
                    "Authorization": auth,
                    "Accept": "application/json",
                    "Content-Type": "application/json",
                },
                allow_redirects=False,
            )
        except (aiohttp.ClientError, asyncio.TimeoutError):
            raise HTTPException(
                status_code=502,
                detail="confluence upstream unavailable" + _ANTI_HALLUCINATION_NOTE,
            ) from None
        if 400 <= response.status < 500:
            raise HTTPException(
                status_code=response.status,
                detail=_auth_error(response.status, "confluence_write"),
            )
        if response.status >= 500:
            raise HTTPException(
                status_code=502,
                detail="confluence upstream unavailable" + _ANTI_HALLUCINATION_NOTE,
            )
        if response.status == 204:
            return response.status, {}
        raw = await _read_bounded_body(response)
        if not raw:
            return response.status, {}
        try:
            value = json.loads(raw)
        except ValueError:
            raise HTTPException(
                status_code=502,
                detail="confluence response invalid" + _ANTI_HALLUCINATION_NOTE,
            ) from None
        if not isinstance(value, dict):
            raise HTTPException(
                status_code=502,
                detail="confluence response invalid" + _ANTI_HALLUCINATION_NOTE,
            )
        return response.status, value


async def _read_bounded_body(response: Any) -> bytes:
    content_length = getattr(response, "content_length", None)
    if isinstance(content_length, int) and content_length > _MAX_RESPONSE_BYTES:
        raise HTTPException(status_code=502, detail="confluence response too large")
    chunks: list[bytes] = []
    total = 0
    async for chunk in response.content.iter_chunked(64 * 1024):
        total += len(chunk)
        if total > _MAX_RESPONSE_BYTES:
            raise HTTPException(status_code=502, detail="confluence response too large")
        chunks.append(chunk)
    return b"".join(chunks)


def _durable_confluence_result(value: Any) -> dict[str, Any] | None:
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != _DURABLE_RECEIPT_SCHEMA
        or value.get("capability_id") != "confluence_write"
        or not isinstance(value.get("result"), dict)
    ):
        return None
    result = value["result"]
    if set(result) != {
        "receipt_id",
        "capability_id",
        "external_id",
        "external_url",
        "artifacts",
    } or result.get("capability_id") != "confluence_write":
        return None
    if not isinstance(result.get("receipt_id"), str) or not result["receipt_id"]:
        return None
    if result.get("external_id") is not None and not isinstance(
        result["external_id"], str
    ):
        return None
    if result.get("external_url") is not None and (
        not isinstance(result["external_url"], str)
        or not result["external_url"].startswith("https://")
    ):
        return None
    if result.get("artifacts") != []:
        return None
    return result


def _confluence_gateway_response(result: dict[str, Any]) -> dict[str, Any]:
    return {
        "receipt_id": result["receipt_id"],
        "external_id": result["external_id"] or "",
        "external_url": result["external_url"] or "",
        "artifacts": [],
    }


async def _confluence_execution_state(
    request: Request,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    run_id: str,
    tool_call_id: str,
    arguments_hash: str,
) -> dict[str, Any] | None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=424, detail="capability execution store unavailable")
    try:
        execution_uuid = uuid.UUID(execution_id)
        run_uuid = uuid.UUID(run_id)
    except ValueError:
        raise HTTPException(status_code=403, detail="capability scope invalid") from None
    row = await pool.fetchrow(
        """SELECT result_summary, capability_id, effect, approval_status, status,
                  tool_call_id, arguments_sha256
             FROM assistant_capability_executions
            WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3
              AND session_id=$4 AND run_id=$5""",
        execution_uuid,
        tenant_id,
        user_id,
        session_id,
        run_uuid,
    )
    if (
        not row
        or row["capability_id"] != "confluence_write"
        or row["effect"] != "write"
        or row["approval_status"] != "consumed"
        or row["status"] not in {"dispatched", "running"}
        or row["tool_call_id"] != tool_call_id
        or str(row["arguments_sha256"]).strip()
        != arguments_hash.removeprefix("sha256:")
    ):
        raise HTTPException(status_code=403, detail="capability execution not active")
    existing = _durable_confluence_result(row["result_summary"])
    if existing is not None:
        return existing
    if row["result_summary"] is not None:
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")
    return None


async def _claim_confluence_execution(request: Request, execution_id: str) -> None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")
    pending = {
        "schema_version": _PENDING_RECEIPT_SCHEMA,
        "capability_id": "confluence_write",
    }
    changed = await pool.execute(
        "UPDATE assistant_capability_executions SET result_summary=$2, updated_at=NOW() "
        "WHERE execution_id=$1 AND result_summary IS NULL "
        "AND status IN ('dispatched','running')",
        uuid.UUID(execution_id),
        json.dumps(pending),
    )
    if not str(changed).endswith(" 1"):
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")


async def _clear_confluence_claim(request: Request, execution_id: str) -> None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")
    changed = await pool.execute(
        "UPDATE assistant_capability_executions SET result_summary=NULL, updated_at=NOW() "
        "WHERE execution_id=$1 AND result_summary->>'schema_version'=$2",
        uuid.UUID(execution_id),
        _PENDING_RECEIPT_SCHEMA,
    )
    if not str(changed).endswith(" 1"):
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")


async def _store_confluence_receipt(
    request: Request, execution_id: str, result: dict[str, Any]
) -> None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")
    receipt = {
        "schema_version": _DURABLE_RECEIPT_SCHEMA,
        "capability_id": "confluence_write",
        "result": result,
    }
    changed = await pool.execute(
        "UPDATE assistant_capability_executions SET result_summary=$2, updated_at=NOW() "
        "WHERE execution_id=$1 AND result_summary->>'schema_version'=$3",
        uuid.UUID(execution_id),
        json.dumps(receipt),
        _PENDING_RECEIPT_SCHEMA,
    )
    if not str(changed).endswith(" 1"):
        raise HTTPException(status_code=502, detail="confluence write outcome unknown")


def _search_cql(payload: ConfluenceReadRequest) -> tuple[str, str]:
    if payload.cql:
        if any(ord(char) < 32 or ord(char) == 127 for char in payload.cql):
            raise HTTPException(
                status_code=400, detail="raw cql must be single-line, no control chars"
            )
        return payload.cql, "raw_cql"
    if not any((payload.query, payload.author, payload.updated_since, payload.under_page_id)):
        raise HTTPException(
            status_code=400,
            detail="`search` needs at least one of: query, cql, author, updated_since, or under_page_id",
        )
    parts = ["type=page"]
    if payload.query:
        if any(ord(char) < 32 or ord(char) == 127 for char in payload.query):
            raise HTTPException(status_code=400, detail="query contains control characters")
        query = payload.query.replace("\\", "\\\\").replace('"', '\\"')
        fields = set(payload.fields or ["title", "text"])
        clauses = []
        if "title" in fields:
            title_query = re.sub(r"[【】「」『』\[\]()（）]", " ", query).strip()
            clauses.append(f'title ~ "{title_query}"')
        if "text" in fields:
            clauses.append(f'text ~ "{query}"')
        parts.append("(" + " OR ".join(clauses) + ")")
        strategy = "title+text" if fields == {"title", "text"} else next(iter(fields))
    else:
        strategy = "filter_only"
    if payload.space_key:
        if not _SPACE_KEY.fullmatch(payload.space_key):
            raise HTTPException(status_code=400, detail=f"invalid space_key: {payload.space_key!r}")
        parts.append(f"space={payload.space_key}")
    if payload.under_page_id:
        if not _PAGE_ID.fullmatch(payload.under_page_id):
            raise HTTPException(status_code=400, detail="invalid under_page_id (must be numeric)")
        parts.append(f"ancestor={payload.under_page_id}")
    if payload.author:
        if any(ord(char) < 32 or ord(char) == 127 for char in payload.author):
            raise HTTPException(status_code=400, detail="author contains control characters")
        parts.append(
            f'creator = "{payload.author.replace(chr(92), chr(92) * 2).replace(chr(34), chr(92) + chr(34))}"'
        )
    if payload.updated_since:
        if not re.fullmatch(
            r"\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?)?", payload.updated_since
        ):
            raise HTTPException(
                status_code=400, detail="updated_since must be ISO date (YYYY-MM-DD)"
            )
        parts.append(f'lastModified >= "{payload.updated_since}"')
    return " AND ".join(parts), strategy


@router.post("/confluence/read")
async def read_confluence(
    request: Request,
    payload: ConfluenceReadEnvelope = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
) -> dict[str, Any]:
    started = time.monotonic()
    raw = payload.model_dump(mode="json", exclude_unset=True)
    tenant_id, user_id, _ = await _authorize(
        request,
        raw,
        x_ai_platform_internal_token,
        x_ai_tenant_id,
        x_ai_user_id,
        x_ai_session_id,
        x_ai_capability_proof,
        x_ai_execution_id,
        x_ai_run_id,
    )
    if getattr(request.app.state, "confluence_capability_enabled", True) is not True:
        raise HTTPException(status_code=404, detail="confluence capability unavailable")
    domain, auth, addresses = await _connection(
        request,
        tenant_id,
        user_id=user_id,
        binding=payload.binding,
    )
    arguments = payload.arguments
    try:
        if arguments.action == "search":
            cql, strategy = _search_cql(arguments)
            _, data = await _upstream(
                domain,
                auth,
                addresses,
                "content/search",
                params={
                    "cql": cql,
                    "limit": min(arguments.limit or 10, 25),
                    "expand": "body.view,space,version,ancestors",
                },
            )
            data["_cql"], data["_strategy"] = cql, strategy
            result, metadata = _format_search(data, arguments.query or "")
        elif arguments.action == "read_page":
            page_id = (
                arguments.page_id
                or (
                    re.search(r"/pages/(\d+)(?:/|$)", arguments.url or "")
                    or re.search(r"[?&]pageId=(\d+)", arguments.url or "", re.I)
                    or [None, None]
                )[1]
            )
            if page_id and not _PAGE_ID.fullmatch(page_id):
                raise HTTPException(status_code=400, detail="invalid page_id (must be numeric)")
            path = f"content/{page_id}" if page_id else "content"
            params = {"expand": "body.view,space,version,ancestors"}
            if not page_id:
                if not arguments.title:
                    raise HTTPException(
                        status_code=400, detail="`read_page` requires page_id, title, or url"
                    )
                params.update({"title": arguments.title, "limit": 1})
            _, data = await _upstream(domain, auth, addresses, path, params=params)
            page = (data.get("results") or [None])[0] if not page_id else data
            if not page:
                result, metadata = (
                    f"No page found (page_id={page_id!r}, title={arguments.title!r}).",
                    {"found": False},
                )
            else:
                ancestors = [
                    {"id": str(item.get("id", "")), "title": item.get("title", "")}
                    for item in page.get("ancestors", [])
                    if item.get("id")
                ]
                parent = ancestors[-1] if ancestors else {}
                content = _html_text(str(page.get("body", {}).get("view", {}).get("value", "")))
                breadcrumb = " > ".join(
                    "{} ({})".format(item.get("title", ""), item.get("id", ""))
                    for item in ancestors
                )
                parent_line = (
                    f"Parent: {parent.get('title', '')} (id: {parent.get('id', '')})\n"
                    f"Path: {breadcrumb} > {page.get('title', '')}\n"
                    if parent
                    else "Parent: (none — this is a space homepage or top-level page)\n"
                )
                result = f"**{page.get('title', '')}** (space: {page.get('space', {}).get('name', '')}, id: {page.get('id', '')})\nURL: {page.get('_links', {}).get('base', '')}{page.get('_links', {}).get('webui', '')}\n{parent_line}Last modified: {page.get('version', {}).get('when', '')}\n\n{content}"
                metadata = {
                    "found": True,
                    "page_id": page.get("id"),
                    "parent_id": parent.get("id", ""),
                    "space_key": page.get("space", {}).get("key"),
                    "chars": len(content),
                }
        elif arguments.action == "list_spaces":
            _, data = await _upstream(
                domain,
                auth,
                addresses,
                "v2/spaces",
                params={
                    "limit": min(arguments.limit or 50, 250),
                    "include-icon": "false",
                    "description-format": "plain",
                    **({"keys": ",".join(arguments.keys)} if arguments.keys else {}),
                    **({"type": arguments.space_type} if arguments.space_type else {}),
                    **({"labels": ",".join(arguments.labels)} if arguments.labels else {}),
                },
            )
            spaces = data.get("results", []) or []
            if arguments.query:
                needle = arguments.query.lower()
                spaces = [
                    space
                    for space in spaces
                    if needle
                    in " ".join(
                        str(space.get(key) or "") for key in ("name", "key", "description")
                    ).lower()
                ]
            result, metadata = _format_spaces(spaces, arguments.query)
        elif arguments.action == "get_space":
            if not arguments.space_id and not arguments.space_key:
                raise HTTPException(
                    status_code=400, detail="`get_space` requires space_id or space_key"
                )
            if arguments.space_id and not _PAGE_ID.fullmatch(arguments.space_id):
                raise HTTPException(status_code=400, detail="invalid space_id (must be numeric)")
            if arguments.space_key and not _SPACE_KEY.fullmatch(arguments.space_key):
                raise HTTPException(status_code=400, detail="invalid space_key")
            path = f"v2/spaces/{arguments.space_id}" if arguments.space_id else "v2/spaces"
            params = {"description-format": "plain", "include-icon": "false"}
            if arguments.space_key:
                params.update({"keys": arguments.space_key, "limit": 1})
            _, data = await _upstream(domain, auth, addresses, path, params=params)
            space = (data.get("results") or [None])[0] if arguments.space_key else data
            if not space:
                result, metadata = (
                    f"No space found (id={arguments.space_id!r}, key={arguments.space_key!r}).",
                    {"found": False},
                )
            else:
                result = f"**{space.get('name', '')}** (key: `{space.get('key', '')}`, id: {space.get('id', '')}, type: {space.get('type', '')})\nURL: {space.get('_links', {}).get('webui', '')}\nHomepage ID: {space.get('homepageId') or '(none)'}\nCreated: {space.get('createdAt') or '(unknown)'}\nDescription: {space.get('description') or '(no description)'}"
                metadata = {
                    "found": True,
                    "space_id": space.get("id"),
                    "space_key": space.get("key"),
                }
        else:
            page_id = arguments.page_id or ""
            if not _PAGE_ID.fullmatch(page_id):
                raise HTTPException(status_code=400, detail="`list_children` requires page_id")
            _, data = await _upstream(
                domain,
                auth,
                addresses,
                f"content/{page_id}/child/page",
                params={"limit": min(arguments.limit or 25, 100), "expand": "version,_links"},
            )
            result, metadata = _format_children(data.get("results", []) or [], page_id)
        return {
            "success": True,
            "result": result,
            "duration_ms": (time.monotonic() - started) * 1000,
            "metadata": metadata,
        }
    except HTTPException:
        raise
    except Exception:
        raise HTTPException(
            status_code=502,
            detail="confluence_read failed: upstream unavailable" + _ANTI_HALLUCINATION_NOTE,
        ) from None


@router.post("/confluence/write")
async def write_confluence(
    request: Request,
    payload: ConfluenceWriteEnvelope = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
    x_ai_tool_call_id: str | None = Header(default=None),
) -> dict[str, Any]:
    """Execute one tenant-authorized Confluence write without retrying side effects."""

    raw = payload.model_dump(mode="json", exclude_unset=True)
    tenant_id, user_id, session_id = await _authorize(
        request,
        raw,
        x_ai_platform_internal_token,
        x_ai_tenant_id,
        x_ai_user_id,
        x_ai_session_id,
        x_ai_capability_proof,
        x_ai_execution_id,
        x_ai_run_id,
        capability_path="/internal/v2/agent-capabilities/confluence/write",
    )
    if getattr(request.app.state, "confluence_capability_enabled", True) is not True:
        raise HTTPException(status_code=404, detail="confluence capability unavailable")
    execution_id = _validate_scope(x_ai_execution_id)
    run_id = _validate_scope(x_ai_run_id)
    tool_call_id = _validate_scope(x_ai_tool_call_id)
    try:
        receipt_id = str(uuid.UUID(execution_id))
    except (ValueError, TypeError, AttributeError):
        raise HTTPException(status_code=403, detail="execution identity is invalid") from None
    existing = await _confluence_execution_state(
        request,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        execution_id=execution_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        arguments_hash=payload.arguments_hash,
    )
    if existing is not None:
        return _confluence_gateway_response(existing)
    domain, auth, addresses = await _connection(
        request,
        tenant_id,
        user_id=user_id,
        binding=payload.binding,
    )
    args = payload.arguments
    action = args.action

    def page_id(name: str = "page_id") -> str:
        value = str(getattr(args, name) or "")
        if not _PAGE_ID.fullmatch(value):
            raise HTTPException(status_code=400, detail=f"`{action}` requires numeric {name}")
        return value

    def receipt(data: dict[str, Any], fallback_id: str = "") -> dict[str, Any]:
        external_id = str(data.get("id") or fallback_id)
        return {
            "receipt_id": receipt_id,
            "external_id": external_id,
            "external_url": _external_url(data),
            "artifacts": [],
        }

    async def perform_write(
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        *,
        fallback_id: str = "",
    ) -> dict[str, Any]:
        await _claim_confluence_execution(request, execution_id)
        try:
            _, data = await _upstream_write(
                domain, auth, addresses, method, path, body
            )
        except HTTPException as error:
            if 400 <= error.status_code < 500:
                await _clear_confluence_claim(request, execution_id)
            raise
        response = receipt(data, fallback_id)
        durable_result = {
            "receipt_id": response["receipt_id"],
            "capability_id": "confluence_write",
            "external_id": response["external_id"] or None,
            "external_url": response["external_url"] or None,
            "artifacts": [],
        }
        await _store_confluence_receipt(request, execution_id, durable_result)
        return response

    async def current_page(identifier: str, expand: str) -> dict[str, Any]:
        try:
            _, current = await _upstream_write(
                domain,
                auth,
                addresses,
                "GET",
                f"content/{identifier}",
                params={"expand": expand},
            )
        except HTTPException as error:
            if error.status_code == 502:
                raise HTTPException(
                    status_code=424,
                    detail="confluence write preflight failed" + _ANTI_HALLUCINATION_NOTE,
                ) from None
            raise
        version = current.get("version")
        version = version if isinstance(version, dict) else {}
        number = version.get("number")
        if (
            isinstance(number, bool)
            or not isinstance(number, int)
            or number < 1
            or not isinstance(current.get("title"), str)
        ):
            raise HTTPException(
                status_code=424,
                detail="confluence response invalid" + _ANTI_HALLUCINATION_NOTE,
            )
        return current

    if action == "create_page":
        space_key = str(args.space_key or "")
        title = str(args.title or "").strip()
        if not _SPACE_KEY.fullmatch(space_key) or not title or args.content is None:
            raise HTTPException(status_code=400, detail="`create_page` requires space_key, title, and content")
        body: dict[str, Any] = {
            "type": "page",
            "title": title,
            "space": {"key": space_key},
            "body": {
                "storage": {
                    "value": _storage_text(args.content),
                    "representation": "storage",
                }
            },
        }
        if args.parent_id:
            if not _PAGE_ID.fullmatch(args.parent_id):
                raise HTTPException(status_code=400, detail="invalid parent_id")
            body["ancestors"] = [{"id": args.parent_id}]
        return await perform_write("POST", "content", body)

    if action == "update_page":
        identifier = page_id()
        if args.content is None:
            raise HTTPException(status_code=400, detail="`update_page` requires content")
        current = await current_page(identifier, "version,space")
        body = {
            "version": {"number": current["version"]["number"] + 1},
            "title": str(args.title or current["title"]),
            "type": "page",
            "body": {
                "storage": {
                    "value": _storage_text(args.content),
                    "representation": "storage",
                }
            },
        }
        return await perform_write("PUT", f"content/{identifier}", body, fallback_id=identifier)

    if action == "find_replace":
        identifier = page_id()
        if not args.find or args.replace is None:
            raise HTTPException(
                status_code=400,
                detail="`find_replace` requires page_id, find, and replace",
            )
        current = await current_page(identifier, "body.storage,version,space")
        storage = current.get("body")
        storage = storage if isinstance(storage, dict) else {}
        storage = storage.get("storage")
        storage = storage if isinstance(storage, dict) else {}
        current_content = storage.get("value")
        if not isinstance(current_content, str):
            raise HTTPException(
                status_code=424,
                detail="confluence response invalid" + _ANTI_HALLUCINATION_NOTE,
            )
        count = current_content.count(args.find)
        if count != 1:
            raise HTTPException(
                status_code=400,
                detail=f"`find` must match exactly once on page {identifier} (matches={count})",
            )
        new_content = current_content.replace(
            args.find,
            args.replace if args.raw_html else escape_storage_text(args.replace),
            1,
        )
        body = {
            "version": {"number": current["version"]["number"] + 1},
            "title": current["title"],
            "type": "page",
            "body": {
                "storage": {
                    "value": new_content,
                    "representation": "storage",
                }
            },
        }
        return await perform_write("PUT", f"content/{identifier}", body, fallback_id=identifier)

    if action == "move_page":
        identifier = page_id()
        target = str(args.target_parent_id or "")
        if not _PAGE_ID.fullmatch(target) or target == identifier:
            raise HTTPException(status_code=400, detail="invalid target_parent_id")
        current = await current_page(identifier, "version,ancestors,space")
        target_page = await current_page(target, "version,space")
        ancestors = current.get("ancestors")
        ancestors = ancestors if isinstance(ancestors, list) else []
        current_parent = ancestors[-1] if ancestors and isinstance(ancestors[-1], dict) else {}
        if str(current_parent.get("id") or "") == target:
            raise HTTPException(status_code=409, detail="page is already under target parent")
        current_space = current.get("space") if isinstance(current.get("space"), dict) else {}
        target_space = (
            target_page.get("space") if isinstance(target_page.get("space"), dict) else {}
        )
        if (
            current_space.get("key")
            and target_space.get("key")
            and current_space["key"] != target_space["key"]
        ):
            raise HTTPException(status_code=409, detail="cross-space moves are not supported")
        body = {
            "version": {"number": current["version"]["number"] + 1},
            "title": current["title"],
            "type": "page",
            "ancestors": [{"id": target}],
        }
        return await perform_write("PUT", f"content/{identifier}", body, fallback_id=identifier)

    if action == "comment":
        identifier = page_id()
        text = args.body
        if not text:
            raise HTTPException(status_code=400, detail="`comment` requires page_id and body")
        body = {
            "type": "comment",
            "container": {"id": identifier, "type": "page"},
            "body": {
                "storage": {
                    "value": _storage_text(text),
                    "representation": "storage",
                }
            },
        }
        return await perform_write("POST", "content", body, fallback_id=identifier)

    identifier = page_id()
    return await perform_write("DELETE", f"content/{identifier}", fallback_id=identifier)
