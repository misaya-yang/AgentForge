import { createHash, createPublicKey, createPrivateKey, sign, verify, timingSafeEqual } from 'node:crypto';
import { lstatSync, openSync, readFileSync, closeSync, fstatSync, realpathSync } from 'node:fs';
import { isAbsolute, resolve, dirname } from 'node:path';
export class Rejection extends Error { constructor(status, code) { super(code); this.status = status; this.code = code; } }
export const reject = (status, code) => { throw new Rejection(status, code); };
export const sha256 = value => createHash('sha256').update(value).digest('hex');
export const uuid = value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);
export const identity = value => typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= 255 && !/[\x00-\x20\x7f]/.test(value);
export function keys(value, allowed, required = []) {
  if (!value || Array.isArray(value) || typeof value !== 'object' || Object.keys(value).some(k => !allowed.includes(k)) || required.some(k => !(k in value))) reject(400, 'INVALID_ARGUMENT');
}
function scalar(value) {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) { const next = value.charCodeAt(++i); if (!(next >= 0xdc00 && next <= 0xdfff)) reject(400, 'INVALID_UNICODE'); }
    else if (code >= 0xdc00 && code <= 0xdfff) reject(400, 'INVALID_UNICODE');
  }
  return value;
}
// RFC 8785: ECMAScript number/string serialization and UTF-16 property order.
export function canonical(value) {
  if (value === null || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number') { if (!Number.isFinite(value)) reject(400, 'INVALID_NUMBER'); return JSON.stringify(value); }
  if (typeof value === 'string') return JSON.stringify(scalar(value));
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && Object.getPrototypeOf(value) === Object.prototype) return `{${Object.keys(value).sort().map(k => `${canonical(k)}:${canonical(value[k])}`).join(',')}}`;
  reject(400, 'INVALID_JSON_VALUE');
}
// Reject duplicate keys before verification/deserialization; JSON.parse alone cannot.
export function parseStrict(bytes, maximum = 262144) {
  if (Buffer.byteLength(bytes) > maximum) reject(413, 'REQUEST_TOO_LARGE');
  let source; try { source = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { reject(400, 'INVALID_UTF8'); } let at = 0; let nodes = 0;
  const ws = () => { while (/^[\x20\t\r\n]$/.test(source[at] ?? '')) at++; };
  function string() { const start = at++; let escaped = false; while (at < source.length) { const c = source[at++]; if (!escaped && c === '"') { try { return scalar(JSON.parse(source.slice(start, at))); } catch { reject(400, 'INVALID_JSON'); } } if (!escaped && c === '\\') escaped = true; else escaped = false; } reject(400, 'INVALID_JSON'); }
  function value(depth = 0) {
    if (++nodes > 100000 || depth > 32) reject(400, 'JSON_LIMIT'); ws(); const c = source[at];
    if (c === '"') return string();
    if (c === '{') { at++; const result = {}; const seen = new Set(); ws(); if (source[at] === '}') { at++; return result; } while (true) { ws(); if (source[at] !== '"') reject(400, 'INVALID_JSON'); const k = string(); if (seen.has(k)) reject(400, 'DUPLICATE_JSON_KEY'); seen.add(k); ws(); if (source[at++] !== ':') reject(400, 'INVALID_JSON'); Object.defineProperty(result, k, { value: value(depth + 1), enumerable: true, configurable: true, writable: true }); ws(); const next = source[at++]; if (next === '}') return result; if (next !== ',') reject(400, 'INVALID_JSON'); } }
    if (c === '[') { at++; const result = []; ws(); if (source[at] === ']') { at++; return result; } while (true) { result.push(value(depth + 1)); ws(); const next = source[at++]; if (next === ']') return result; if (next !== ',') reject(400, 'INVALID_JSON'); } }
    for (const [literal, result] of [['true', true], ['false', false], ['null', null]]) if (source.startsWith(literal, at)) { at += literal.length; return result; }
    const number = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(source.slice(at)); if (number) { at += number[0].length; const n = Number(number[0]); if (Number.isFinite(n)) return n; }
    reject(400, 'INVALID_JSON');
  }
  const result = value(); ws(); if (at !== source.length) reject(400, 'INVALID_JSON'); return result;
}
export function readFile(path, { privateFile = true, maximum = 1048576 } = {}) {
  if (typeof path !== 'string' || !isAbsolute(path)) reject(503, 'ABSOLUTE_FILE_REQUIRED');
  const before = lstatSync(path); if (!before.isFile() || before.isSymbolicLink() || before.size > maximum || (before.mode & (privateFile ? 0o077 : 0o022))) reject(503, 'FILE_POLICY_REJECTED');
  const fd = openSync(path, 'r'); try { const actual = fstatSync(fd); if (before.ino !== actual.ino || before.dev !== actual.dev) reject(503, 'FILE_CHANGED'); const raw = readFileSync(fd); if (raw.length > maximum) reject(503, 'FILE_TOO_LARGE'); return raw; } finally { closeSync(fd); }
}
export function secret(path) { const value = readFile(path, { maximum: 65536 }).toString('utf8').replace(/\r?\n$/, ''); if (value.length < 32 || /[^\x21-\x7e]/.test(value)) reject(503, 'SECRET_POLICY_REJECTED'); return value; }
export function origin(raw) { let url; try { url = new URL(raw); } catch { reject(503, 'ORIGIN_INVALID'); } if (url.protocol !== 'https:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash || raw.includes('\\') || raw.trim() !== raw) reject(503, 'EXACT_HTTPS_ORIGIN_REQUIRED'); return url.origin; }
export function externalDirectory(path, repository) { if (!isAbsolute(path)) reject(503, 'ABSOLUTE_STATE_REQUIRED'); let parent = path; const tail = []; while (true) { try { parent = realpathSync(parent); break; } catch { const next = dirname(parent); if (next === parent) reject(503, 'STATE_PARENT_INVALID'); tail.unshift(parent.slice(next.length + 1)); parent = next; } } const actual = resolve(parent, ...tail); const repo = realpathSync(repository); if (actual === repo || actual.startsWith(`${repo}/`)) reject(503, 'EXTERNAL_PRIVATE_STATE_REQUIRED'); return actual; }
export function decode64(raw) { if (typeof raw !== 'string' || !/^[A-Za-z0-9_-]+$/.test(raw)) reject(401, 'INVALID_PROOF'); const bytes = Buffer.from(raw, 'base64url'); if (bytes.toString('base64url') !== raw) reject(401, 'INVALID_PROOF'); return bytes; }
export function keyring(file) { const doc = parseStrict(readFile(file, { privateFile: false })); keys(doc, ['keys'], ['keys']); if (!Array.isArray(doc.keys) || !doc.keys.length || doc.keys.length > 32) reject(503, 'TRUST_KEYS_INVALID'); const result = new Map(); for (const k of doc.keys) { keys(k, ['kid', 'kty', 'crv', 'x', 'alg', 'use'], ['kid', 'kty', 'crv', 'x']); if (!identity(k.kid) || k.kty !== 'OKP' || k.crv !== 'Ed25519' || decode64(k.x).length !== 32 || result.has(k.kid) || (k.alg && k.alg !== 'EdDSA') || (k.use && k.use !== 'sig')) reject(503, 'TRUST_KEYS_INVALID'); result.set(k.kid, { jwk: k, key: createPublicKey({ key: k, format: 'jwk' }) }); } return result; }
export function verifyJws(raw, trust, typ) {
  if (typeof raw !== 'string' || raw.length > 16384) reject(401, 'INVALID_PROOF'); const parts = raw.split('.'); if (parts.length !== 3) reject(401, 'INVALID_PROOF');
  const header = parseStrict(decode64(parts[0])); keys(header, ['alg', 'typ', 'kid'], ['alg', 'typ', 'kid']); const k = trust.get(header.kid); if (header.alg !== 'EdDSA' || header.typ !== typ || !k || !verify(null, Buffer.from(`${parts[0]}.${parts[1]}`), k.key, decode64(parts[2]))) reject(401, 'INVALID_PROOF'); return parseStrict(decode64(parts[1]));
}
export function verifyTask(raw, { trust, issuer, audience, agentName, method, path, body, now = Math.floor(Date.now() / 1000) }) {
  const c = verifyJws(raw, trust, 'agentforge-a2a-task+jwt'); keys(c, ['v', 'iss', 'aud', 'tenant_id', 'user_id', 'agent_name', 'message_id', 'body_sha256', 'method', 'path', 'iat', 'exp', 'jti'], ['v', 'iss', 'aud', 'tenant_id', 'user_id', 'agent_name', 'message_id', 'body_sha256', 'method', 'path', 'iat', 'exp', 'jti']);
  if (c.v !== 'agentforge-a2a-task/v1' || c.iss !== issuer || c.aud !== audience || c.agent_name !== agentName || c.method !== method || c.path !== path || c.body_sha256 !== sha256(body) || !identity(c.tenant_id) || !identity(c.user_id) || !uuid(c.message_id) || !uuid(c.jti) || !Number.isSafeInteger(c.iat) || !Number.isSafeInteger(c.exp) || c.iat > now + 5 || c.exp <= now || c.exp <= c.iat || c.exp - c.iat > 120) reject(401, 'TASK_AUTHORITY_REJECTED'); return c;
}
export function bearer(raw, expected) { if (typeof raw !== 'string' || !raw.startsWith('Bearer ')) reject(401, 'AUTHENTICATION_REQUIRED'); const actual = Buffer.from(raw.slice(7)); const desired = Buffer.from(expected); if (actual.length !== desired.length || !timingSafeEqual(actual, desired)) reject(401, 'AUTHENTICATION_REQUIRED'); }
export function request(value) {
  keys(value, ['message', 'configuration'], ['message']); keys(value.message, ['messageId', 'contextId', 'taskId', 'role', 'parts'], ['messageId', 'role', 'parts']); const m = value.message; const c = value.configuration ?? {};
  keys(c, ['returnImmediately', 'acceptedOutputModes', 'historyLength']); if (!uuid(m.messageId) || m.role !== 'ROLE_USER' || m.taskId != null || (m.contextId != null && !uuid(m.contextId)) || !Array.isArray(m.parts) || !m.parts.length || m.parts.length > 32 || (c.returnImmediately != null && typeof c.returnImmediately !== 'boolean') || (c.historyLength != null && (!Number.isInteger(c.historyLength) || c.historyLength < 0 || c.historyLength > 100)) || (c.acceptedOutputModes != null && (!Array.isArray(c.acceptedOutputModes) || c.acceptedOutputModes.some(v => v !== 'text/plain')))) reject(400, 'INVALID_ARGUMENT');
  const text = m.parts.map(p => { keys(p, ['text', 'mediaType'], ['text']); if (typeof p.text !== 'string' || (p.mediaType != null && p.mediaType !== 'text/plain')) reject(400, 'CONTENT_TYPE_NOT_SUPPORTED'); return p.text; }).join('\n'); if (!text.trim() || Buffer.byteLength(text) > 65536) reject(400, 'INVALID_ARGUMENT'); return { value, text, messageId: m.messageId, contextId: m.contextId ?? null, immediate: c.returnImmediately ?? false };
}
export function signCard(payload, keyFile, kid) { const key = createPrivateKey(readFile(keyFile)); if (key.asymmetricKeyType !== 'ed25519') reject(503, 'CARD_KEY_INVALID'); const jwk = createPublicKey(key).export({ format: 'jwk' }); const protected64 = Buffer.from(canonical({ alg: 'EdDSA', kid, jwk })).toString('base64url'); const input = `${protected64}.${Buffer.from(canonical(payload)).toString('base64url')}`; return { ...payload, signatures: [{ protected: protected64, signature: sign(null, Buffer.from(input), key).toString('base64url') }] }; }
export function verifyCard(card, pins, expectedOrigin) { keys(card.signatures?.[0], ['protected', 'signature'], ['protected', 'signature']); if (card.signatures.length !== 1) reject(401, 'CARD_SIGNATURE_INVALID'); const s = card.signatures[0]; const h = parseStrict(decode64(s.protected)); keys(h, ['alg', 'kid', 'jwk'], ['alg', 'kid']); const trusted = pins.get(h.kid); if (!trusted || h.alg !== 'EdDSA' || (h.jwk && (h.jwk.kty !== 'OKP' || h.jwk.crv !== 'Ed25519' || h.jwk.x !== trusted.jwk.x))) reject(401, 'CARD_SIGNATURE_INVALID'); const payload = { ...card }; delete payload.signatures; if (!verify(null, Buffer.from(`${s.protected}.${Buffer.from(canonical(payload)).toString('base64url')}`), trusted.key, decode64(s.signature))) reject(401, 'CARD_SIGNATURE_INVALID'); if (!Array.isArray(card.supportedInterfaces) || card.supportedInterfaces.length !== 1 || card.supportedInterfaces[0].url !== `${expectedOrigin}/a2a` || card.supportedInterfaces[0].protocolBinding !== 'HTTP+JSON' || card.supportedInterfaces[0].protocolVersion !== '1.0') reject(401, 'CARD_INTERFACE_MISMATCH'); return payload; }
