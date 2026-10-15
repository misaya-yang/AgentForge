"""Fail-closed MCP client for tenant-authorized Streamable HTTP.

Tenant-configurable MCP never launches local processes. The client pins DNS,
refuses redirects and unsafe address ranges, validates Origin/OAuth audience,
bounds response bytes, pins the MCP session, and returns stable redacted errors.
Remote catalog text is always treated as untrusted data.
"""

from __future__ import annotations

import asyncio
import ipaddress
import json
import logging
import re
import socket
import time
import urllib.parse
import uuid
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from typing import Any, Final

import httpx

from ai_gateway_core.logging import record_internal_exception

from .resilience import (
    MCPCircuitBreaker,
    MCPCircuitLease,
    MCPCircuitOpen,
    MCPFailureDecision,
    MCPFailureKind,
    MCPInvocationPolicy,
    MCPOperationKind,
    counts_toward_circuit,
    decide_mcp_failure,
)

logger = logging.getLogger(__name__)

MCP_PROTOCOL_VERSION: Final = "2025-11-25"
MAX_TOOLS_PER_SERVER: Final = 50
MAX_DESCRIPTION_LENGTH: Final = 500
MAX_TOOL_NAME_LENGTH: Final = 64
DEFAULT_RESPONSE_LIMIT_BYTES: Final = 1024 * 1024
_SESSION_ID_RE: Final = re.compile(r"^[\x21-\x7e]{1,256}$")
_STATIC_TOOL_NAME_RE: Final = re.compile(r"^[A-Za-z0-9_.:/ -]{1,128}$")
_STATIC_CAPABILITY_KEYS: Final = frozenset(
    {
        "operation_kind",
        "read_only",
        "idempotency_supported",
        "read_back_tool",
        "risk_level",
        "requires_confirmation",
        "display_title",
    }
)


def _merge_generated_headers(
    configured: dict[str, str],
    generated: dict[str, str],
) -> dict[str, str]:
    """Merge headers with case-insensitive precedence for client values."""

    generated_names = {name.casefold() for name in generated}
    merged = {
        name: value for name, value in configured.items() if name.casefold() not in generated_names
    }
    merged.update(generated)
    return merged


class MCPError(Exception):
    """Stable error with no URL, credential, upstream body or stack detail."""

    def __init__(
        self,
        code: int,
        message: str,
        *,
        stable_code: str = "MCP_ERROR",
        failure: MCPFailureDecision | None = None,
        circuit: dict[str, Any] | None = None,
        recovery_evidence: dict[str, Any] | None = None,
    ):
        self.code = code
        self.stable_code = stable_code
        self.failure = failure
        self.circuit = dict(circuit or {})
        self.recovery_evidence = dict(recovery_evidence or {})
        super().__init__(message)

    def metadata(self) -> dict[str, Any]:
        return {
            "mcp_error_code": self.stable_code,
            "mcp_failure": self.failure.to_dict() if self.failure is not None else None,
            "mcp_circuit": self.circuit,
            "mcp_recovery_evidence": self.recovery_evidence,
        }


DNSResolver = Callable[[str, int], Iterable[str]]


@dataclass(frozen=True)
class MCPStaticToolCapability:
    """Operator-owned operation facts for one statically configured tool."""

    operation_kind: MCPOperationKind = MCPOperationKind.UNKNOWN
    read_only: bool = False
    idempotency_supported: bool = False
    read_back_tool: str | None = None
    risk_level: str | None = None
    requires_confirmation: bool | None = None
    display_title: str | None = None

    @classmethod
    def from_config(cls, tool_name: str, value: Any) -> MCPStaticToolCapability:
        if isinstance(value, cls):
            return value
        if not _STATIC_TOOL_NAME_RE.fullmatch(str(tool_name or "")):
            raise ValueError("MCP_STATIC_CAPABILITY_TOOL_INVALID")
        if not isinstance(value, dict) or set(value) - _STATIC_CAPABILITY_KEYS:
            raise ValueError("MCP_STATIC_CAPABILITY_INVALID")

        raw_kind = value.get("operation_kind")
        try:
            operation_kind = (
                MCPOperationKind(str(raw_kind).strip().lower())
                if raw_kind is not None
                else MCPOperationKind.UNKNOWN
            )
        except ValueError as exc:
            raise ValueError("MCP_STATIC_CAPABILITY_OPERATION_INVALID") from exc

        for key in ("read_only", "idempotency_supported", "requires_confirmation"):
            if key in value and not isinstance(value[key], bool):
                raise ValueError("MCP_STATIC_CAPABILITY_BOOLEAN_INVALID")
        read_only_declared = value.get("read_only") is True
        read_only_explicit_false = "read_only" in value and value.get("read_only") is False
        if (
            read_only_declared
            and operation_kind
            in {
                MCPOperationKind.WRITE,
                MCPOperationKind.UNKNOWN,
            }
            and raw_kind is not None
        ):
            raise ValueError("MCP_STATIC_CAPABILITY_CONFLICT")
        if operation_kind is MCPOperationKind.READ and read_only_explicit_false:
            raise ValueError("MCP_STATIC_CAPABILITY_CONFLICT")
        if read_only_declared:
            operation_kind = MCPOperationKind.READ
        read_only = operation_kind is MCPOperationKind.READ

        raw_read_back = value.get("read_back_tool")
        if raw_read_back is not None and (
            not isinstance(raw_read_back, str)
            or not _STATIC_TOOL_NAME_RE.fullmatch(raw_read_back)
            or raw_read_back == tool_name
        ):
            raise ValueError("MCP_STATIC_CAPABILITY_READ_BACK_INVALID")
        raw_risk_level = value.get("risk_level")
        if raw_risk_level is not None and raw_risk_level not in {"low", "medium", "high"}:
            raise ValueError("MCP_STATIC_CAPABILITY_RISK_INVALID")
        if raw_risk_level == "high" and value.get("requires_confirmation") is False:
            raise ValueError("MCP_STATIC_CAPABILITY_CONFLICT")
        raw_display_title = value.get("display_title")
        if raw_display_title is not None and (
            not isinstance(raw_display_title, str)
            or not 1 <= len(raw_display_title.strip()) <= 80
            or any(ord(char) < 32 for char in raw_display_title)
        ):
            raise ValueError("MCP_STATIC_CAPABILITY_DISPLAY_TITLE_INVALID")
        return cls(
            operation_kind=operation_kind,
            read_only=read_only,
            idempotency_supported=bool(value.get("idempotency_supported", False)),
            read_back_tool=raw_read_back or None,
            risk_level=raw_risk_level,
            requires_confirmation=value.get("requires_confirmation"),
            display_title=raw_display_title.strip() if raw_display_title else None,
        )


