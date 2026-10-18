import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, createPublicKey, createPrivateKey, sign, randomUUID } from 'node:crypto';
import { mkdtempSync, writeFileSync, chmodSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { canonical, parseStrict, sha256, verifyTask, verifyCard, signCard, request, externalDirectory, keyring } from '../src/contract.mjs';
import { Store } from '../src/store.mjs';
import { createAdapter } from '../src/adapter.mjs';
import { activation, validateActivationClaims, sourceFingerprint } from '../src/config.mjs';
import { validatedHeaders } from '../src/server.mjs';
const wire = JSON.parse(readFileSync(new URL('../../../rust/platform-native/crates/platform-gateway/tests/fixtures/protocols/a2a-task-authority-v1.json', import.meta.url)));
const testPrivate = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), Buffer.from(wire.seed_hex, 'hex')]), format: 'der', type: 'pkcs8' });
const publicJwk = createPublicKey(testPrivate).export({ format: 'jwk' });
const trust = new Map([[wire.header.kid, { key: createPublicKey(testPrivate), jwk: publicJwk }]]);
const proof = (claims, header = wire.header, key = testPrivate) => { const input = `${Buffer.from(canonical(header)).toString('base64url')}.${Buffer.from(canonical(claims)).toString('base64url')}`; return `${input}.${sign(null, Buffer.from(input), key).toString('base64url')}`; };
const options = { trust, issuer: wire.claims.iss, audience: wire.claims.aud, agentName: wire.claims.agent_name, method: 'POST', path: wire.claims.path, body: Buffer.from(wire.raw_body), now: wire.expected_now };
const limits = { maxConcurrentTasks: 2, maxTasksPerActorPerDay: 10, maxModelCalls: 2, maxTurns: 4, maxOutputTokens: 128, maxRunMs: 2000 };
const owner = { ...wire.claims };
const cleanups = new WeakMap();
function cleanup(t, fn) { if (!cleanups.has(t)) { cleanups.set(t, []); t.after(async () => { for (const f of cleanups.get(t).reverse()) await f(); }); } cleanups.get(t).push(fn); }
function temp(t) { const directory = mkdtempSync(join(tmpdir(), 'af-a2a-')); chmodSync(directory, 0o700); cleanup(t, () => rmSync(directory, { recursive: true, force: true })); return directory; }
function store(t) { const result = new Store(join(temp(t), 'state'), { now: () => wire.expected_now * 1000 }); cleanup(t, () => result.close()); return result; }
const input = () => request(parseStrict(Buffer.from(wire.raw_body)));
test('shared native Gateway fixture is byte-identical Node Ed25519/JCS v1 proof', () => {
  assert.equal(canonical(JSON.parse(wire.raw_body)), wire.raw_body);
  assert.equal(sha256(wire.raw_body), wire.claims.body_sha256);
  assert.equal(proof(wire.claims), wire.proof);
  assert.deepEqual(verifyTask(wire.proof, options), wire.claims);
});
test('signature, body, issuer, audience, method, path, clock and self-JWK reject', () => {
  for (const patch of [{ body: Buffer.from(`${wire.raw_body} `) }, { issuer: 'https://evil.example' }, { audience: 'https://evil.example' }, { method: 'GET' }, { path: '/a2a/tasks/foo' }, { now: wire.claims.exp }, { now: wire.claims.iat - 6 }]) assert.throws(() => verifyTask(wire.proof, { ...options, ...patch }));
  for (const patch of [{ tenant_id: '' }, { user_id: 'bad actor' }, { message_id: 'not-uuid' }, { exp: owner.iat + 121 }, { jti: 'not-uuid' }]) assert.throws(() => verifyTask(proof({ ...owner, ...patch }), options));
  assert.throws(() => verifyTask(proof(owner, { ...wire.header, jwk: publicJwk }), options));
  assert.throws(() => verifyTask(proof(owner, wire.header, generateKeyPairSync('ed25519').privateKey), options));
});
test('bounded strict JSON and RFC8785 edge cases', () => {
  for (const raw of ['{"x":1,"x":2}', '{"x":1,"\\u0078":2}', '[1,]', '{"x":1,}', '1e999', '01', 'true false', '"\\ud800"', '"\\udfff"', '{"x":undefined}']) assert.throws(() => parseStrict(Buffer.from(raw)));
  assert.throws(() => parseStrict(Buffer.from([0xff])));
  assert.throws(() => parseStrict(Buffer.from(' '.repeat(20)), 10));
  assert.equal(canonical(parseStrict(Buffer.from('{"z":-0,"a":1e30,"é":"\\\\\\\"𝄞"}'))), '{"a":1e+30,"z":0,"é":"\\\\\\\"𝄞"}');
  assert.equal(canonical(parseStrict(Buffer.from('{"__proto__":{"polluted":true}}'))), '{"__proto__":{"polluted":true}}');
  assert.equal({}.polluted, undefined);
  assert.throws(() => request({ message: { messageId: owner.message_id, role: 'user', parts: [{ kind: 'text', text: 'legacy' }] } }));
  assert.throws(() => request({ message: { messageId: owner.message_id, role: 'ROLE_USER', parts: [{ text: 'x' }], taskId: randomUUID() } }));
  assert.throws(() => validatedHeaders(['Host', 'peer.example', 'host', 'evil.example']));
});
test('signed Agent Card binds explicit public key and exact HTTP+JSON interface', t => {
  const directory = temp(t); const key = join(directory, 'card.pem'); writeFileSync(key, testPrivate.export({ format: 'pem', type: 'pkcs8' }), { mode: 0o600 });
  const card = signCard({ name: 'fixture', supportedInterfaces: [{ url: `${owner.aud}/a2a`, protocolBinding: 'HTTP+JSON', protocolVersion: '1.0' }] }, key, wire.header.kid);
  assert.equal(verifyCard(card, trust, owner.aud).name, 'fixture');
  assert.throws(() => verifyCard({ ...card, name: 'changed' }, trust, owner.aud));
  assert.throws(() => verifyCard(card, trust, owner.iss));
  assert.throws(() => verifyCard(card, new Map(), owner.aud));
  assert.throws(() => externalDirectory(resolve('apps/state'), resolve('.')));
  const jwks = join(directory, 'jwks.json'); writeFileSync(jwks, JSON.stringify({ keys: [{ ...publicJwk, kid: wire.header.kid }] }), { mode: 0o600 }); assert.equal(keyring(jwks).size, 1);
});
test('durable idempotency, owner isolation, nonce replay, context and budgets', t => {
  const s = store(t); s.nonce(owner); assert.throws(() => s.nonce(owner), /REPLAYED/);
  const first = s.admit(owner, input(), 'hash', limits); assert.equal(first.fresh, true); assert.equal(s.admit(owner, input(), 'hash', limits).row.id, first.row.id);
  assert.throws(() => s.admit(owner, input(), 'different', limits), /CONFLICT/);
  for (const patch of [{ tenant_id: 'other' }, { user_id: 'other' }, { agent_name: 'other' }, { message_id: randomUUID() }]) { assert.throws(() => s.row({ ...owner, ...patch }, first.row.id), /NOT_FOUND/); assert.throws(() => s.byMessage({ ...owner, ...patch }), /NOT_FOUND/); }
  const secondOwner = { ...owner, message_id: randomUUID() }; const secondRequest = { ...input(), messageId: secondOwner.message_id, contextId: first.row.context };
  assert.throws(() => s.admit(secondOwner, secondRequest, 'hash', limits), /BUSY/);
  assert.throws(() => s.admit({ ...secondOwner, user_id: 'other' }, secondRequest, 'hash', limits), /CONTEXT_NOT_FOUND/);
  assert.throws(() => s.admit(secondOwner, { ...secondRequest, contextId: null }, 'hash', { ...limits, maxTasksPerActorPerDay: 1 }), /BUDGET_EXHAUSTED/);
});
test('unknown model effect never becomes successful and per-task dispatch cap is durable', t => {
  const s = store(t); const { row } = s.admit(owner, input(), 'hash', limits); assert.equal(s.working(owner, row.id), true);
  const first = s.dispatch(owner, row.id, 'bodyhash', 2); s.response(row.id, first.sequence, { effect: 'confirmed', responseId: 'response1', inputTokens: 4, outputTokens: 2, status: 200 });
  s.dispatch(owner, row.id, 'bodyhash2', 2); assert.throws(() => s.dispatch(owner, row.id, 'bodyhash3', 2), /BUDGET_EXHAUSTED/);
  s.finish(owner, row.id, 'TASK_STATE_COMPLETED', { output: 'must not escape' }); const task = s.describe(owner, row.id); assert.equal(task.status.state, 'TASK_STATE_UNSPECIFIED'); assert.equal(task.metadata.effectState, 'unknown'); assert.equal(task.artifacts, undefined);
});
test('restart preserves unknown instead of replaying; nonce replay survives restart', t => {
  const directory = join(temp(t), 'state'); let s = new Store(directory, { now: () => wire.expected_now * 1000 }); const { row } = s.admit(owner, input(), 'hash', limits); s.nonce(owner); s.working(owner, row.id); s.close(); s = new Store(directory, { now: () => wire.expected_now * 1000 }); cleanup(t, () => s.close());
  assert.equal(s.describe(owner, row.id).status.state, 'TASK_STATE_UNSPECIFIED'); assert.equal(s.admit(owner, input(), 'hash', limits).fresh, false); assert.throws(() => s.nonce(owner), /REPLAYED/); assert.equal(s.working(owner, row.id), false);
});
function adapter(t, run, gate = () => {}) {
  const directory = temp(t); const key = join(directory, 'card.pem'); writeFileSync(key, testPrivate.export({ format: 'pem', type: 'pkcs8' }), { mode: 0o600 });
  const config = { origin: owner.aud, gatewayOrigin: owner.iss, agentName: owner.agent_name, gatewayKeys: trust, cardKeyFile: key, cardKeyId: wire.header.kid, peerToken: 'test-peer-token-'.repeat(4), credentials: new Map([[`${owner.tenant_id}\0${owner.user_id}`, {}]]), activationFile: join(directory, 'does-not-exist'), limits };
  const s = store(t); const a = createAdapter(config, s, { run, gate, now: () => wire.expected_now * 1000 }); cleanup(t, () => a.shutdown());
  return { ...a, s, config, call: async (method, path, body = Buffer.alloc(0), patch = {}, extra = {}) => { const claims = { ...owner, jti: randomUUID(), method, path, body_sha256: sha256(body), ...patch }; return a.handle({ method, path, body, headers: { host: new URL(config.origin).host, authorization: `Bearer ${config.peerToken}`, 'a2a-version': '1.0', 'content-type': 'application/json', 'x-agentforge-a2a-task': proof(claims), ...extra } }); } };
}
test('stock activation gate closes before framework dispatch and durable task admission', async t => {
  let calls = 0; const a = adapter(t, async () => { calls++; }, activation); const r = await a.call('POST', '/a2a/message:send', Buffer.from(wire.raw_body)); assert.equal(r.status, 503); assert.equal(r.body.error.code, 'U_GW15_ACTIVATION_GATE_CLOSED'); assert.equal(calls, 0); assert.throws(() => a.s.byMessage(owner));
});
test('handler idempotency/conflict and read-only reconcile preserve signed owner/message binding', async t => {
  let calls = 0; const a = adapter(t, async () => { calls++; return { output: 'offline fixture only', framework: 'test-fake', frameworkCalls: 1, toolCalls: 1 }; });
  const raw = Buffer.from(wire.raw_body); const first = await a.call('POST', '/a2a/message:send', raw); await new Promise(resolve => setImmediate(resolve)); const id = first.body.task.id;
  assert.equal((await a.call('POST', '/a2a/message:send', raw)).body.task.id, id); assert.equal(calls, 1);
  const changed = JSON.parse(wire.raw_body); changed.message.parts[0].text = 'different'; assert.equal((await a.call('POST', '/a2a/message:send', Buffer.from(canonical(changed)))).status, 409);
  assert.equal((await a.call('GET', `/a2a/tasks:byMessage/${owner.message_id}`)).body.id, id);
  assert.equal((await a.call('GET', `/a2a/tasks/${id}`, undefined, { tenant_id: 'other' })).status, 404);
  assert.equal((await a.call('GET', `/a2a/tasks/${id}`, undefined, { user_id: 'other' })).status, 404);
  assert.equal((await a.call('GET', `/a2a/tasks/${id}`, undefined, { message_id: randomUUID() })).status, 404);
  assert.equal((await a.call('POST', `/a2a/tasks/${id}:cancel`, Buffer.from('{}'))).status, 409);
  assert.equal((await a.call('POST', '/a2a/message:send', raw, {}, { 'x-tenant-id': owner.tenant_id })).status, 400);
});
test('cancel persists intent, aborts framework, waits for stop and never claims external undo', async t => {
  let entered; const started = new Promise(resolve => { entered = resolve; }); const a = adapter(t, async ({ signal, store, taskId, owner }) => { store.dispatch(owner, taskId, 'unknown-network-effect', 2); entered(); await new Promise((_, reject) => signal.addEventListener('abort', () => reject(new Error('abort')), { once: true })); });
  const r = await a.call('POST', '/a2a/message:send', Buffer.from(wire.raw_body)); await started;
  const canceled = await a.call('POST', `/a2a/tasks/${r.body.task.id}:cancel`, Buffer.from('{}')); assert.equal(canceled.status, 200); assert.equal(canceled.body.status.state, 'TASK_STATE_CANCELED'); assert.equal(canceled.body.metadata.effectState, 'unknown'); assert.equal(canceled.body.artifacts, undefined);
  assert.equal((await a.call('POST', `/a2a/tasks/${r.body.task.id}:cancel`, Buffer.from('{}'))).body.status.state, 'TASK_STATE_CANCELED');
});
test('native acceptance evidence fixture is explicitly unit-only, signed and hash bound', () => {
  const f = JSON.parse(readFileSync(new URL('../fixtures/native-evidence.test-only.json', import.meta.url))); assert.equal(f.test_only, true); assert.equal(f.payload.observationSha256, sha256(f.rawObservation));
  const header = { alg: 'EdDSA', typ: 'agentforge-external-a2a-evidence+jwt', kid: f.kid }; assert.equal(proof(f.payload, header), f.jws); assert.equal(f.publicKey, publicJwk.x);
});
test('one local process exclusively owns state and capabilities are bounded/unknown-safe', t => {
  const directory = join(temp(t), 'exclusive'); const s = new Store(directory); cleanup(t, () => s.close()); assert.throws(() => new Store(directory), /ALREADY_OWNED/);
  const { row } = s.admit(owner, input(), 'hash', limits); s.working(owner, row.id); const id = s.capability(owner, row.id, 'read_contract', 'hash', 1); assert.throws(() => s.capability(owner, row.id, 'read_contract', 'hash', 1), /BUDGET_EXHAUSTED/); assert.equal(s.unknownModelEffects(row.id), true); s.capabilityResponse(row.id, id, 'confirmed', 200); assert.equal(s.successfulCapabilities(row.id), 1); assert.equal(s.unknownModelEffects(row.id), false);
});

