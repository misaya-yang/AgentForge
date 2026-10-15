"""
MCP Manager — orchestrates connections to multiple local MCP servers
and exposes a unified, namespaced tool catalogue.

Tool names are qualified as ``mcp_{server_name}__{tool_name}`` so they
never collide across servers or with server-side tools.
"""

from __future__ import annotations

import logging
from typing import Any

from .client import MCPClient, MCPServerConfig, MCPTool

logger = logging.getLogger(__name__)


class MCPManager:
    """Manage multiple local MCP server connections.

    Example::

        mgr = MCPManager()
        await mgr.connect("fs", command="npx", args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
        await mgr.connect("github", command="npx", args=["-y", "@modelcontextprotocol/server-github"],
                          env={"GITHUB_TOKEN": "ghp_..."})

        print(mgr.list_tools())
        result = await mgr.call_tool("mcp_fs__read_file", {"path": "/tmp/hello.txt"})

        await mgr.disconnect()  # shut down all servers
    """

    def __init__(self) -> None:
        self._clients: dict[str, MCPClient] = {}
        # Qualified tool name -> (owning client, tool metadata)
        self._tools: dict[str, tuple[MCPClient, MCPTool]] = {}

    # -- Connection management ----------------------------------------------

    async def connect(
        self,
        name: str,
        *,
        command: str | None = None,
        args: list[str] | None = None,
        url: str | None = None,
        api_key: str | None = None,
        env: dict[str, str] | None = None,
    ) -> list[MCPTool]:
        """Connect to an MCP server and register its tools.

        Args:
            name: A short identifier for this server (e.g. ``"fs"``, ``"github"``).
            command: Executable for stdio transport (``"npx"``, ``"python"``, ...).
            args: Arguments passed to the subprocess.
            url: Base URL for HTTP transport.
            api_key: Optional bearer token for HTTP transport.
            env: Extra environment variables merged into the subprocess env.

        Returns:
            The list of :class:`MCPTool` objects discovered on the server.
        """
        if name in self._clients:
            logger.warning("MCP server %s is already connected — disconnecting first", name)
            await self.disconnect(name)

        config = MCPServerConfig(
            name=name,
            command=command,
            args=args or [],
            url=url,
            api_key=api_key,
            env=env or {},
        )
        client = MCPClient(config)
        tools = await client.connect()

        self._clients[name] = client
        for tool in tools:
            qualified = self._qualify(name, tool.name)
            self._tools[qualified] = (client, tool)
            logger.debug("Registered MCP tool: %s", qualified)

        return tools

    # -- Tool invocation ----------------------------------------------------

    async def call_tool(self, qualified_name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        """Call a tool by its qualified name (``mcp_server__tool``).

        Raises:
            KeyError: If the tool name is not found in the registry.
        """
        if qualified_name not in self._tools:
            raise KeyError(f"Unknown MCP tool: {qualified_name}")
        client, tool = self._tools[qualified_name]
        return await client.call_tool(tool.name, arguments)

    # -- Discovery ----------------------------------------------------------

    def list_tools(self) -> list[dict[str, Any]]:
        """Return a list of all registered MCP tools across every connected server.

        Each entry contains ``name`` (qualified), ``description``, ``server``,
        and ``input_schema``.
        """
        return [
            {
                "name": qn,
                "description": tool.description,
                "server": tool.server_name,
                "input_schema": tool.input_schema,
            }
            for qn, (_, tool) in self._tools.items()
        ]

    def get_tools_for_server(self, name: str) -> list[dict[str, Any]]:
        """Return tools registered under a specific server name."""
        return [
            entry
            for entry in self.list_tools()
            if entry["server"] == name
        ]

    # -- Disconnection ------------------------------------------------------

    async def disconnect(self, name: str | None = None) -> None:
        """Disconnect one server (by name) or all servers (if *name* is ``None``).

        Idempotent — disconnecting a server that is not connected is a no-op.
        """
        if name is not None:
            client = self._clients.pop(name, None)
            if client is not None:
                await client.close()
                # Remove tools belonging to this client
                self._tools = {
                    k: v for k, v in self._tools.items() if v[0] is not client
                }
                logger.info("Disconnected MCP server: %s", name)
        else:
            for client in self._clients.values():
                await client.close()
            self._clients.clear()
            self._tools.clear()
            logger.info("Disconnected all MCP servers")

    # -- Properties ---------------------------------------------------------

    @property
    def servers(self) -> list[str]:
        """Names of currently connected MCP servers."""
        return list(self._clients.keys())

    def is_connected(self, name: str) -> bool:
        """Check whether a server is currently connected."""
        return name in self._clients

    # -- Async context manager ----------------------------------------------

    async def __aenter__(self) -> MCPManager:
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.disconnect()

    # -- Internals ----------------------------------------------------------

    @staticmethod
    def _qualify(server_name: str, tool_name: str) -> str:
        """Build a qualified tool name: ``mcp_{server}__{tool}``."""
        return f"mcp_{server_name}__{tool_name}"