@dataclass
class MCPServerConfig:
    """Resolved runtime configuration for one remote MCP connection."""

    name: str
    url: str = ""
    # Holds the resolved plaintext credential (Bearer secret / OAuth access
    # token). Keep it out of repr so stringifying the config can never leak
    # the secret into logs, tracebacks, or error reports (AS-MCP-002),
    # mirroring dns_resolver/code_verifier below.
    api_key: str | None = field(default=None, repr=False)
    headers: dict[str, str] = field(default_factory=dict, repr=False)
    transport: str = "streamable_http"
    timeout: float = 30.0
    host_deadline: float | None = None
    enabled: bool = True
    description: str = ""
    allowed_tools: list[str] | None = None
    blocked_tools: list[str] | None = None
    max_concurrent: int = 10
    circuit_failure_threshold: int = 3
    circuit_cooldown_seconds: float = 30.0
    response_limit_bytes: int = DEFAULT_RESPONSE_LIMIT_BYTES
    endpoint_path: str = "/mcp"
    auth_method: str = "none"
    oauth_resource: str | None = None
    oauth_audience: str | None = None
    credential_audience: str | None = None
    origin: str | None = None
    allowed_origins: list[str] = field(default_factory=list)
    allow_localhost: bool = False
    allow_private_network: bool = False
    platform_managed: bool = False
    default_tenant_enabled: bool = False
    command: str = ""
    args: tuple[str, ...] = ()
    process_env: dict[str, str] = field(default_factory=dict, repr=False)
    inherited_env_names: tuple[str, ...] = ()
    cwd: str = ""
    plugin_root: str = ""
    plugin_data_dir: str = ""
    tool_capabilities: dict[str, MCPStaticToolCapability | dict[str, Any]] = field(
        default_factory=dict
    )
    dns_resolver: DNSResolver | None = field(default=None, repr=False, compare=False)

    def __post_init__(self) -> None:
        if self.transport not in {"streamable_http", "http"}:
            raise ValueError("Unsupported MCP transport")
        # `http` was the legacy spelling. It remains readable for the static,
        # platform-managed compatibility config but is normalized immediately.
        if self.transport == "http":
            self.transport = "streamable_http"
        if not self.endpoint_path.startswith("/") or ".." in self.endpoint_path:
            raise ValueError("Invalid MCP endpoint path")
        if self.max_concurrent < 1 or self.max_concurrent > 32:
            raise ValueError("Invalid MCP concurrency limit")
        if self.timeout <= 0:
            raise ValueError("Invalid MCP transport timeout")
        if self.host_deadline is None:
            self.host_deadline = self.timeout + min(5.0, max(0.1, self.timeout * 0.1))
        if self.host_deadline <= 0:
            raise ValueError("Invalid MCP host deadline")
        if self.circuit_failure_threshold < 1 or self.circuit_failure_threshold > 20:
            raise ValueError("Invalid MCP circuit threshold")
        if self.circuit_cooldown_seconds < 0 or self.circuit_cooldown_seconds > 3600:
            raise ValueError("Invalid MCP circuit cooldown")
        if self.response_limit_bytes < 1024 or self.response_limit_bytes > 8 * 1024 * 1024:
            raise ValueError("Invalid MCP response limit")
        if not isinstance(self.headers, dict) or any(
            not isinstance(name, str) or not isinstance(value, str)
            for name, value in self.headers.items()
        ):
            raise ValueError("Invalid MCP fixed headers")
        if not isinstance(self.tool_capabilities, dict):
            raise ValueError("MCP_STATIC_CAPABILITIES_INVALID")
        if self.tool_capabilities and not self.platform_managed:
            raise ValueError("MCP_STATIC_CAPABILITY_SOURCE_UNTRUSTED")
        self.tool_capabilities = {
            str(tool_name): MCPStaticToolCapability.from_config(str(tool_name), capability)
            for tool_name, capability in self.tool_capabilities.items()
        }

    def static_tool_capability(self, tool_name: str) -> MCPStaticToolCapability:
        capability = self.tool_capabilities.get(tool_name)
        return (
            capability
            if isinstance(capability, MCPStaticToolCapability)
            else MCPStaticToolCapability()
        )


