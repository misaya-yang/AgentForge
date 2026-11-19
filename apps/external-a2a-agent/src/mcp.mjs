// Implements the pinned SDK MCPServer transport interface. Model/tool iteration
// stays in OpenAI Runner. Gateway's stateless MCP 2026 profile has no initialize.
import { randomUUID } from 'node:crypto';
import { parseStrict, reject } from './contract.mjs';
export const MCP_VERSION = '2026-07-28';
export class GovernedMCPServer {
  constructor(config, fetch, signal) { this.config = config; this.fetch = fetch; this.signal = signal; this.name = 'governed_gateway'; this.cacheToolsList = false; this.errorFunction = null; this.useStructuredContent = true; this.toolFilter = { allowedToolNames: config.model.mcp.allowedTools }; }
  async rpc(method, params, signal = this.signal) {
    const id = randomUUID(); const meta = { 'io.modelcontextprotocol/protocolVersion': MCP_VERSION, 'io.modelcontextprotocol/clientCapabilities': {} };
    const response = await this.fetch(`${this.config.gatewayOrigin}/mcp`, { method: 'POST', headers: { 'content-type': 'application/json', 'mcp-protocol-version': MCP_VERSION, 'mcp-method': method, ...(method === 'tools/call' ? { 'mcp-name': params.name } : {}) }, body: JSON.stringify({ jsonrpc: '2.0', id, method, params: { ...params, _meta: meta } }), signal });
    if (!response.ok) reject(502, 'MCP_GATEWAY_REJECTED');
    const value = parseStrict(Buffer.from(await response.arrayBuffer()), 4194304); if (value.id !== id || value.jsonrpc !== '2.0' || value.error || !value.result || value.result.resultType !== 'complete') reject(502, 'MCP_RESPONSE_INVALID'); return value.result;
  }
  async connect() { const discovery = await this.rpc('server/discover', {}); if (!discovery.supportedVersions?.includes(MCP_VERSION) || !discovery.capabilities?.tools) reject(502, 'MCP_PROFILE_UNSUPPORTED'); }
  async close() {}
  async invalidateToolsCache() {}
  async listTools() { const result = await this.rpc('tools/list', {}); if (!Array.isArray(result.tools) || result.nextCursor) reject(502, 'MCP_BOUNDED_LIST_REQUIRED'); const tools = result.tools.filter(t => this.config.model.mcp.allowedTools.includes(t.name)); if (!tools.length || tools.some(t => t.annotations?.readOnlyHint !== true || t.annotations?.destructiveHint === true)) reject(403, 'MCP_READ_ONLY_PROFILE_REQUIRED'); return tools; }
  async callToolResult(name, args, _meta, options = {}) { if (!this.config.model.mcp.allowedTools.includes(name)) reject(403, 'MCP_TOOL_DENIED'); return await this.rpc('tools/call', { name, arguments: args ?? {} }, options.signal ? AbortSignal.any([this.signal, options.signal]) : this.signal); }
  async callTool(...args) { const result = await this.callToolResult(...args); return result.content; }
}
