"""
Low-level MCP client that talks to a single MCP server over
stdio (subprocess) or HTTP (Streamable HTTP / SSE) transport.

Implements the JSON-RPC 2.0 message framing required by the
Model Context Protocol specification (protocol version 2025-11-25).
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

@dataclass
class MCPTool:
    """A single tool exposed by an MCP server."""

    name: str
    description: str
    input_schema: dict[str, Any]
    server_name: str


@dataclass
class MCPServerConfig:
    """Connection parameters for one MCP server.

    Provide *either* ``command`` (stdio transport) *or* ``url`` (HTTP transport).

    Examples::

        # stdio — spawns a subprocess
        MCPServerConfig(name="filesystem", command="npx",
                        args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

        # HTTP — connects to a running server
        MCPServerConfig(name="remote", url="http://localhost:3001/mcp")
    """

    name: str
    command: str | None = None
    args: list[str] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    url: str | None = None
    api_key: str | None = None


# ---------------------------------------------------------------------------
# MCP Client
# ---------------------------------------------------------------------------

class MCPClient:
    """Connect to a single MCP server via stdio (subprocess) or HTTP.

    Lifecycle::

        client = MCPClient(config)
        tools  = await client.connect()   # handshake + tool discovery
        result = await client.call_tool("read_file", {"path": "/etc/hosts"})
        await client.close()              # terminate subprocess / close HTTP
    """

    PROTOCOL_VERSION = "2025-11-25"
    CLIENT_INFO = {"name": "ai-gateway-sdk", "version": "1.0.0"}

    def __init__(self, config: MCPServerConfig) -> None:
        self.config = config
        self.tools: list[MCPTool] = []
        self._process: asyncio.subprocess.Process | None = None
        self._request_id: int = 0
        self._initialized: bool = False
        # Pending futures keyed by JSON-RPC request id
        self._pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self._reader_task: asyncio.Task[None] | None = None

    # -- Public API ---------------------------------------------------------

    async def connect(self) -> list[MCPTool]:
        """Start the MCP server and perform the initialize handshake.

        Returns the list of tools discovered on the server.
        """
        if self.config.command:
            return await self._connect_stdio()
        if self.config.url:
            return await self._connect_http()
        raise ValueError(
            "MCPServerConfig must have either 'command' (stdio) or 'url' (HTTP)"
        )

    async def call_tool(self, tool_name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        """Invoke a tool on the connected MCP server.

        Args:
            tool_name: The unqualified tool name (as reported by the server).
            arguments: Tool-specific input arguments.

        Returns:
            The ``result`` payload from the server's JSON-RPC response.
        """
        if not self._initialized:
            raise RuntimeError("MCP client is not connected — call connect() first")
        return await self._jsonrpc("tools/call", {"name": tool_name, "arguments": arguments})

    async def close(self) -> None:
        """Shut down the connection. Safe to call multiple times."""
        # Cancel the reader task first
        if self._reader_task and not self._reader_task.done():
            self._reader_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._reader_task
            self._reader_task = None

        # Terminate subprocess
        if self._process and self._process.returncode is None:
            logger.debug("Terminating MCP server %s (pid %s)", self.config.name, self._process.pid)
            self._process.terminate()
            try:
                await asyncio.wait_for(self._process.wait(), timeout=5.0)
            except TimeoutError:
                logger.warning("MCP server %s did not exit in time — killing", self.config.name)
                self._process.kill()
                await self._process.wait()

        # Reject any still-pending futures
        for fut in self._pending.values():
            if not fut.done():
                fut.set_exception(ConnectionError("MCP client closed"))
        self._pending.clear()

        self._initialized = False

    # -- Async context manager ----------------------------------------------

    async def __aenter__(self) -> MCPClient:
        await self.connect()
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.close()

    # -- stdio transport ----------------------------------------------------

    async def _connect_stdio(self) -> list[MCPTool]:
        """Spawn the MCP server as a child process and perform the handshake."""
        merged_env = {**os.environ, **self.config.env}
        self._process = await asyncio.create_subprocess_exec(
            self.config.command,
            *self.config.args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=merged_env,
        )
        logger.info(
            "Started MCP server %s (pid %s): %s %s",
            self.config.name, self._process.pid, self.config.command, self.config.args,
        )

        # Start background reader so we can match responses to requests
        self._reader_task = asyncio.create_task(
            self._stdio_reader(), name=f"mcp-reader-{self.config.name}"
        )

        # JSON-RPC initialize
        await self._jsonrpc("initialize", {
            "protocolVersion": self.PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "clientInfo": self.CLIENT_INFO,
        })

        # Notify the server that initialisation is complete
        await self._notify("notifications/initialized")
        self._initialized = True

        # Discover tools
        tools_result = await self._jsonrpc("tools/list", {})
        self.tools = [
            MCPTool(
                name=t["name"],
                description=t.get("description", ""),
                input_schema=t.get("inputSchema", {}),
                server_name=self.config.name,
            )
            for t in tools_result.get("tools", [])
        ]
        logger.info(
            "MCP server %s: discovered %d tools: %s",
            self.config.name, len(self.tools), [t.name for t in self.tools],
        )
        return self.tools

    async def _stdio_reader(self) -> None:
        """Background task: read lines from the subprocess stdout and dispatch."""
        assert self._process and self._process.stdout
        try:
            while True:
                line = await self._process.stdout.readline()
                if not line:
                    break  # EOF — process exited
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    logger.warning("MCP %s: non-JSON line: %s", self.config.name, line[:200])
                    continue

                msg_id = msg.get("id")
                if msg_id is not None and msg_id in self._pending:
                    fut = self._pending.pop(msg_id)
                    if "error" in msg:
                        fut.set_exception(
                            MCPError(msg["error"].get("message", "Unknown MCP error"),
                                     code=msg["error"].get("code"))
                        )
                    else:
                        fut.set_result(msg.get("result", {}))
                else:
                    # Server-initiated notification or unknown id — log and skip
                    logger.debug(
                        "MCP %s notification: %s",
                        self.config.name,
                        msg.get("method", msg),
                    )
        except asyncio.CancelledError:
            return

    # -- HTTP transport -----------------------------------------------------

    async def _connect_http(self) -> list[MCPTool]:
        """Connect to an HTTP-based MCP server and perform the handshake."""
        try:
            import httpx
        except ImportError as exc:
            raise ImportError(
                "httpx is required for HTTP MCP transport — pip install httpx"
            ) from exc

        headers: dict[str, str] = {"Content-Type": "application/json"}
        if self.config.api_key:
            headers["Authorization"] = f"Bearer {self.config.api_key}"

        async with httpx.AsyncClient(
            base_url=self.config.url,  # type: ignore[arg-type]
            headers=headers,
            timeout=30.0,
        ) as http:
            # Initialize
            init_resp = await http.post("/", json=self._make_request("initialize", {
                "protocolVersion": self.PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "clientInfo": self.CLIENT_INFO,
            }))
            init_resp.raise_for_status()
            init_data = init_resp.json()
            if "error" in init_data:
                raise MCPError(init_data["error"].get("message", "init failed"))

            # Notify initialized
            await http.post("/", json={
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            })

            self._initialized = True

            # Discover tools
            tools_resp = await http.post("/", json=self._make_request("tools/list", {}))
            tools_resp.raise_for_status()
            tools_data = tools_resp.json()
            if "error" in tools_data:
                raise MCPError(tools_data["error"].get("message", "tools/list failed"))

            self.tools = [
                MCPTool(
                    name=t["name"],
                    description=t.get("description", ""),
                    input_schema=t.get("inputSchema", {}),
                    server_name=self.config.name,
                )
                for t in tools_data.get("result", {}).get("tools", [])
            ]

        logger.info(
            "MCP server %s (HTTP): discovered %d tools: %s",
            self.config.name, len(self.tools), [t.name for t in self.tools],
        )
        return self.tools

    # -- JSON-RPC helpers ---------------------------------------------------

    def _next_id(self) -> int:
        self._request_id += 1
        return self._request_id

    def _make_request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": method,
            "params": params,
        }

    async def _jsonrpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        """Send a JSON-RPC request over stdio and await the matching response."""
        assert self._process and self._process.stdin
        req_id = self._next_id()
        request = json.dumps({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params,
        }) + "\n"

        loop = asyncio.get_running_loop()
        fut: asyncio.Future[dict[str, Any]] = loop.create_future()
        self._pending[req_id] = fut

        self._process.stdin.write(request.encode())
        await self._process.stdin.drain()

        try:
            return await asyncio.wait_for(fut, timeout=30.0)
        except TimeoutError as exc:
            self._pending.pop(req_id, None)
            raise TimeoutError(
                f"MCP server {self.config.name} did not respond to {method} within 30 s"
            ) from exc

    async def _notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        """Send a JSON-RPC notification (no ``id``, no response expected)."""
        assert self._process and self._process.stdin
        notification = json.dumps({
            "jsonrpc": "2.0",
            "method": method,
            "params": params or {},
        }) + "\n"
        self._process.stdin.write(notification.encode())
        await self._process.stdin.drain()


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------

class MCPError(Exception):
    """An error returned by an MCP server in a JSON-RPC error response."""

    def __init__(self, message: str, *, code: int | None = None) -> None:
        self.code = code
        super().__init__(message)
