"""Gateway-owned MCP data plane backed by the mature shared MCP client.

The protocol client lives in :mod:`ai_gateway_core.mcp`. This broker is the only outbound boundary for the
Rust Runtime: it resolves tenant credentials, authorizes the exact grant and
owns the reusable, DNS-pinned HTTP client. Writes intentionally remain behind
the durable capability-execution path and are rejected here.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import time
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Protocol

from ai_gateway_core.mcp.client import (
    MCP_PROTOCOL_VERSION,
    MCPClient,
    MCPError,
    MCPServerConfig,
)
from ai_gateway_core.mcp.resilience import MCPInvocationPolicy, MCPOperationKind
from ai_gateway_core.persistence.repositories.mcp_repository import MCPAuthorizationError

MCPClientCacheKey = tuple[str, str, str, str, str]


class MCPGatewayBrokerError(RuntimeError):
    """Stable, redacted broker error."""

    def __init__(self, code: str, message: str = "MCP broker request failed") -> None:
        self.code = code
        super().__init__(message)


class MCPSecretResolver(Protocol):
    async def resolve(self, secret_ref: str) -> str: ...


def _client_config(item: Mapping[str, Any], credential: str | None) -> MCPServerConfig:
    return MCPServerConfig(
        name=str(item.get("name") or item.get("server_id") or "mcp"),
        url=str(item["base_url"]),
        api_key=credential,
        transport="streamable_http",
        timeout=max(0.1, int(item.get("timeout_ms") or 30_000) / 1000),
        host_deadline=max(0.1, int(item.get("timeout_ms") or 30_000) / 1000),
        max_concurrent=max(1, min(32, int(item.get("max_concurrency") or 5))),
        response_limit_bytes=max(
            1024, min(8 * 1024 * 1024, int(item.get("response_limit_bytes") or 1_048_576))
        ),
        endpoint_path=str(item.get("endpoint_path") or "/mcp"),
        auth_method=str(item.get("auth_method") or "none"),
        oauth_resource=item.get("oauth_resource"),
        oauth_audience=item.get("oauth_audience"),
        credential_audience=item.get("audience"),
        allowed_origins=[str(value) for value in item.get("allowed_origins") or []],
    )


@dataclass
class _CacheEntry:
    client: MCPClient
    expires_at: float
    last_used_at: float
    in_use: int = 0
    invalidated: bool = False


class MCPGatewayBroker:
    """Tenant-authorized MCP discovery and read-only invocation broker."""

    def __init__(
        self,
        *,
        repository: Any,
        secret_resolver: MCPSecretResolver,
        ttl_seconds: float = 60.0,
        max_entries: int = 100,
    ) -> None:
        self._repository = repository
        self._secret_resolver = secret_resolver
        self._ttl_seconds = max(1.0, min(float(ttl_seconds), 3600.0))
        self._max_entries = max(1, min(int(max_entries), 1000))
        self._cache: dict[MCPClientCacheKey, _CacheEntry] = {}
        self._inflight: dict[MCPClientCacheKey, asyncio.Task[MCPClient]] = {}
        self._lock = asyncio.Lock()

    @staticmethod
    def _cache_key(item: Mapping[str, Any], tenant_id: str) -> MCPClientCacheKey:
        credential_revision = str(
            item.get("credential_revision")
            or item.get("credential_updated_at")
            or "none"
        )
        config_values = {
            "server_revision": str(item.get("server_updated_at") or item.get("updated_at") or ""),
            "base_url": str(item.get("base_url") or ""),
            "endpoint_path": str(item.get("endpoint_path") or "/mcp"),
            "timeout_ms": int(item.get("timeout_ms") or 30_000),
            "max_concurrency": int(item.get("max_concurrency") or 5),
            "response_limit_bytes": int(item.get("response_limit_bytes") or 1_048_576),
            "auth_method": str(item.get("auth_method") or "none"),
            "oauth_resource": item.get("oauth_resource"),
            "oauth_audience": item.get("oauth_audience"),
            "audience": item.get("audience"),
            "allowed_origins": sorted(str(value) for value in item.get("allowed_origins") or []),
        }
        fingerprint = hashlib.sha256(
            json.dumps(config_values, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        return (
            tenant_id,
            str(item["connection_id"]),
            credential_revision,
            MCP_PROTOCOL_VERSION,
            fingerprint,
        )

    async def _credential(self, item: Mapping[str, Any]) -> str | None:
        if str(item.get("auth_method") or "none") == "none":
            return None
        secret_ref = str(item.get("secret_ref") or "")
        if not secret_ref:
            raise MCPGatewayBrokerError("MCP_SECRET_UNAVAILABLE")
        try:
            value = await self._secret_resolver.resolve(secret_ref)
        except Exception as exc:
            raise MCPGatewayBrokerError("MCP_SECRET_UNAVAILABLE") from exc
        if not value:
            raise MCPGatewayBrokerError("MCP_SECRET_UNAVAILABLE")
        return value

    async def _acquire_client(
        self, item: Mapping[str, Any], *, tenant_id: str
    ) -> tuple[MCPClientCacheKey, MCPClient]:
        key = self._cache_key(item, tenant_id)
        now = time.monotonic()
        close_now: list[MCPClient] = []
        async with self._lock:
            for cached_key, entry in list(self._cache.items()):
                same_connection = cached_key[:2] == key[:2]
                if entry.expires_at <= now or (same_connection and cached_key != key):
                    self._cache.pop(cached_key, None)
                    entry.invalidated = True
                    if entry.in_use == 0:
                        close_now.append(entry.client)
            cached = self._cache.get(key)
            if cached is not None:
                cached.last_used_at = now
                cached.in_use += 1
                task = None
                client = cached.client
            else:
                client = None
                task = self._inflight.get(key)
                if task is None:
                    task = asyncio.create_task(self._create_client(item, key))
                    self._inflight[key] = task
        for stale_client in close_now:
            await stale_client.close()
        if client is not None:
            return key, client
        assert task is not None
        try:
            client = await asyncio.shield(task)
        finally:
            if task.done():
                async with self._lock:
                    if self._inflight.get(key) is task:
                        self._inflight.pop(key, None)
        async with self._lock:
            entry = self._cache.get(key)
            if entry is None or entry.client is not client or entry.invalidated:
                close_client = True
            else:
                entry.in_use += 1
                entry.last_used_at = time.monotonic()
                close_client = False
        if close_client:
            await client.close()
            raise MCPGatewayBrokerError("MCP_CLIENT_INVALIDATED")
        return key, client

    async def _release_client(self, key: MCPClientCacheKey, client: MCPClient) -> None:
        close_client = False
        async with self._lock:
            entry = self._cache.get(key)
            if entry is not None and entry.client is client:
                entry.in_use = max(0, entry.in_use - 1)
                close_client = entry.invalidated and entry.in_use == 0
            else:
                close_client = True
        if close_client:
            await client.close()

    async def _create_client(
        self, item: Mapping[str, Any], key: MCPClientCacheKey
    ) -> MCPClient:
        client = MCPClient(_client_config(item, await self._credential(item)))
        try:
            initialized = await client.initialize()
            if initialized.get("protocolVersion") != MCP_PROTOCOL_VERSION:
                raise MCPGatewayBrokerError("MCP_PROTOCOL_VERSION_MISMATCH")
        except BaseException:
            await client.close()
            raise
        now = time.monotonic()
        evicted: list[MCPClient] = []
        async with self._lock:
            self._cache[key] = _CacheEntry(client, now + self._ttl_seconds, now)
            while len(self._cache) > self._max_entries:
                old_key, old_entry = min(
                    self._cache.items(), key=lambda pair: pair[1].last_used_at
                )
                self._cache.pop(old_key, None)
                old_entry.invalidated = True
                if old_entry.in_use == 0:
                    evicted.append(old_entry.client)
        for old_client in evicted:
            await old_client.close()
        return client

    async def discover(
        self,
        *,
        tenant_id: str,
        user_id: str,
        server_id: str,
        connection_id: str,
        principal_type: str,
        repository: Any | None = None,
    ) -> dict[str, Any]:
        if repository is not None and repository is not self._repository:
            raise MCPGatewayBrokerError("MCP_REPOSITORY_MISMATCH")
        item = await self._repository.resolve_discovery_connection(
            tenant_id=tenant_id,
            user_id=user_id,
            authenticated=True,
            server_id=server_id,
            connection_id=connection_id,
            principal_type=principal_type,
        )
        key, client = await self._acquire_client(item, tenant_id=tenant_id)
        try:
            tools = await client.list_tools()
        except MCPError as exc:
            raise MCPGatewayBrokerError(exc.stable_code) from exc
        finally:
            await self._release_client(key, client)
        normalized = [
            {
                "name": tool.upstream_name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "read_only": False,
                "risk_level": "medium",
            }
            for tool in tools
        ]
        result = await self._repository.record_discovery(
            tenant_id=tenant_id,
            server_id=server_id,
            tools=normalized,
        )
        return {**result, "protocol_version": MCP_PROTOCOL_VERSION}

    async def invoke_read_only(
        self,
        *,
        tenant_id: str,
        user_id: str,
        authenticated: bool,
        channel: str,
        runtime_name: str,
        schema_hash: str,
        risk_level: str,
        connection_id: str,
        principal_type: str,
        arguments: Mapping[str, Any],
    ) -> dict[str, Any]:
        item = await self._repository.authorize_mcp_tool(
            tenant_id=tenant_id,
            user_id=user_id,
            authenticated=authenticated,
            runtime_name=runtime_name,
            schema_hash=schema_hash,
            risk_level=risk_level,
            connection_id=connection_id,
            principal_type=principal_type,
            channel=channel,
        )
        if not bool(item.get("read_only")):
            raise MCPAuthorizationError("MCP_WRITE_REQUIRES_CAPABILITY_EXECUTION")
        key, client = await self._acquire_client(item, tenant_id=tenant_id)
        try:
            result = await client.call_tool(
                str(item.get("upstream_name") or runtime_name),
                dict(arguments),
                invocation_policy=MCPInvocationPolicy(
                    operation_kind=MCPOperationKind.READ,
                    operation_id=f"mcp-read-{time.time_ns()}",
                    max_attempts=1,
                ),
            )
        except MCPError as exc:
            raise MCPGatewayBrokerError(exc.stable_code) from exc
        finally:
            await self._release_client(key, client)
        return {
            "tool_name": runtime_name,
            "content": result.content,
            "is_error": result.is_error,
            "schema_hash": str(item["schema_hash"]),
            "connection_id": str(item["connection_id"]),
        }

    async def invalidate_connection(self, *, tenant_id: str, connection_id: str) -> None:
        async with self._lock:
            entries = [
                self._cache.pop(key)
                for key in list(self._cache)
                if key[0] == tenant_id and key[1] == connection_id
            ]
            tasks = [
                self._inflight.pop(key)
                for key in list(self._inflight)
                if key[0] == tenant_id and key[1] == connection_id
            ]
            for entry in entries:
                entry.invalidated = True
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        for entry in entries:
            if entry.in_use == 0:
                await entry.client.close()

    async def close(self) -> None:
        async with self._lock:
            entries = list(self._cache.values())
            self._cache.clear()
            tasks = list(self._inflight.values())
            self._inflight.clear()
            for entry in entries:
                entry.invalidated = True
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        for entry in entries:
            if entry.in_use == 0:
                await entry.client.close()


__all__ = ["MCPGatewayBroker", "MCPGatewayBrokerError", "MCPSecretResolver"]
