"""
Connector MCP Service — dynamically manages MCP connections for user connectors.

When a user activates Confluence (or other services), this module:
1. Registers Confluence search/read as tools via the ToolRegistry
2. Tools use the connector OAuth credentials stored by the gateway connector plane
3. On deactivation, tools are deregistered

Note: The server-side MCPClient uses HTTP transport, but mcp-atlassian
runs as a stdio subprocess. Instead of spawning subprocesses, we register
lightweight tool wrappers that call the Confluence REST API directly
using the stored credentials. This is functionally equivalent to MCP
but avoids subprocess management complexity in a Docker environment.
"""

from __future__ import annotations

import threading

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


class ConnectorMCPService:
    """Manages connector tool registration per tenant."""

    def __init__(self) -> None:
        # tenant_id:provider -> activated flag
        self._active: dict[str, bool] = {}
        # tenant_id:provider -> tool names registered
        self._tool_names: dict[str, list[str]] = {}

    async def start_confluence(
        self,
        tenant_id: str,
        domain: str,
        email: str,
        api_token: str,
    ) -> list[dict[str, str]]:
        """Activate Confluence tools for a tenant.

        Registers `confluence_read` and `confluence_write` meta-tools
        that use the provided credentials. Between them they expose 11
        Confluence operations via an `action` enum.
        """
        key = f"{tenant_id}:confluence"

        if key in self._active:
            await self.stop_connector(tenant_id, "confluence")

        # Import and register the confluence tools
        try:
            from ..tools.confluence_tool import register_confluence_tools
            register_confluence_tools(
                domain=domain,
                email=email,
                api_token=api_token,
                tenant_id=tenant_id,
            )
        except Exception as exc:
            logger.error(f"Failed to register Confluence tools: {exc}")
            raise RuntimeError(f"Failed to activate Confluence tools: {exc}")

        self._active[key] = True
        # Two meta-tools replacing the old 8 single-purpose ones.
        # Actions: read = search/read_page/list_spaces/get_space/list_children
        #          write = create_page/update_page/find_replace/comment/delete_page
        self._tool_names[key] = [
            "confluence_read",
            "confluence_write",
        ]

        tools = [
            {
                "name": "confluence_read",
                "description": "Read Confluence: search pages, read a page (by id/title/url), list spaces, get a space, list children.",
            },
            {
                "name": "confluence_write",
                "description": "Write Confluence: create_page / update_page / find_replace (safe partial edit) / comment / delete_page. Requires user approval.",
            },
        ]

        logger.info(
            f"Confluence tools activated for tenant {tenant_id}: "
            f"{len(tools)} tools from {domain}"
        )
        return tools

    async def stop_connector(self, tenant_id: str, provider: str) -> None:
        """Deactivate tools for a connector."""
        key = f"{tenant_id}:{provider}"
        self._active.pop(key, None)
        self._tool_names.pop(key, None)
        logger.info(f"Stopped {provider} tools for tenant {tenant_id}")

    def get_tools(self, tenant_id: str, provider: str) -> list[dict[str, str]]:
        key = f"{tenant_id}:{provider}"
        if key not in self._active:
            return []
        return [{"name": n, "description": ""} for n in self._tool_names.get(key, [])]

    def is_connected(self, tenant_id: str, provider: str) -> bool:
        return f"{tenant_id}:{provider}" in self._active

    @property
    def active_connections(self) -> list[str]:
        return list(self._active.keys())

    async def stop_all(self) -> None:
        self._active.clear()
        self._tool_names.clear()


# Singleton with double-checked locking
_connector_mcp_service: ConnectorMCPService | None = None
_connector_mcp_lock = threading.Lock()


def get_connector_mcp_service() -> ConnectorMCPService:
    global _connector_mcp_service
    # Fast path — already initialized
    if _connector_mcp_service is not None:
        return _connector_mcp_service
    # Slow path — acquire lock and re-check
    with _connector_mcp_lock:
        if _connector_mcp_service is None:
            _connector_mcp_service = ConnectorMCPService()
    return _connector_mcp_service