test('activation binds real gate status, artifact, source, credentials, card, origins and short expiry', () => {
  const config = { configSha: 'config-hash', credentialSha: 'credential-hash', gatewayOrigin: owner.iss, origin: owner.aud };
  const claims = { schema: 'agentforge-external-a2a-activation/v1', u_gw15: { status: 'direct_verified', evidence_sha256: 'a'.repeat(64) }, configuration_sha256: config.configSha, credentials_sha256: config.credentialSha, source_sha256: sourceFingerprint(), card_sha256: 'card-hash', gateway_origin: owner.iss, agent_origin: owner.aud, allow_model_calls: true, iat: wire.expected_now, exp: wire.expected_now + 120 };
  assert.equal(validateActivationClaims(claims, config, 'card-hash', wire.expected_now), claims);
  for (const patch of [{ u_gw15: { status: 'fixture_passed', evidence_sha256: 'a'.repeat(64) } }, { configuration_sha256: 'different' }, { credentials_sha256: 'different' }, { source_sha256: 'different' }, { card_sha256: 'different' }, { gateway_origin: 'https://other.example' }, { agent_origin: 'https://other.example' }, { allow_model_calls: false }, { exp: wire.expected_now }, { exp: wire.expected_now + 3601 }, { iat: wire.expected_now + 6 }]) assert.throws(() => validateActivationClaims({ ...claims, ...patch }, config, 'card-hash', wire.expected_now), /GATE_CLOSED/);
});
test('rehearsal signed owner scope rejects foreign principals before durable admission',async t=>{
 let runs=0;const permit={admission:'rehearsal',rehearsal:{tenant_id:owner.tenant_id,user_id:owner.user_id}};const a=adapter(t,async()=>{runs++;return{output:'unit-only',framework:'test-fake',frameworkCalls:1,toolCalls:1};},()=>permit);
 const denied=await a.call('POST','/a2a/message:send',Buffer.from(wire.raw_body),{user_id:'other-actor'});assert.equal(denied.status,403);assert.equal(denied.body.error.code,'REHEARSAL_OWNER_REJECTED');assert.equal(runs,0);
 const admitted=await a.call('POST','/a2a/message:send',Buffer.from(wire.raw_body));assert.equal(admitted.body.task.metadata.admission,'rehearsal');assert.match(admitted.body.task.metadata.profileSha256,/^[a-f0-9]{64}$/);
});

test('native container interop fixture is byte-identical to the independent Node corpus',()=>{
 const native=readFileSync(new URL('../../../rust/platform-native/crates/platform-tools/tests/fixtures/external-a2a/native-evidence.test-only.json',import.meta.url));
 const node=readFileSync(new URL('../fixtures/native-evidence.test-only.json',import.meta.url));assert.deepEqual(native,node);
});
