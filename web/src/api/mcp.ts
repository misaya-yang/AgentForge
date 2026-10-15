/**
 * MCP (Model Context Protocol) API client.
 */
import { api } from "@/lib/api";

export interface MCPServer {
  name: string;
  url: string;
  description: string;
  enabled: boolean;
  connected: boolean;
  tool_count: number;
}

export interface MCPTool {
  name: string;
  description: string;
  category: string;
}

export async function listMCPServers() {
  const { data } = await api.get<{ servers: MCPServer[] }>("/api/v1/assistant/mcp/servers");
  return data;
}

export async function listMCPTools() {
  const { data } = await api.get<{ tools: MCPTool[]; total: number }>("/api/v1/assistant/mcp/tools");
  return data;
}

export async function refreshMCPServer(name: string) {
  const { data } = await api.post<{ server: string; tools_refreshed: number }>(
    `/api/v1/assistant/mcp/servers/${name}/refresh`,
  );
  return data;
}
