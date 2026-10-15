"""
MCP (Model Context Protocol) client-side integration.

Connects the AI Assistant SDK to local MCP servers (Confluence, Filesystem,
GitHub, etc.) so their tools can be used alongside server-side AI.
"""

from .client import MCPClient, MCPServerConfig, MCPTool
from .manager import MCPManager

__all__ = ["MCPClient", "MCPManager", "MCPServerConfig", "MCPTool"]
