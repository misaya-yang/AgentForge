import { canonical, sha256, signCard, bearer, verifyTask, parseStrict, request, keys, uuid, Rejection, reject } from './contract.mjs';
import { activation } from './config.mjs';
import { runFramework } from './framework.mjs';
import { TERMINAL } from './store.mjs';
export const AUTHORITY = 'https://agentforge.dev/a2a/extensions/task-authority/v1';
export const RECONCILE = 'https://agentforge.dev/a2a/extensions/task-by-message/v1';
export function agentCard(config) {
  return signCard({ name: config.agentName, description: 'Customer-owned OpenAI Agents SDK reference agent. Governed Gateway credentials and pinned transport; activation requires U-GW-15 direct verification.', version: '0.1.0', supportedInterfaces: [{ url: `${config.origin}/a2a`, protocolBinding: 'HTTP+JSON', protocolVersion: '1.0' }], capabilities: { streaming: false, pushNotifications: false, extendedAgentCard: false, extensions: [{ uri: AUTHORITY, required: true }, { uri: RECONCILE, required: false }] }, securitySchemes: { gateway: { httpAuthSecurityScheme: { scheme: 'Bearer' } } }, securityRequirements: [{ schemes: { gateway: { list: [] } } }], defaultInputModes: ['text/plain'], defaultOutputModes: ['text/plain'], skills: [{ id: 'governed-reference', name: 'Governed reference answer', description: 'SDK-owned model and read-only tool iteration', tags: ['external-framework', 'activation-required'] }] }, config.cardKeyFile, config.cardKeyId);
}
const wait = (promise, ms) => new Promise(resolve => { const timer = setTimeout(() => resolve(false), ms); promise.then(() => { clearTimeout(timer); resolve(true); }, () => { clearTimeout(timer); resolve(true); }); });
// Pure HTTP adapter: no sockets or alternate model/tool iteration. Tests inject only the runner/gate.
export function createAdapter(config, store, { run = runFramework, gate = activation, now = () => Date.now() } = {}) {
  const card = agentCard(config); const cardSha = sha256(canonical(card)); const running = new Map(); let stopping = false;
  function checkOwner(permit, owner) { if (permit?.admission === 'rehearsal' && (permit.rehearsal.tenant_id !== owner.tenant_id || permit.rehearsal.user_id !== owner.user_id)) reject(403, 'REHEARSAL_OWNER_REJECTED'); }
  function start(owner, task, text) {
    const controller = new AbortController(); const entry = { controller, promise: null }; running.set(task.id, entry);
    entry.promise = Promise.resolve().then(async () => {
      if (!store.working(owner, task.id)) { store.finish(owner, task.id, 'TASK_STATE_CANCELED'); return; }
      const timer = setTimeout(() => controller.abort(), config.limits.maxRunMs);
      try { const permit = gate(config, cardSha, Math.floor(now() / 1000)); checkOwner(permit, owner); const result = await run({ config, owner, taskId: task.id, text, store, signal: controller.signal, authorize: () => { const current=gate(config,cardSha,Math.floor(now()/1000));checkOwner(current,owner); } }); if (controller.signal.aborted) reject(504, 'EXECUTION_DEADLINE_EXCEEDED'); store.finish(owner, task.id, 'TASK_STATE_COMPLETED', result); }
      catch (error) { store.finish(owner, task.id, store.unknownModelEffects(task.id) ? 'TASK_STATE_UNSPECIFIED' : 'TASK_STATE_FAILED', { error: error instanceof Rejection ? error.code : 'FRAMEWORK_EXECUTION_FAILED' }); }
      finally { clearTimeout(timer); running.delete(task.id); }
    });
    return entry;
  }
  async function handle({ method, path, headers = {}, body = Buffer.alloc(0) }) {
    try {
      if (headers.host !== new URL(config.origin).host || path.includes('?') || path.includes('%') || path.includes('#') || path.length > 512) reject(400, 'EXACT_REQUEST_TARGET_REQUIRED');
      if (method === 'GET' && path === '/a2a/.well-known/agent-card.json') { if (body.length) reject(400, 'BODY_NOT_ALLOWED'); return { status: 200, body: card }; }
      if (headers['a2a-version'] !== '1.0') reject(400, 'A2A_VERSION_UNSUPPORTED');
      bearer(headers.authorization, config.peerToken);
      if (headers['x-tenant-id'] || headers['x-user-id'] || headers['x-agentforge-tenant-id']) reject(400, 'UNSIGNED_ACTOR_HEADERS_REJECTED');
      const owner = verifyTask(headers['x-agentforge-a2a-task'], { trust: config.gatewayKeys, issuer: config.gatewayOrigin, audience: config.origin, agentName: config.agentName, method, path, body, now: Math.floor(now() / 1000) });
      store.nonce(owner);
      if (method === 'POST' && path === '/a2a/message:send') {
        if (!String(headers['content-type'] ?? '').split(';')[0].trim().match(/^application\/json$/i)) reject(415, 'JSON_REQUIRED');
        const parsed = request(parseStrict(body)); if (parsed.messageId !== owner.message_id) reject(403, 'MESSAGE_AUTHORITY_MISMATCH');
        if (stopping) reject(503, 'AGENT_STOPPING');
        const permit = gate(config, cardSha, Math.floor(now() / 1000)); checkOwner(permit, owner);
        if (!config.credentials.has(`${owner.tenant_id}\0${owner.user_id}`)) reject(403, 'PRINCIPAL_MODEL_CREDENTIAL_UNAVAILABLE');
        const { row, fresh } = store.admit(owner, parsed, sha256(canonical(parsed.value)), config.limits, permit?.admission === 'rehearsal' ? 'rehearsal' : 'production', sha256(canonical(config.model ?? {})), permit ? sha256(canonical(permit)) : null);
        const entry = fresh ? start(owner, row, parsed.text) : running.get(row.id);
        if (!parsed.immediate && entry && !await wait(entry.promise, Math.min(config.limits.maxRunMs + 1000, 110000))) return { status: 504, body: { error: { code: 'TASK_STILL_RUNNING' }, taskId: row.id, contextId: row.context } };
        return { status: 200, body: { task: store.describe(owner, row.id) } };
      }
      const byMessage = /^\/a2a\/tasks:byMessage\/([^/]+)$/.exec(path);
      if (method === 'GET' && byMessage) { if (body.length || !uuid(byMessage[1]) || byMessage[1] !== owner.message_id) reject(400, 'INVALID_ARGUMENT'); const row = store.byMessage(owner); return { status: 200, body: store.describe(owner, row.id) }; }
      const task = /^\/a2a\/tasks\/([^/:]+)(:cancel)?$/.exec(path);
      if (!task || !uuid(task[1])) reject(404, 'ROUTE_NOT_FOUND');
      if (!task[2] && method === 'GET') { if (body.length) reject(400, 'BODY_NOT_ALLOWED'); return { status: 200, body: store.describe(owner, task[1]) }; }
      if (task[2] && method === 'POST') {
        keys(parseStrict(body), []); const row = store.cancel(owner, task[1]); const entry = running.get(row.id);
        if (entry) { entry.controller.abort(); if (!await wait(entry.promise, 10000)) return { status: 504, body: { error: { code: 'CANCELLATION_UNCONFIRMED' }, taskId: row.id } }; }
        else if (!TERMINAL.has(row.state)) store.finish(owner, row.id, 'TASK_STATE_UNSPECIFIED', { error: 'EXECUTION_OWNER_UNAVAILABLE' });
        return { status: 200, body: store.describe(owner, row.id) };
      }
      reject(405, 'METHOD_NOT_ALLOWED');
    } catch (error) { return { status: error instanceof Rejection ? error.status : 500, body: { error: { code: error instanceof Rejection ? error.code : 'INTERNAL_ERROR' } } }; }
  }
  return { handle, card, cardSha, async shutdown() { stopping = true; for (const entry of running.values()) entry.controller.abort(); await Promise.all([...running.values()].map(x => x.promise)); } };
}
