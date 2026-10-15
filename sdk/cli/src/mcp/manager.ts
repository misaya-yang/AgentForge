/**
 * MCP Manager — orchestrates multiple MCP server connections.
 * Ported from Python SDK: sdk/python/ai_assistant/mcp/manager.py
 *
 * Tool names are qualified as `mcp_{server}__{tool}` to avoid collisions.
 */

import { MCPClient, type MCPServerConfig, type MCPTool } from "./client.js";

export class MCPManager {
  private clients = new Map<string, MCPClient>();
  private tools = new Map<string, { client: MCPClient; tool: MCPTool }>();

  /**
   * Connect to an MCP server and register its tools.
   */
  async connect(
    name: string,
    command: string,
    args: string[],
    env?: Record<string, string>,
  ): Promise<MCPTool[]> {
    // Disconnect existing if re-connecting
    if (this.clients.has(name)) {
      await this.disconnect(name);
    }

    const config: MCPServerConfig = { name, command, args, env };
    const client = new MCPClient(config);
    const tools = await client.connect();

    this.clients.set(name, client);
    for (const tool of tools) {
      const qualified = MCPManager.qualify(name, tool.name);
      this.tools.set(qualified, { client, tool });
    }

    return tools;
  }

  /**
   * Call a tool by qualified name (mcp_server__tool).
   */
  async callTool(qualifiedName: string, args: Record<string, any>): Promise<any> {
    const entry = this.tools.get(qualifiedName);
    if (!entry) throw new Error(`Unknown MCP tool: ${qualifiedName}`);
    return entry.client.callTool(entry.tool.name, args);
  }

  /**
   * List all registered tools across all servers.
   */
  listTools(): Array<{
    name: string;
    description: string;
    server: string;
    inputSchema: Record<string, any>;
  }> {
    return Array.from(this.tools.entries()).map(([qn, { tool }]) => ({
      name: qn,
      description: tool.description,
      server: tool.serverName,
      inputSchema: tool.inputSchema,
    }));
  }

  /**
   * Disconnect one or all servers.
   */
  async disconnect(name?: string): Promise<void> {
    if (name) {
      const client = this.clients.get(name);
      if (client) {
        await client.close();
        this.clients.delete(name);
        // Remove tools for this server
        for (const [qn, entry] of this.tools) {
          if (entry.client === client) this.tools.delete(qn);
        }
      }
    } else {
      for (const client of this.clients.values()) {
        await client.close();
      }
      this.clients.clear();
      this.tools.clear();
    }
  }

  get servers(): string[] {
    return Array.from(this.clients.keys());
  }

  isConnected(name: string): boolean {
    return this.clients.has(name);
  }

  /**
   * Check if a tool name belongs to MCP.
   */
  isMCPTool(name: string): boolean {
    return this.tools.has(name);
  }

  static qualify(serverName: string, toolName: string): string {
    return `mcp_${serverName}__${toolName}`;
  }
}
