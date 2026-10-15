"""Safe parser for Agent Plugins 1.0.0 MCP declarations."""

from __future__ import annotations

import ipaddress
import json
import re
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Final

MCP_SCHEMA_V1: Final = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json"

_MAX_MCP_CONFIG_BYTES: Final = 64 * 1024
_HTTP_HEADER_NAME_RE: Final = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")
_ENV_NAME_RE: Final = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


@dataclass(frozen=True)
class AgentPluginMCPServer:
    """One validated portable MCP declaration."""

    name: str
    transport: str
    url: str = ""
    headers: dict[str, str] = field(default_factory=dict)
    command: str = ""
    args: tuple[str, ...] = ()
    env: dict[str, str] = field(default_factory=dict)
    cwd: str = ""


@dataclass(frozen=True)
class AgentPluginMCPDiagnostic:
    """One content-free diagnostic produced while parsing ``mcp.json``."""

    code: str
    component: str


@dataclass(frozen=True)
class AgentPluginMCPParseResult:
    """Parsed MCP component plus non-fatal server diagnostics."""

    present: bool
    servers: tuple[AgentPluginMCPServer, ...] = ()
    diagnostics: tuple[AgentPluginMCPDiagnostic, ...] = ()


class _MCPJSONDuplicateKey(ValueError):
    """Internal marker for duplicate JSON object keys."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _MCPJSONDuplicateKey
        result[key] = value
    return result


def _diagnostic(code: str, component: str = "mcp.json") -> AgentPluginMCPDiagnostic:
    return AgentPluginMCPDiagnostic(code=code, component=component)


def _failed_component(code: str) -> AgentPluginMCPParseResult:
    return AgentPluginMCPParseResult(present=True, diagnostics=(_diagnostic(code),))


def _read_mcp_config(root: Path) -> tuple[str | None, str | None]:
    try:
        path = (root / "mcp.json").resolve(strict=True)
    except OSError:
        return None, "AGENT_PLUGIN_MCP_COMPONENT_INVALID"
    if not path.is_relative_to(root) or not path.is_file():
        return None, "AGENT_PLUGIN_MCP_COMPONENT_INVALID"
    try:
        if path.stat().st_size > _MAX_MCP_CONFIG_BYTES:
            return None, "AGENT_PLUGIN_MCP_COMPONENT_INVALID_TOO_LARGE"
        return path.read_text(encoding="utf-8"), None
    except OSError:
        return None, "AGENT_PLUGIN_MCP_COMPONENT_INVALID"
    except UnicodeError:
        return None, "AGENT_PLUGIN_MCP_COMPONENT_INVALID_ENCODING"


def _valid_remote_url(value: Any) -> bool:
    if not isinstance(value, str) or not value:
        return False
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError:
        return False
    hostname = parsed.hostname
    if (
        parsed.scheme.lower() not in {"http", "https"}
        or not hostname
        or parsed.username is not None
        or parsed.password is not None
        or bool(parsed.fragment)
    ):
        return False
    if port is not None and not 1 <= port <= 65535:
        return False
    if parsed.scheme.lower() == "https" or hostname.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(hostname).is_loopback
    except ValueError:
        return False


def _valid_headers(value: Any) -> dict[str, str] | None:
    if value is None:
        return {}
    if not isinstance(value, dict):
        return None

    normalized_names: set[str] = set()
    headers: dict[str, str] = {}
    for name, header_value in value.items():
        if not isinstance(name, str) or not _HTTP_HEADER_NAME_RE.fullmatch(name):
            return None
        if not isinstance(header_value, str) or any(
            (ord(char) < 32 and char != "\t") or ord(char) == 127 for char in header_value
        ):
            return None
        normalized = name.lower()
        if normalized in normalized_names:
            return None
        normalized_names.add(normalized)
        headers[name] = header_value
    return headers


def _contained_plugin_path(root: Path, value: str, *, directory: bool) -> bool:
    try:
        resolved = (root / value.removeprefix("./")).resolve(strict=True)
    except OSError:
        return False
    if not resolved.is_relative_to(root):
        return False
    return resolved.is_dir() if directory else resolved.is_file()


def _valid_stdio_cwd(root: Path, cwd: Any) -> bool:
    if not isinstance(cwd, str) or "\x00" in cwd:
        return False
    if not cwd:
        return True
    if cwd.startswith("./"):
        return _contained_plugin_path(root, cwd, directory=True)
    if cwd == "${PLUGIN_ROOT}" or cwd.startswith("${PLUGIN_ROOT}/"):
        relative = cwd.removeprefix("${PLUGIN_ROOT}").lstrip("/")
        return not relative or _contained_plugin_path(root, f"./{relative}", directory=True)
    if cwd == "${PLUGIN_DATA}" or cwd.startswith("${PLUGIN_DATA}/"):
        relative = cwd.removeprefix("${PLUGIN_DATA}").lstrip("/")
        return ".." not in Path(relative).parts
    return False


def _parse_stdio_server(
    root: Path,
    name: str,
    raw: dict[str, Any],
) -> AgentPluginMCPServer | None:
    if set(raw) - {"type", "command", "args", "env", "cwd"}:
        return None
    command = raw.get("command")
    if not isinstance(command, str) or not command or "\x00" in command:
        return None
    if command.startswith("./"):
        if not _contained_plugin_path(root, command, directory=False):
            return None
    elif "/" in command or "\\" in command or command in {".", ".."}:
        return None

    args = raw.get("args", [])
    if not isinstance(args, list) or any(
        not isinstance(value, str) or "\x00" in value for value in args
    ):
        return None
    env = raw.get("env", {})
    if not isinstance(env, dict) or any(
        not isinstance(key, str)
        or not _ENV_NAME_RE.fullmatch(key)
        or key in {"PLUGIN_ROOT", "PLUGIN_DATA"}
        or not isinstance(value, str)
        or "\x00" in value
        for key, value in env.items()
    ):
        return None
    cwd = raw.get("cwd", "")
    if not _valid_stdio_cwd(root, cwd):
        return None

    return AgentPluginMCPServer(
        name=name,
        transport="stdio",
        command=command,
        args=tuple(args),
        env={str(key): str(value) for key, value in env.items()},
        cwd=cwd,
    )


def _parse_http_server(name: str, raw: dict[str, Any]) -> AgentPluginMCPServer | None:
    if set(raw) - {"type", "url", "headers"}:
        return None
    headers = _valid_headers(raw.get("headers"))
    if headers is None or not _valid_remote_url(raw.get("url")):
        return None
    return AgentPluginMCPServer(
        name=name,
        transport="streamable-http",
        url=str(raw["url"]),
        headers=headers,
    )


def _parse_server(
    root: Path,
    name: str,
    raw: dict[str, Any],
) -> tuple[AgentPluginMCPServer | None, str | None]:
    transport = raw.get("type")
    if transport == "stdio":
        return _parse_stdio_server(root, name, raw), None
    if transport == "streamable-http":
        return _parse_http_server(name, raw), None
    if transport == "sse":
        return None, "AGENT_PLUGIN_MCP_TRANSPORT_UNSUPPORTED"
    return None, None


def load_agent_plugin_mcp(root: Path) -> AgentPluginMCPParseResult:
    """Parse one optional ``mcp.json`` without executing plugin code."""

    if not (root / "mcp.json").exists():
        return AgentPluginMCPParseResult(present=False)

    text, read_error = _read_mcp_config(root)
    if read_error is not None:
        return _failed_component(read_error)
    try:
        raw = json.loads(text or "", object_pairs_hook=_reject_duplicate_keys)
    except (_MCPJSONDuplicateKey, json.JSONDecodeError):
        return _failed_component("AGENT_PLUGIN_MCP_JSON_INVALID")
    if not isinstance(raw, dict) or set(raw) != {"$schema", "mcpServers"}:
        return _failed_component("AGENT_PLUGIN_MCP_TOP_LEVEL_INVALID")
    if raw.get("$schema") != MCP_SCHEMA_V1:
        return _failed_component("AGENT_PLUGIN_MCP_SCHEMA_UNSUPPORTED")
    raw_servers = raw.get("mcpServers")
    if not isinstance(raw_servers, dict):
        return _failed_component("AGENT_PLUGIN_MCP_TOP_LEVEL_INVALID")

    servers: list[AgentPluginMCPServer] = []
    diagnostics: list[AgentPluginMCPDiagnostic] = []
    for raw_name, raw_server in raw_servers.items():
        component = str(raw_name)
        if not isinstance(raw_server, dict):
            diagnostics.append(_diagnostic("AGENT_PLUGIN_MCP_SERVER_INVALID", component))
            continue

        server, diagnostic_code = _parse_server(root, raw_name, raw_server)
        if server is not None:
            servers.append(server)
        else:
            diagnostics.append(
                _diagnostic(diagnostic_code or "AGENT_PLUGIN_MCP_SERVER_INVALID", component)
            )

    return AgentPluginMCPParseResult(
        present=True,
        servers=tuple(servers),
        diagnostics=tuple(diagnostics),
    )


__all__ = [
    "AgentPluginMCPParseResult",
    "AgentPluginMCPServer",
    "MCP_SCHEMA_V1",
    "load_agent_plugin_mcp",
]
