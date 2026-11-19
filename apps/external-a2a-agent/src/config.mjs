import { isIP } from 'node:net';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { keys, origin, readFile, parseStrict, keyring, secret, sha256, canonical, verifyJws, externalDirectory, identity, reject } from './contract.mjs';
export const SDK = Object.freeze({ framework: '@openai/agents-core', version: '0.20.0', provider: '@openai/agents-openai', providerVersion: '0.20.0', clientVersion: '7.38.0', mcpVersion: '2.3.1', zodVersion: '4.6.7' });
export function sourceFingerprint() { const dir = dirname(fileURLToPath(import.meta.url)); const hashes = Object.fromEntries(readdirSync(dir).filter(p => p.endsWith('.mjs')).sort().map(p => [p, sha256(readFileSync(resolve(dir, p)))])); const root = resolve(dir, '..'); for (const name of ['package.json', 'package-lock.json']) hashes[name] = existsSync(resolve(root, name)) ? sha256(readFileSync(resolve(root, name))) : null; return sha256(canonical({ files: hashes, sdk: SDK })); }
export function loadConfig(file) {
  const rawConfiguration = readFile(file); const c = parseStrict(rawConfiguration); keys(c, ['schema', 'origin', 'bind', 'port', 'agentName', 'stateDirectory', 'repository', 'tls', 'peerTokenFile', 'gatewayOrigin', 'gatewayKeysFile', 'cardKeyFile', 'cardKeyId', 'activationFile', 'activationKeysFile', 'model', 'limits'], ['schema', 'origin', 'bind', 'port', 'agentName', 'stateDirectory', 'repository', 'tls', 'peerTokenFile', 'gatewayOrigin', 'gatewayKeysFile', 'cardKeyFile', 'cardKeyId', 'activationFile', 'activationKeysFile', 'model', 'limits']);
  if (c.schema !== 'agentforge-external-a2a/v1' || !/^[a-zA-Z0-9_-]{1,64}$/.test(c.agentName) || !identity(c.cardKeyId) || !isIP(c.bind) || !Number.isInteger(c.port) || c.port < 1024 || c.port > 65535) reject(503, 'CONFIGURATION_INVALID');
  if (!(c.bind === '127.0.0.1' || c.bind === '::1' || c.bind.startsWith('10.') || c.bind.startsWith('192.168.') || /^172\.(1[6-9]|2\d|3[01])\./.test(c.bind) || /^f[cd]/i.test(c.bind))) reject(503, 'PRIVATE_LISTENER_REQUIRED');
  c.origin = origin(c.origin); c.gatewayOrigin = origin(c.gatewayOrigin); c.stateDirectory = externalDirectory(c.stateDirectory, c.repository);
  keys(c.tls, ['keyFile', 'certFile'], ['keyFile', 'certFile']);
  keys(c.model, ['baseURL', 'address', 'caFile', 'name', 'responseModel', 'credentials', 'mcp'], ['baseURL', 'address', 'caFile', 'name', 'credentials']);
  const url = new URL(c.model.baseURL); if (url.origin !== c.gatewayOrigin || url.pathname !== '/v1' || url.search || url.hash || url.username || url.password || !isIP(c.model.address) || !identity(c.model.name) || (c.model.responseModel != null && !identity(c.model.responseModel))) reject(503, 'GOVERNED_MODEL_ORIGIN_REQUIRED');
  const ipHost=url.hostname.replace(/^\[|\]$/g,'');if(isIP(ipHost)&&ipHost!==c.model.address)reject(503,'MODEL_IP_PIN_MISMATCH');
  if (c.model.mcp != null) { keys(c.model.mcp, ['path', 'allowedTools'], ['path', 'allowedTools']); if (c.model.mcp.path !== '/mcp' || !Array.isArray(c.model.mcp.allowedTools) || !c.model.mcp.allowedTools.length || c.model.mcp.allowedTools.length > 16 || c.model.mcp.allowedTools.some(t => !/^[A-Za-z0-9_-]{1,128}$/.test(t))) reject(503, 'MCP_PROFILE_INVALID'); }
  if (!Array.isArray(c.model.credentials) || !c.model.credentials.length || c.model.credentials.length > 32) reject(503, 'MODEL_CREDENTIAL_BINDINGS_INVALID');
  const credentials = new Map(); for (const entry of c.model.credentials) { keys(entry, ['tenantId', 'userId', 'tokenFile', 'mcpTokenFile'], ['tenantId', 'userId', 'tokenFile']); const key = `${entry.tenantId}\0${entry.userId}`; if (!identity(entry.tenantId) || !identity(entry.userId) || credentials.has(key)) reject(503, 'MODEL_CREDENTIAL_BINDINGS_INVALID'); credentials.set(key, { token: secret(entry.tokenFile), mcpToken: c.model.mcp ? secret(entry.mcpTokenFile) : null, tenantId: entry.tenantId, userId: entry.userId }); }
  keys(c.limits, ['maxConcurrentTasks', 'maxTasksPerActorPerDay', 'maxModelCalls', 'maxTurns', 'maxOutputTokens', 'maxRunMs'], ['maxConcurrentTasks', 'maxTasksPerActorPerDay', 'maxModelCalls', 'maxTurns', 'maxOutputTokens', 'maxRunMs']);
  for (const [key, low, high] of [['maxConcurrentTasks', 1, 8], ['maxTasksPerActorPerDay', 1, 1000], ['maxModelCalls', 1, 8], ['maxTurns', 1, 8], ['maxOutputTokens', 16, 2048], ['maxRunMs', 1000, 120000]]) if (!Number.isSafeInteger(c.limits[key]) || c.limits[key] < low || c.limits[key] > high) reject(503, 'EXECUTION_LIMIT_INVALID');
  const configSha = sha256(rawConfiguration);
  const peerToken = secret(c.peerTokenFile); const tlsMaterial = { key: readFile(c.tls.keyFile), cert: readFile(c.tls.certFile, { privateFile: false }) }; const modelCA = readFile(c.model.caFile, { privateFile: false });
  const credentialSha = sha256(canonical({ principals: [...credentials].map(([owner, x]) => ({ owner, tokenSha256: sha256(x.token), mcpTokenSha256: x.mcpToken ? sha256(x.mcpToken) : null })), material: { peerToken: sha256(peerToken), serverKey: sha256(tlsMaterial.key), serverCert: sha256(tlsMaterial.cert), modelCA: sha256(modelCA), cardKey: sha256(readFile(c.cardKeyFile)), gatewayTrust: sha256(readFile(c.gatewayKeysFile,{privateFile:false})), operatorTrust: sha256(readFile(c.activationKeysFile,{privateFile:false})) } }));
  const gatewayKeys = keyring(c.gatewayKeysFile); const activationKeys = keyring(c.activationKeysFile);
  for (const ring of [gatewayKeys, activationKeys]) for (const [kid, key] of ring) if (kid.startsWith('UNIT_TEST_ONLY') || key.jwk.x === 'Zs1gi5KLiOUODv6qM_rxxDzv4HKUsLh-n-CrpqPPdjM') reject(503, 'TEST_TRUST_KEY_REJECTED');
  return { ...c, configSha, credentialSha, credentials, peerToken, gatewayKeys, activationKeys, tlsMaterial, modelCA };
}
export function activation(config, cardSha, now = Math.floor(Date.now() / 1000)) {
  let raw; try { raw = readFile(config.activationFile, { maximum: 65536 }).toString('utf8').trim(); } catch { reject(503, 'U_GW15_ACTIVATION_GATE_CLOSED'); }
  if (!existsSync(resolve(dirname(fileURLToPath(import.meta.url)), '../package-lock.json'))) reject(503, 'FRAMEWORK_LOCK_REQUIRED');
  return validateActivationClaims(verifyJws(raw, config.activationKeys, 'agentforge-external-a2a-activation+jwt'), config, cardSha, now);
}
export const privateIP = value => isIP(value) && (value === '::1' || value === '127.0.0.1' || value.startsWith('10.') || value.startsWith('192.168.') || /^172\.(1[6-9]|2\d|3[01])\./.test(value) || /^f[cd]/i.test(value));
export function validateActivationClaims(c, config, cardSha, now) {
  keys(c, ['schema', 'admission', 'rehearsal', 'u_gw15', 'configuration_sha256', 'credentials_sha256', 'source_sha256', 'card_sha256', 'gateway_origin', 'agent_origin', 'allow_model_calls', 'iat', 'exp'], ['schema', 'u_gw15', 'configuration_sha256', 'credentials_sha256', 'source_sha256', 'card_sha256', 'gateway_origin', 'agent_origin', 'allow_model_calls', 'iat', 'exp']);
  if (c.schema !== 'agentforge-external-a2a-activation/v1' ||  c.configuration_sha256 !== config.configSha || c.credentials_sha256 !== config.credentialSha || c.source_sha256 !== sourceFingerprint() || c.card_sha256 !== cardSha || c.gateway_origin !== config.gatewayOrigin || c.agent_origin !== config.origin || c.allow_model_calls !== true || !Number.isSafeInteger(c.iat) || !Number.isSafeInteger(c.exp) || c.iat > now + 5 || c.exp <= now || c.exp - c.iat > 3600 || c.exp <= c.iat) reject(503, 'U_GW15_ACTIVATION_GATE_CLOSED');
  if (c.admission === 'rehearsal') {
    const r = c.rehearsal; keys(r, ['tenant_id', 'user_id', 'profile_sha256', 'address'], ['tenant_id', 'user_id', 'profile_sha256', 'address']);
    const host = raw => new URL(raw).hostname.replace(/^\[|\]$/g, '');
    if (!identity(r.tenant_id) || !identity(r.user_id) || r.profile_sha256 !== sha256(canonical(config.model)) || r.address !== config.model.address || !privateIP(r.address) || !privateIP(host(config.origin)) || !privateIP(host(config.gatewayOrigin)) || c.exp - c.iat > 900 || c.u_gw15?.status !== 'not_run') reject(503, 'REHEARSAL_SCOPE_REJECTED');
  } else if (c.admission != null && c.admission !== 'production' || c.rehearsal != null || c.u_gw15?.status !== 'direct_verified' || !/^[0-9a-f]{64}$/.test(c.u_gw15.evidence_sha256 ?? '')) reject(503, 'U_GW15_ACTIVATION_GATE_CLOSED');
  return c;
}
