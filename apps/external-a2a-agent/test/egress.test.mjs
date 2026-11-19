import test from 'node:test';
import assert from 'node:assert/strict';
import { pinnedFetch } from '../src/network.mjs';
const owner = { tenant_id: 't', user_id: 'u' };
const config = { gatewayOrigin: 'https://gateway.example', model: { name: 'approved-model', mcp: { path: '/mcp', allowedTools: ['read_contract'] } }, limits: { maxOutputTokens: 64, maxRunMs: 1000, maxModelCalls: 1 } };
const opts = { owner, taskId: 'task', signal: new AbortController().signal, token: 'never-sent', store: { dispatch: () => assert.fail('must reject before dispatch') } };
const body = { model: 'approved-model', max_tokens: 64, store: false, stream: false };
test('exact governed egress rejects origin/path/redirect-style URL escape without sockets', async () => {
  const fetch = pinnedFetch(config, opts);
  for (const url of ['https://provider.example/v1/chat/completions', 'https://gateway.example/v1/responses', 'https://gateway.example/v1/chat/completions?redirect=1', 'https://gateway.example/mcp', 'https://user:password@gateway.example/v1/chat/completions']) await assert.rejects(() => fetch(url, { method: 'POST', body: JSON.stringify(body) }));
  await assert.rejects(() => fetch('https://gateway.example/v1/chat/completions', { method: 'GET' }));
});
test('governed model, store/stream/background and output budgets reject before dispatch', async () => {
  const fetch = pinnedFetch(config, opts);
  for (const patch of [{ model: 'unapproved' }, { store: true }, { stream: true }, { background: true }, { max_tokens: 65 }, { max_tokens: 0 }]) await assert.rejects(() => fetch('https://gateway.example/v1/chat/completions', { method: 'POST', body: JSON.stringify({ ...body, ...patch }) }), /SCOPE_REJECTED/);
});
test('MCP uses explicit method and tool allowlist before transport', async () => {
  const fetch = pinnedFetch(config, { ...opts, kind: 'mcp' });
  for (const rpc of [{ method: 'resources/read' }, { method: 'tools/call', params: { name: 'write_anything' } }, { method: 'sampling/createMessage' }]) await assert.rejects(() => fetch('https://gateway.example/mcp', { method: 'POST', body: JSON.stringify(rpc) }), /DENIED/);
});
