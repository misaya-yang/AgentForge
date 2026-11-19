import https from 'node:https';
import { parseStrict, sha256, reject, Rejection } from './contract.mjs';
// One exact pinned Gateway origin, no proxy/redirect/default-client escape.
export function pinnedFetch(config, { owner, taskId, store, signal, token, authorize = () => {}, kind = 'model' }) {
  return async (input, options) => {
    const request = new Request(input, options); const url = new URL(request.url); const path = kind === 'model' ? '/v1/chat/completions' : config.model.mcp.path;
    if (url.origin !== config.gatewayOrigin || url.pathname !== path || url.search || url.hash || url.username || url.password || (kind === 'model' ? request.method !== 'POST' : !['GET', 'POST', 'DELETE'].includes(request.method))) reject(403, 'GOVERNED_EGRESS_DENIED');
    if (signal.aborted || request.signal.aborted) reject(409, 'EXECUTION_STOPPED');
    const bytes = Buffer.from(await request.arrayBuffer()); if (bytes.length > 1048576) reject(413, 'MODEL_REQUEST_TOO_LARGE');
    authorize();
    let call = null; let mcpCall = null;
    if (kind === 'model') {
      const body = parseStrict(bytes, 1048576);
      if (body.model !== config.model.name || body.stream === true || body.background === true || body.store === true || !Number.isInteger(body.max_tokens) || body.max_tokens < 1 || body.max_tokens > config.limits.maxOutputTokens) reject(403, 'MODEL_REQUEST_SCOPE_REJECTED');
      call = store.dispatch(owner, taskId, sha256(bytes), config.limits.maxModelCalls);
    } else if (request.method === 'POST') {
      const rpc = parseStrict(bytes, 1048576);
      if (!['server/discover', 'tools/list', 'tools/call'].includes(rpc.method)) reject(403, 'MCP_METHOD_DENIED');
      if (rpc.method === 'tools/call') { if (!config.model.mcp.allowedTools.includes(rpc.params?.name)) reject(403, 'MCP_TOOL_DENIED'); mcpCall = store.capability(owner, taskId, rpc.params.name, sha256(bytes), config.limits.maxTurns); }
    }
    const abortSignal = AbortSignal.any([signal, request.signal, AbortSignal.timeout(Math.min(30000, config.limits.maxRunMs))]);
    const headers = { authorization: `Bearer ${token}`, accept: kind === 'model' ? 'application/json' : 'application/json, text/event-stream', 'content-type': 'application/json', 'x-agentforge-a2a-task-id': taskId };
    if (call || mcpCall) headers['x-request-id'] = call?.id ?? mcpCall;
    if (kind === 'mcp') for (const name of ['mcp-protocol-version', 'mcp-method', 'mcp-name']) { const value = request.headers.get(name); if (value != null) { if (value.length > 512 || /[\r\n]/.test(value)) reject(400, 'MCP_HEADER_INVALID'); headers[name] = value; } }
    return await new Promise((resolve, fail) => {
      const upstream = https.request(url, { method: request.method, headers, ca: config.modelCA, rejectUnauthorized: true, servername: url.hostname, lookup: (_host, opts, cb) => { const address = { address: config.model.address, family: config.model.address.includes(':') ? 6 : 4 }; cb(null, opts?.all ? [address] : address.address, address.family); }, signal: abortSignal }, response => {
        const chunks = []; let size = 0; let failed = false;
        response.on('data', chunk => { size += chunk.length; if (size > 4194304) { failed = true; upstream.destroy(new Rejection(502, 'GOVERNED_RESPONSE_TOO_LARGE')); return; } chunks.push(chunk); });
        response.on('error', () => fail(new Rejection(502, 'GOVERNED_EFFECT_UNKNOWN')));
        response.on('end', () => {
          if (failed) return;
          try {
            const raw = Buffer.concat(chunks); const status = response.statusCode;
            if (status >= 300 && status < 400) reject(502, 'GOVERNED_REDIRECT_REJECTED');
            if (kind === 'model') {
              const body = parseStrict(raw, 4194304);
              if (status >= 200 && status < 300) {
                if (typeof body.id !== 'string' || !body.id || body.model !== (config.model.responseModel ?? config.model.name) || !Number.isSafeInteger(body.usage?.prompt_tokens) || !Number.isSafeInteger(body.usage?.completion_tokens) || body.usage.prompt_tokens < 0 || body.usage.completion_tokens < 0) reject(502, 'MODEL_RECEIPT_MISSING');
                if (typeof response.headers['x-gateway-request-id'] !== 'string' || !/^llm_[0-9a-f]{32}$/.test(response.headers['x-gateway-request-id'])) reject(502, 'GATEWAY_RECEIPT_ID_MISSING');
                store.response(taskId, call.sequence, { gatewayRequestId: response.headers['x-gateway-request-id'], effect: 'confirmed', status, responseId: body.id, inputTokens: body.usage.prompt_tokens, outputTokens: body.usage.completion_tokens });
              } else store.response(taskId, call.sequence, { gatewayRequestId: response.headers['x-gateway-request-id'] ?? null, effect: status >= 500 ? 'unknown' : 'rejected', status });
            } else if (status >= 200 && status < 300 && raw.length) {
              if (!String(response.headers['content-type'] ?? '').includes('application/json')) reject(502, 'MCP_JSON_PROFILE_REQUIRED');
              const rpc = parseStrict(raw, 4194304);
              if (mcpCall) store.capabilityResponse(taskId, mcpCall, !rpc.error && rpc.result && rpc.result.isError !== true ? 'confirmed' : 'rejected', status);
              if (Array.isArray(rpc.result?.tools)) for (const tool of rpc.result.tools) if (config.model.mcp.allowedTools.includes(tool.name) && tool.annotations?.readOnlyHint !== true) reject(403, 'MCP_READ_ONLY_PROFILE_REQUIRED');
            }
            if (mcpCall && status >= 400) store.capabilityResponse(taskId, mcpCall, status >= 500 ? 'unknown' : 'rejected', status);
            const outgoing = new Headers(); for (const [name, value] of Object.entries(response.headers)) if (['content-type', 'mcp-session-id', 'mcp-protocol-version', 'x-request-id', 'x-gateway-request-id', 'retry-after'].includes(name) && typeof value === 'string') outgoing.set(name, value);
            resolve(new Response([204, 205, 304].includes(status) ? null : raw, { status, headers: outgoing }));
          } catch (e) { fail(e); }
        });
      });
      upstream.on('error', () => fail(new Rejection(502, 'GOVERNED_EFFECT_UNKNOWN'))); upstream.end(bytes);
    });
  };
}