@dataclass
class MCPTool:
    name: str
    description: str
    input_schema: dict[str, Any]
    server_name: str
    annotations: dict[str, Any] = field(default_factory=dict)
    upstream_name: str = ""
    full_name: str = ""

    def __post_init__(self) -> None:
        if not self.upstream_name:
            self.upstream_name = self.name
        if not self.full_name:
            self.full_name = f"{self.server_name}:{self.name}"


@dataclass
class MCPToolResult:
    content: list[dict[str, Any]] = field(default_factory=list)
    is_error: bool = False
    failure: MCPFailureDecision | None = None
    circuit: dict[str, Any] = field(default_factory=dict)


class MCPClient:
    """MCP Streamable HTTP client for a single already-authorized principal."""

    def __init__(
        self,
        config: MCPServerConfig,
        *,
        http_client: httpx.AsyncClient | None = None,
        circuit_breaker: MCPCircuitBreaker | None = None,
    ) -> None:
        self.config = config
        self._http = http_client
        self._owns_http = http_client is None
        self._tools: list[MCPTool] = []
        self._initialized = False
        self._server_info: dict[str, Any] = {}
        self._semaphore = asyncio.Semaphore(config.max_concurrent)
        self._pinned_addresses: frozenset[str] = frozenset()
        self._session_id: str | None = None
        self._circuit = circuit_breaker or MCPCircuitBreaker(
            failure_threshold=config.circuit_failure_threshold,
            cooldown_seconds=config.circuit_cooldown_seconds,
        )
        self._circuits: dict[str, MCPCircuitBreaker] = {"default": self._circuit}

    def _circuit_for(self, scope: str) -> MCPCircuitBreaker:
        key = str(scope or "default")[:256]
        breaker = self._circuits.get(key)
        if breaker is None:
            breaker = MCPCircuitBreaker(
                failure_threshold=self.config.circuit_failure_threshold,
                cooldown_seconds=self.config.circuit_cooldown_seconds,
            )
            self._circuits[key] = breaker
        if len(self._circuits) > 500:
            oldest = min(
                (item for item in self._circuits if item != key),
                key=lambda item: self._circuits[item].touched_at,
                default=None,
            )
            if oldest is not None:
                self._circuits.pop(oldest, None)
        return breaker

    @staticmethod
    def _default_resolver(hostname: str, port: int) -> Iterable[str]:
        return {
            str(item[4][0])
            for item in socket.getaddrinfo(
                hostname,
                port,
                type=socket.SOCK_STREAM,
            )
        }

    @classmethod
    def _resolve_addresses(
        cls,
        hostname: str,
        port: int,
        resolver: DNSResolver | None,
    ) -> frozenset[str]:
        try:
            raw = (resolver or cls._default_resolver)(hostname, port)
            addresses = frozenset(str(ipaddress.ip_address(value)) for value in raw)
        except (OSError, ValueError, TypeError) as exc:
            raise MCPError(
                -10,
                "MCP destination cannot be resolved",
                stable_code="MCP_DNS_UNAVAILABLE",
            ) from exc
        if not addresses:
            raise MCPError(
                -10,
                "MCP destination cannot be resolved",
                stable_code="MCP_DNS_UNAVAILABLE",
            )
        return addresses

    @classmethod
    def _validate_url(
        cls,
        url: str,
        *,
        allow_localhost: bool = False,
        allow_private_network: bool = False,
        platform_managed: bool = False,
        resolver: DNSResolver | None = None,
    ) -> frozenset[str]:
        try:
            parsed = urllib.parse.urlsplit(url)
            port = parsed.port or (443 if parsed.scheme.lower() == "https" else 80)
        except ValueError as exc:
            raise MCPError(
                -11,
                "MCP destination is invalid",
                stable_code="MCP_URL_INVALID",
            ) from exc
        hostname = parsed.hostname or ""
        if (
            parsed.scheme.lower() not in {"http", "https"}
            or not hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.fragment
        ):
            raise MCPError(
                -11,
                "MCP destination is invalid",
                stable_code="MCP_URL_INVALID",
            )
        is_local_name = hostname.lower() == "localhost"
        try:
            is_loopback_literal = ipaddress.ip_address(hostname).is_loopback
        except ValueError:
            is_loopback_literal = False
        internal_name = hostname.lower().endswith((".internal", ".local"))
        local_exception = platform_managed and (
            (allow_localhost and (is_local_name or is_loopback_literal))
            or (allow_private_network and internal_name)
        )
        if parsed.scheme.lower() != "https" and not local_exception:
            raise MCPError(
                -11,
                "MCP destination must use TLS",
                stable_code="MCP_TLS_REQUIRED",
            )
        addresses = cls._resolve_addresses(hostname, port, resolver)
        for raw in addresses:
            ip = ipaddress.ip_address(raw)
            if ip.is_link_local or ip.is_multicast or ip.is_reserved or ip.is_unspecified:
                raise MCPError(
                    -12,
                    "MCP destination is not permitted",
                    stable_code="MCP_SSRF_BLOCKED",
                )
            if ip.is_loopback and platform_managed and allow_localhost:
                continue
            if ip.is_private and platform_managed and allow_private_network:
                continue
            if not ip.is_global:
                raise MCPError(
                    -12,
                    "MCP destination is not permitted",
                    stable_code="MCP_SSRF_BLOCKED",
                )
        return addresses

    @staticmethod
    def _pinned_request_target(
        url: str,
        addresses: frozenset[str],
        *,
        relative_path: str | None = None,
    ) -> tuple[str, str, str]:
        """Return an IP-literal URL plus the original HTTP Host and TLS SNI.

        DNS validation alone is not a pin: a normal hostname request lets the
        transport resolve the name again. Requests therefore connect to one of
        the already-validated IP literals while preserving the original
        authority for certificate validation and virtual-host routing.
        """

        parsed = urllib.parse.urlsplit(url)
        hostname = parsed.hostname or ""
        original_host = hostname.encode("idna").decode("ascii")
        selected = min(
            (ipaddress.ip_address(value) for value in addresses),
            key=lambda value: (value.version, int(value)),
        )
        rendered_ip = f"[{selected}]" if selected.version == 6 else str(selected)
        explicit_port = parsed.port
        pinned_authority = (
            f"{rendered_ip}:{explicit_port}" if explicit_port is not None else rendered_ip
        )
        rendered_host = f"[{original_host}]" if ":" in original_host else original_host
        host_header = (
            f"{rendered_host}:{explicit_port}" if explicit_port is not None else rendered_host
        )

        path = parsed.path or "/"
        query = parsed.query
        if relative_path is not None:
            relative = urllib.parse.urlsplit(relative_path)
            base_path = parsed.path.rstrip("/")
            path = f"{base_path}/{relative.path.lstrip('/')}"
            query = relative.query
        target = urllib.parse.urlunsplit((parsed.scheme.lower(), pinned_authority, path, query, ""))
        return target, host_header, original_host

    def _validate_origin_and_audience(self) -> None:
        if self.config.origin:
            parsed = urllib.parse.urlsplit(self.config.origin)
            canonical = (
                f"{parsed.scheme.lower()}://{parsed.netloc}"
                if parsed.scheme and parsed.netloc and parsed.path in {"", "/"}
                else ""
            )
            if canonical != self.config.origin.rstrip("/") or canonical not in {
                origin.rstrip("/") for origin in self.config.allowed_origins
            }:
                raise MCPError(
                    -13,
                    "MCP Origin is not permitted",
                    stable_code="MCP_ORIGIN_DENIED",
                )
        if self.config.auth_method == "oauth" and (
            not self.config.oauth_resource
            or not self.config.oauth_audience
            or self.config.credential_audience != self.config.oauth_audience
        ):
            raise MCPError(
                -14,
                "MCP OAuth audience is invalid",
                stable_code="MCP_OAUTH_AUDIENCE_MISMATCH",
            )

    def _validate_dns_pin(self) -> frozenset[str]:
        current = self._validate_url(
            self.config.url,
            allow_localhost=self.config.allow_localhost,
            allow_private_network=self.config.allow_private_network,
            platform_managed=self.config.platform_managed,
            resolver=self.config.dns_resolver,
        )
        if self._pinned_addresses and current != self._pinned_addresses:
            raise MCPError(
                -15,
                "MCP destination changed during the session",
                stable_code="MCP_DNS_REBINDING_BLOCKED",
            )
        self._pinned_addresses = current
        return current

    def _current_dns_pin(self) -> frozenset[str]:
        """Return the initialize-time pin without repeating synchronous DNS.

        Requests connect to an IP literal from this immutable set, so DNS
        rebinding cannot redirect an established client. A fresh runtime cache
        generation resolves and validates DNS again after TTL or revision
        invalidation.
        """
        if not self._pinned_addresses:
            raise MCPError(
                -1,
                "MCP client is not initialized",
                stable_code="MCP_NOT_CONNECTED",
            )
        return self._pinned_addresses

    def _build_pinned_request(
        self,
        method: str,
        *,
        headers: dict[str, str],
        payload: dict[str, Any],
    ) -> httpx.Request:
        if self._http is None:
            raise MCPError(
                -1,
                "MCP client is not connected",
                stable_code="MCP_NOT_CONNECTED",
            )
        addresses = self._current_dns_pin()
        target, host_header, sni_hostname = self._pinned_request_target(
            self.config.url,
            addresses,
            relative_path=self.config.endpoint_path,
        )
        request = self._http.build_request(
            method,
            target,
            json=payload,
            headers=headers,
        )
        request.headers["Host"] = host_header
        request.extensions["sni_hostname"] = sni_hostname
        return request

    async def initialize(self) -> dict[str, Any]:
        if self._initialized:
            return {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "serverInfo": dict(self._server_info),
            }
        self._validate_origin_and_audience()
        await asyncio.to_thread(self._validate_dns_pin)
        # Client-generated protocol and authorization headers take precedence
        # over visible package configuration as required by Agent Plugins v1.
        generated_headers = {
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
        }
        if self.config.origin:
            generated_headers["Origin"] = self.config.origin.rstrip("/")
        if self.config.api_key:
            generated_headers["Authorization"] = f"Bearer {self.config.api_key}"
        headers = _merge_generated_headers(self.config.headers, generated_headers)
        if self._http is None:
            self._http = httpx.AsyncClient(
                base_url=self.config.url,
                timeout=httpx.Timeout(self.config.timeout),
                headers=headers,
                follow_redirects=False,
                trust_env=False,
            )
        else:
            self._http.headers.update(headers)

        result = await self._jsonrpc(
            "initialize",
            {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": True}},
                "clientInfo": {"name": "ai-gateway", "version": "1.0.0"},
            },
        )
        if result.get("protocolVersion") != MCP_PROTOCOL_VERSION:
            raise MCPError(
                -16,
                "MCP protocol version is unsupported",
                stable_code="MCP_PROTOCOL_VERSION_MISMATCH",
            )
        self._server_info = dict(result.get("serverInfo") or {})
        await self._notify("notifications/initialized")
        self._initialized = True
        logger.info("MCP server initialized: %s", self.config.name)
        return result

    async def list_tools(self) -> list[MCPTool]:
        response = await self._jsonrpc("tools/list", {})
        raw_tools = response.get("tools")
        if not isinstance(raw_tools, list):
            raise MCPError(
                -17,
                "MCP tool catalog is invalid",
                stable_code="MCP_CATALOG_INVALID",
            )
        tools: list[MCPTool] = []
        for raw in raw_tools[:MAX_TOOLS_PER_SERVER]:
            if not isinstance(raw, dict):
                continue
            upstream_name = raw.get("name")
            input_schema = raw.get("inputSchema") or {}
            if not isinstance(upstream_name, str) or not isinstance(input_schema, dict):
                continue
            if not self._is_tool_allowed(upstream_name):
                continue
            tools.append(
                MCPTool(
                    name=self._sanitize_name(upstream_name),
                    description=self._sanitize_description(str(raw.get("description") or "")),
                    input_schema=input_schema,
                    server_name=self.config.name,
                    upstream_name=upstream_name,
                    annotations=(
                        dict(raw.get("annotations") or {})
                        if isinstance(raw.get("annotations"), dict)
                        else {}
                    ),
                )
            )
        self._tools = tools
        return list(tools)

    async def call_tool(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        *,
        invocation_policy: MCPInvocationPolicy | None = None,
    ) -> MCPToolResult:
        policy = invocation_policy or MCPInvocationPolicy(
            operation_kind=MCPOperationKind.UNKNOWN,
            operation_id=f"mcp_op_{uuid.uuid4().hex[:24]}",
        )
        started = time.monotonic()
        circuit_breaker = self._circuit_for(policy.circuit_scope)
        lease: MCPCircuitLease | None = None
        operation_started = False
        circuit_recorded = False
        response: dict[str, Any] | None = None
        content: list[Any] = []
        circuit: dict[str, Any] = {}
        failure: MCPFailureDecision | None = None
        try:
            # The Assistant package requires Python 3.11+, while the monorepo
            # root mypy target remains 3.10 for older services.
            host_timeout = asyncio.timeout(  # type: ignore[attr-defined]
                float(self.config.host_deadline or self.config.timeout)
            )
            async with host_timeout, self._semaphore:
                try:
                    lease = await circuit_breaker.acquire()
                except MCPCircuitOpen as exc:
                    failure = decide_mcp_failure("MCP_CIRCUIT_OPEN", policy)
                    circuit = await circuit_breaker.snapshot()
                    circuit["retry_after_seconds"] = exc.retry_after
                    raise MCPError(
                        -25,
                        "MCP circuit is open",
                        stable_code="MCP_CIRCUIT_OPEN",
                        failure=failure,
                        circuit=circuit,
                    ) from exc
                try:
                    params: dict[str, Any] = {
                        "name": tool_name,
                        "arguments": arguments,
                    }
                    request_meta = policy.request_meta()
                    if request_meta is not None:
                        params["_meta"] = request_meta

                    last_error: MCPError | None = None
                    operation_started = True
                    for attempt in range(policy.max_attempts):
                        try:
                            response = await self._jsonrpc("tools/call", params)
                            last_error = None
                            break
                        except MCPError as exc:
                            failure = exc.failure or decide_mcp_failure(
                                exc.stable_code,
                                policy,
                            )
                            exc.failure = failure
                            last_error = exc
                            if not (
                                failure.auto_retry_allowed and attempt + 1 < policy.max_attempts
                            ):
                                raise
                    if last_error is not None or response is None:
                        raise last_error or MCPError(
                            -1,
                            "MCP operation failed",
                            stable_code="MCP_ERROR",
                        )

                    raw_content = response.get("content") or []
                    if not isinstance(raw_content, list):
                        raise MCPError(
                            -18,
                            "MCP tool response is invalid",
                            stable_code="MCP_RESPONSE_INVALID",
                        )
                    content = raw_content
                except MCPError as exc:
                    failure = exc.failure or decide_mcp_failure(
                        exc.stable_code,
                        policy,
                        operation_started=operation_started,
                    )
                    exc.failure = failure
                    if lease is not None:
                        if failure.cause is MCPFailureKind.CANCELLED:
                            await circuit_breaker.record_neutral(lease)
                        elif counts_toward_circuit(failure):
                            await circuit_breaker.record_failure(lease)
                        else:
                            await circuit_breaker.record_success(lease)
                        circuit_recorded = True
                    exc.circuit = await circuit_breaker.snapshot()
                    raise
                except asyncio.CancelledError:
                    if host_timeout.expired():
                        # Preserve the host-timeout stable code; the timeout
                        # context converts its own cancellation outside.
                        raise
                    error_code = (
                        "MCP_CANCELLED_AFTER_DISPATCH" if operation_started else "MCP_CANCELLED"
                    )
                    cancellation_failure = decide_mcp_failure(
                        error_code,
                        policy,
                        operation_started=operation_started,
                    )
                    if lease is not None:
                        await circuit_breaker.record_neutral(lease)
                        circuit_recorded = True
                    raise MCPError(
                        499,
                        "MCP invocation cancelled",
                        stable_code=error_code,
                        failure=cancellation_failure,
                        circuit=await circuit_breaker.snapshot(),
                    ) from None

                is_error = bool(response.get("isError", False))
                failure = decide_mcp_failure("MCP_REMOTE_TOOL_ERROR", policy) if is_error else None
                await circuit_breaker.record_success(lease)
                circuit_recorded = True
                circuit = await circuit_breaker.snapshot()
        except asyncio.CancelledError:
            error_code = "MCP_CANCELLED_AFTER_DISPATCH" if operation_started else "MCP_CANCELLED"
            failure = decide_mcp_failure(
                error_code,
                policy,
                operation_started=operation_started,
            )
            if lease is not None and not circuit_recorded:
                await circuit_breaker.record_neutral(lease)
                circuit_recorded = True
            raise MCPError(
                499,
                "MCP invocation cancelled",
                stable_code=error_code,
                failure=failure,
                circuit=await circuit_breaker.snapshot(),
            ) from None
        except TimeoutError as exc:
            failure = decide_mcp_failure(
                "MCP_HOST_DEADLINE",
                policy,
                operation_started=operation_started,
            )
            if lease is not None and not circuit_recorded:
                if counts_toward_circuit(failure):
                    await circuit_breaker.record_failure(lease)
                else:
                    await circuit_breaker.record_success(lease)
            circuit = await circuit_breaker.snapshot()
            raise MCPError(
                -2,
                "MCP host deadline exceeded",
                stable_code="MCP_HOST_DEADLINE",
                failure=failure,
                circuit=circuit,
            ) from exc
        except MCPError as exc:
            failure = exc.failure or decide_mcp_failure(exc.stable_code, policy)
            exc.failure = failure
            if lease is not None and not circuit_recorded:
                if counts_toward_circuit(failure):
                    await circuit_breaker.record_failure(lease)
                else:
                    await circuit_breaker.record_success(lease)
                exc.circuit = await circuit_breaker.snapshot()
            record_internal_exception(
                logger,
                "mcp.client.tool_failed",
                exc,
                level=logging.WARNING,
            )
            raise

        logger.info(
            "MCP tool completed: server=%s tool=%s duration_ms=%.0f",
            self.config.name,
            self._sanitize_name(tool_name),
            (time.monotonic() - started) * 1000,
        )
        return MCPToolResult(
            content=[item for item in content if isinstance(item, dict)],
            is_error=is_error,
            failure=failure,
            circuit=circuit,
        )

    async def download_resource_link(self, resource_url: str) -> tuple[bytes, str]:
        """Download one same-origin HTTP resource through the pinned MCP connection."""

        if self._http is None:
            raise MCPError(
                -1,
                "MCP client is not connected",
                stable_code="MCP_NOT_CONNECTED",
            )
        if not self._pinned_addresses:
            await asyncio.to_thread(self._validate_dns_pin)
        try:
            base = urllib.parse.urlsplit(self.config.url)
            resource = urllib.parse.urlsplit(resource_url)
            base_port = base.port or (443 if base.scheme.lower() == "https" else 80)
            resource_port = resource.port or (443 if resource.scheme.lower() == "https" else 80)
        except ValueError as exc:
            raise MCPError(
                -26,
                "MCP resource URL is invalid",
                stable_code="MCP_RESOURCE_URL_INVALID",
            ) from exc
        resource_host = (resource.hostname or "").lower()
        try:
            resource_is_loopback = ipaddress.ip_address(resource_host).is_loopback
        except ValueError:
            resource_is_loopback = resource_host == "localhost"
        loopback_alias = bool(
            self.config.platform_managed
            and self.config.allow_private_network
            and resource_is_loopback
            and resource.scheme.lower() == base.scheme.lower()
            and resource_port == base_port
            and resource.username is None
            and resource.password is None
            and not resource.fragment
        )
        if loopback_alias:
            resource_url = urllib.parse.urlunsplit(
                (
                    base.scheme,
                    base.netloc,
                    resource.path,
                    resource.query,
                    "",
                )
            )
            resource = urllib.parse.urlsplit(resource_url)
        if (
            resource.scheme.lower() != base.scheme.lower()
            or (resource.hostname or "").lower() != (base.hostname or "").lower()
            or resource_port != base_port
            or resource.username is not None
            or resource.password is not None
            or bool(resource.fragment)
        ):
            raise MCPError(
                -27,
                "MCP resource origin is not permitted",
                stable_code="MCP_RESOURCE_ORIGIN_MISMATCH",
            )

        addresses = self._current_dns_pin()
        target, host_header, sni_hostname = self._pinned_request_target(
            resource_url,
            addresses,
        )
        request = self._http.build_request("GET", target, headers={"Accept": "*/*"})
        request.headers["Host"] = host_header
        request.extensions["sni_hostname"] = sni_hostname
        response: httpx.Response | None = None
        try:
            response = await self._http.send(
                request,
                stream=True,
                follow_redirects=False,
            )
            if 300 <= response.status_code < 400:
                raise MCPError(
                    -22,
                    "MCP redirects are not permitted",
                    stable_code="MCP_REDIRECT_BLOCKED",
                )
            if response.status_code >= 500:
                raise MCPError(
                    response.status_code,
                    "MCP resource was unavailable",
                    stable_code="MCP_UPSTREAM_UNAVAILABLE",
                )
            if response.status_code < 200 or response.status_code >= 300:
                raise MCPError(
                    response.status_code,
                    "MCP resource request was rejected",
                    stable_code="MCP_UPSTREAM_REJECTED",
                )
            body = await self._read_response_body(response)
            content_type = (
                response.headers.get(
                    "Content-Type",
                    "application/octet-stream",
                )
                .split(";", 1)[0]
                .strip()
                .lower()
            )
            return body, content_type or "application/octet-stream"
        except httpx.TimeoutException as exc:
            raise MCPError(
                -2,
                "MCP resource request timed out",
                stable_code="MCP_TIMEOUT",
            ) from exc
        except httpx.RequestError as exc:
            raise MCPError(
                -3,
                "MCP resource is unavailable",
                stable_code="MCP_UPSTREAM_UNAVAILABLE",
            ) from exc
        finally:
            if response is not None:
                await response.aclose()

    async def close(self) -> None:
        if self._http is not None and self._owns_http:
            await self._http.aclose()
        self._http = None
        self._initialized = False
        self._session_id = None

    @property
    def tools(self) -> list[MCPTool]:
        return list(self._tools)

    @property
    def is_initialized(self) -> bool:
        return self._initialized

    async def _read_response_body(self, response: httpx.Response) -> bytes:
        content_length = response.headers.get("Content-Length")
        if content_length:
            try:
                if int(content_length) > self.config.response_limit_bytes:
                    raise MCPError(
                        -19,
                        "MCP response exceeded the configured limit",
                        stable_code="MCP_RESPONSE_TOO_LARGE",
                    )
            except ValueError as exc:
                raise MCPError(
                    -18,
                    "MCP response is invalid",
                    stable_code="MCP_RESPONSE_INVALID",
                ) from exc
        body = bytearray()
        async for chunk in response.aiter_bytes():
            if len(body) + len(chunk) > self.config.response_limit_bytes:
                raise MCPError(
                    -19,
                    "MCP response exceeded the configured limit",
                    stable_code="MCP_RESPONSE_TOO_LARGE",
                )
            body.extend(chunk)
        return bytes(body)

    def _response_payload(
        self,
        response: httpx.Response,
        body: bytes,
    ) -> dict[str, Any]:
        content_type = response.headers.get("Content-Type", "").split(";", 1)[0].lower()
        try:
            if content_type == "application/json":
                payload = json.loads(body)
            elif content_type == "text/event-stream":
                data_lines = [
                    line[5:].strip()
                    for line in body.decode("utf-8").splitlines()
                    if line.startswith("data:")
                ]
                if not data_lines:
                    raise ValueError("missing SSE data")
                payload = json.loads(data_lines[-1])
            else:
                raise MCPError(
                    -20,
                    "MCP response content type is unsupported",
                    stable_code="MCP_CONTENT_TYPE_INVALID",
                )
        except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as exc:
            raise MCPError(
                -18,
                "MCP response is invalid",
                stable_code="MCP_RESPONSE_INVALID",
            ) from exc
        if not isinstance(payload, dict) or payload.get("jsonrpc") != "2.0":
            raise MCPError(
                -18,
                "MCP response is invalid",
                stable_code="MCP_RESPONSE_INVALID",
            )
        return payload

    def _pin_session(self, response: httpx.Response) -> None:
        received = response.headers.get("Mcp-Session-Id")
        if received is None:
            return
        if not _SESSION_ID_RE.fullmatch(received):
            raise MCPError(
                -21,
                "MCP session identifier is invalid",
                stable_code="MCP_SESSION_INVALID",
            )
        if self._session_id is not None and received != self._session_id:
            raise MCPError(
                -21,
                "MCP session identifier changed",
                stable_code="MCP_SESSION_CONFUSION_BLOCKED",
            )
        self._session_id = received

    async def _jsonrpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if self._http is None:
            raise MCPError(
                -1,
                "MCP client is not connected",
                stable_code="MCP_NOT_CONNECTED",
            )
        request_id = str(uuid.uuid4())
        payload = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }
        headers = {"MCP-Protocol-Version": MCP_PROTOCOL_VERSION}
        if self._session_id:
            headers["Mcp-Session-Id"] = self._session_id
        request = self._build_pinned_request(
            "POST",
            headers=headers,
            payload=payload,
        )
        response: httpx.Response | None = None
        try:
            response = await self._http.send(
                request,
                stream=True,
                follow_redirects=False,
            )
            if 300 <= response.status_code < 400:
                raise MCPError(
                    -22,
                    "MCP redirects are not permitted",
                    stable_code="MCP_REDIRECT_BLOCKED",
                )
            if response.status_code >= 500:
                raise MCPError(
                    response.status_code,
                    "MCP upstream response was unavailable",
                    stable_code="MCP_UPSTREAM_UNAVAILABLE",
                )
            if response.status_code < 200 or response.status_code >= 300:
                raise MCPError(
                    response.status_code,
                    "MCP server rejected the request",
                    stable_code="MCP_UPSTREAM_REJECTED",
                )
            self._pin_session(response)
            body = await self._read_response_body(response)
            result = self._response_payload(response, body)
        except httpx.TimeoutException as exc:
            raise MCPError(
                -2,
                "MCP request timed out",
                stable_code="MCP_TIMEOUT",
            ) from exc
        except httpx.RequestError as exc:
            raise MCPError(
                -3,
                "MCP server is unavailable",
                stable_code="MCP_UPSTREAM_UNAVAILABLE",
            ) from exc
        finally:
            if response is not None:
                await response.aclose()
        if str(result.get("id")) != request_id:
            raise MCPError(
                -23,
                "MCP response identity does not match the request",
                stable_code="MCP_RESPONSE_ID_MISMATCH",
            )
        if "error" in result:
            raise MCPError(
                -24,
                "MCP operation failed",
                stable_code="MCP_REMOTE_ERROR",
            )
        value = result.get("result") or {}
        if not isinstance(value, dict):
            raise MCPError(
                -18,
                "MCP response is invalid",
                stable_code="MCP_RESPONSE_INVALID",
            )
        return value

    async def _notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        if self._http is None:
            return
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params:
            payload["params"] = params
        headers = {"MCP-Protocol-Version": MCP_PROTOCOL_VERSION}
        if self._session_id:
            headers["Mcp-Session-Id"] = self._session_id
        request = self._build_pinned_request(
            "POST",
            headers=headers,
            payload=payload,
        )
        response: httpx.Response | None = None
        try:
            response = await self._http.send(
                request,
                stream=True,
                follow_redirects=False,
            )
            if 300 <= response.status_code < 400:
                raise MCPError(
                    -22,
                    "MCP redirects are not permitted",
                    stable_code="MCP_REDIRECT_BLOCKED",
                )
            if response.status_code >= 500:
                raise MCPError(
                    response.status_code,
                    "MCP upstream response was unavailable",
                    stable_code="MCP_UPSTREAM_UNAVAILABLE",
                )
            if response.status_code not in {200, 202, 204}:
                raise MCPError(
                    response.status_code,
                    "MCP server rejected the notification",
                    stable_code="MCP_UPSTREAM_REJECTED",
                )
            self._pin_session(response)
            await self._read_response_body(response)
        except httpx.TimeoutException as exc:
            raise MCPError(
                -2,
                "MCP request timed out",
                stable_code="MCP_TIMEOUT",
            ) from exc
        except httpx.RequestError as exc:
            raise MCPError(
                -3,
                "MCP server is unavailable",
                stable_code="MCP_UPSTREAM_UNAVAILABLE",
            ) from exc
        finally:
            if response is not None:
                await response.aclose()

    def _is_tool_allowed(self, tool_name: str) -> bool:
        if not tool_name:
            return False
        if self.config.blocked_tools and tool_name in self.config.blocked_tools:
            return False
        if self.config.allowed_tools is not None:
            return tool_name in self.config.allowed_tools
        return True

    @staticmethod
    def _sanitize_name(name: str) -> str:
        clean = re.sub(r"[^a-zA-Z0-9_\-]", "_", name)[:MAX_TOOL_NAME_LENGTH]
        return clean or "unnamed"

    @staticmethod
    def _sanitize_description(description: str) -> str:
        clean = re.sub(r"[\x00-\x1f\x7f]", " ", description)
        clean = re.sub(
            r"(?i)(authorization\s*:\s*bearer\s+)[^\s,;]+",
            r"\1[REDACTED]",
            clean,
        )
        clean = re.sub(
            r"(?i)\b(api[_-]?key|token|password|secret)\s*[:=]\s*[^\s,;]+",
            r"\1=[REDACTED]",
            clean,
        )
        clean = re.sub(
            r"(?i)(ignore\s+(all\s+)?previous|system\s+prompt|developer\s+message|jailbreak)",
            "[untrusted-instruction]",
            clean,
        )
        return " ".join(clean.split())[:MAX_DESCRIPTION_LENGTH]


__all__ = [
    "MCPClient",
    "MCPError",
    "MCPServerConfig",
    "MCPStaticToolCapability",
    "MCPTool",
    "MCPToolResult",
    "MCP_PROTOCOL_VERSION",
]
