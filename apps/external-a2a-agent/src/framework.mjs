import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { existsSync, readFileSync } from 'node:fs';
import { GovernedMCPServer } from './mcp.mjs';
import { SDK } from './config.mjs';
import { pinnedFetch } from './network.mjs';
import { reject } from './contract.mjs';
const require = createRequire(import.meta.url);
function version(name) { let directory = dirname(require.resolve(name)); for (let i = 0; i < 6; i++) { const file = join(directory, 'package.json'); if (existsSync(file)) { const p = JSON.parse(readFileSync(file, 'utf8')); if (p.name === name) return p.version; } directory = dirname(directory); } reject(503, 'FRAMEWORK_VERSION_UNAVAILABLE'); }
export async function runFramework({ config, owner, taskId, text, store, signal, authorize }) {
  if (typeof authorize !== 'function') reject(503, 'ADMISSION_RECHECK_REQUIRED'); authorize();
  // This is the actual third-party SDK Runner, not a platform model/tool loop.
  const [{ Agent, Runner, tool }, { OpenAIProvider }, { default: OpenAI }] = await Promise.all([import('@openai/agents-core'), import('@openai/agents-openai'), import('openai')]);
  if (version('@openai/agents-core') !== SDK.version || version('@openai/agents-openai') !== SDK.providerVersion || version('openai') !== SDK.clientVersion || version('zod') !== SDK.zodVersion || (config.model.mcp && version('@modelcontextprotocol/client') !== SDK.mcpVersion)) reject(503, 'FRAMEWORK_VERSION_MISMATCH');
  const credential = config.credentials.get(`${owner.tenant_id}\0${owner.user_id}`); if (!credential) reject(403, 'PRINCIPAL_MODEL_CREDENTIAL_UNAVAILABLE');
  const client = new OpenAI({ apiKey: credential.token, baseURL: config.model.baseURL, maxRetries: 0, timeout: config.limits.maxRunMs, fetch: pinnedFetch(config, { owner, taskId, store, signal, authorize, token: credential.token }) });
  let toolCalls = 0; const contractTool = tool({ name: 'describe_a2a_contract', description: 'Read the fixed protocol and cancellation contract. Has no network or write effects.', parameters: { type: 'object', properties: {}, required: [], additionalProperties: false }, execute: async () => { if (signal.aborted) reject(409, 'EXECUTION_STOPPED'); toolCalls++; return 'A2A 1.0 HTTP+JSON. Tasks are owner-isolated and idempotent. Cancellation is durable and never claims to undo billing or external effects.'; } });
  const mcp = config.model.mcp ? new GovernedMCPServer(config, pinnedFetch(config, { owner, taskId, store, signal, authorize, token: credential.mcpToken, kind: 'mcp' }), signal) : null;
  try {
    if (mcp) await mcp.connect();
    const agent = new Agent({ name: 'Governed A2A reference agent', instructions: 'Use describe_a2a_contract once. If governed MCP tools are supplied, call one allowed read-only MCP tool once before answering the user briefly. Only the supplied read-only tools are available. Never claim an action, refund, approval or platform acceptance that was not verified.', model: config.model.name, modelSettings: { maxTokens: config.limits.maxOutputTokens, toolChoice: 'describe_a2a_contract', store: false }, resetToolChoice: true, tools: [contractTool], mcpServers: mcp ? [mcp] : [] });
    const runner = new Runner({ modelProvider: new OpenAIProvider({ openAIClient: client, useResponses: false, useResponsesWebSocket: false }), tracingDisabled: true });
    const result = await runner.run(agent, text, { signal, maxTurns: config.limits.maxTurns, context: { taskId } });
    if (typeof result.finalOutput !== 'string' || !result.finalOutput.trim() || Buffer.byteLength(result.finalOutput) > 65536 || !Array.isArray(result.rawResponses) || !result.rawResponses.length || toolCalls < 1 || (mcp && store.successfulCapabilities(taskId) < 1)) reject(502, 'FRAMEWORK_RESULT_INVALID');
    return { output: result.finalOutput, framework: `openai-agents-js@${SDK.version}`, frameworkCalls: result.rawResponses.length, toolCalls };
  } finally { if (mcp) await mcp.close().catch(() => {}); }
}
