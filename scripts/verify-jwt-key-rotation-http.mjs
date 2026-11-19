#!/usr/bin/env node
// Bounded HTTP check for a pre/post Gateway JWT key rotation. Both Gateways
// must share the intended test account database and Redis session store.
// Credentials and tokens remain in memory and are never printed.

import { randomBytes } from 'node:crypto';

const PRIVATE_JWK_FIELDS = new Set(['d', 'p', 'q', 'dp', 'dq', 'qi', 'oth', 'k']);
const MAX_RESPONSE_BYTES = 1_048_576;

function setting(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`missing required environment setting: ${name}`);
  return value;
}

function checkedOrigin(value, name) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`invalid ${name}`);
  }
  if (
    !['http:', 'https:'].includes(url.protocol) ||
    !url.hostname ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    url.pathname !== '/'
  ) {
    throw new Error(`invalid ${name}`);
  }
  return url.origin;
}

async function readBounded(response) {
  if (!response.body) return null;
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_RESPONSE_BYTES) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString('utf8');
}

async function requestJson(origin, method, path, { body, token } = {}) {
  const headers = { accept: 'application/json' };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (token !== undefined) headers.authorization = `Bearer ${token}`;
  let response;
  try {
    response = await fetch(new URL(path, origin), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: 'manual',
      signal: AbortSignal.timeout(15_000),
    });
  } catch {
    return { status: 0, payload: null };
  }
  let text;
  try {
    text = await readBounded(response);
  } catch {
    return { status: 0, payload: null };
  }
  let payload = null;
  if (text !== null) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = null;
    }
  }
  return { status: response.status, payload };
}

function tokenKid(token) {
  const parts = token.split('.');
  if (parts.length !== 3) throw new Error('login returned malformed token');
  let header;
  try {
    header = JSON.parse(Buffer.from(parts[0], 'base64url').toString('utf8'));
  } catch {
    throw new Error('login returned malformed token header');
  }
  if (!header || typeof header.kid !== 'string' || !header.kid) {
    throw new Error('login token has no key id');
  }
  return header.kid;
}

function unknownKidToken(token) {
  const [encodedHeader, payload, signature] = token.split('.');
  let header;
  try {
    header = JSON.parse(Buffer.from(encodedHeader, 'base64url').toString('utf8'));
  } catch {
    throw new Error('login returned malformed token header');
  }
  header.kid = `unknown-${randomBytes(8).toString('hex')}`;
  const replacement = Buffer.from(JSON.stringify(header)).toString('base64url');
  return `${replacement}.${payload}.${signature}`;
}

async function login(origin, email, password) {
  const { status, payload } = await requestJson(origin, 'POST', '/api/v1/auth/login', {
    body: { email, password },
  });
  if (status !== 200 || typeof payload?.access_token !== 'string' || !payload.access_token) {
    throw new Error('login failed');
  }
  return payload.access_token;
}

async function validate(origin, token) {
  return (await requestJson(origin, 'POST', '/api/v1/auth/validate-token', { token })).status;
}

function requirePass(name, condition) {
  if (!condition) throw new Error(`verification failed: ${name}`);
  console.log(`PASS ${name}`);
}

function dryRun() {
  const before = checkedOrigin('http://gateway-before.example', 'before URL');
  const after = checkedOrigin('https://gateway-after.example', 'after URL');
  const header = Buffer.from(JSON.stringify({ alg: 'EdDSA', kid: 'old-fixture' })).toString('base64url');
  const sample = `${header}.fixture-payload.fixture-signature`;
  const changed = unknownKidToken(sample).split('.');
  const changedHeader = JSON.parse(Buffer.from(changed[0], 'base64url').toString('utf8'));
  requirePass('safe origin validation', before !== after);
  requirePass('token kid parsing', tokenKid(sample) === 'old-fixture');
  requirePass(
    'unknown-kid request preserves token payload and changes only header kid',
    changed.length === 3 &&
      changed[1] === 'fixture-payload' &&
      changed[2] === 'fixture-signature' &&
      changedHeader.kid.startsWith('unknown-'),
  );
  requirePass(
    'public JWK filter recognizes private fields',
    PRIVATE_JWK_FIELDS.has('d') && !PRIVATE_JWK_FIELDS.has('x'),
  );
  console.log('DRY RUN only; no network requests sent.');
}

async function main() {
  if (process.argv.length === 3 && process.argv[2] === '--dry-run') {
    dryRun();
    return;
  }
  if (process.argv.length !== 2) {
    throw new Error('usage: node scripts/verify-jwt-key-rotation-http.mjs [--dry-run]');
  }

  const before = checkedOrigin(setting('JWT_ROTATION_BEFORE_URL'), 'before URL');
  const after = checkedOrigin(setting('JWT_ROTATION_AFTER_URL'), 'after URL');
  const email = setting('JWT_ROTATION_EMAIL');
  const password = setting('JWT_ROTATION_PASSWORD');
  const oldKid = setting('JWT_ROTATION_OLD_KID');
  const newKid = setting('JWT_ROTATION_NEW_KID');

  const oldToken = await login(before, email, password);
  requirePass('pre-rotation token uses old kid', tokenKid(oldToken) === oldKid);

  const { status: jwksStatus, payload: jwks } = await requestJson(
    after,
    'GET',
    '/.well-known/jwks.json',
  );
  const keys = Array.isArray(jwks?.keys)
    ? jwks.keys.filter((key) => key && typeof key === 'object')
    : null;
  requirePass('public JWKS endpoint is unauthenticated', jwksStatus === 200 && keys !== null);
  const kids = new Set(keys.map((key) => key.kid));
  requirePass('active and retained keys are published', kids.has(oldKid) && kids.has(newKid));
  requirePass(
    'JWKS contains public fields only',
    keys.every((key) => !Object.keys(key).some((field) => PRIVATE_JWK_FIELDS.has(field))),
  );

  requirePass('old token verifies after rotation', await validate(after, oldToken) === 200);
  const newToken = await login(after, email, password);
  requirePass('post-rotation token uses new kid', tokenKid(newToken) === newKid);
  requirePass('new token verifies', await validate(after, newToken) === 200);
  requirePass('unknown kid is rejected', await validate(after, unknownKidToken(oldToken)) === 401);

  const logout = await requestJson(after, 'POST', '/api/v1/auth/logout', { token: oldToken });
  requirePass('old session revocation succeeds', logout.status === 200);
  requirePass('revoked old token is rejected', await validate(after, oldToken) === 401);
  requirePass('new active-key session remains valid', await validate(after, newToken) === 200);

  const expiredToken = process.env.JWT_ROTATION_EXPIRED_TOKEN?.trim();
  if (expiredToken) {
    requirePass('expired token is rejected', await validate(after, expiredToken) === 401);
  }
}

main().catch((error) => {
  // All thrown messages are static stage labels; HTTP bodies and credentials
  // are deliberately never included in diagnostics.
  console.error(error instanceof Error ? error.message : 'verification failed');
  process.exitCode = 1;
});
