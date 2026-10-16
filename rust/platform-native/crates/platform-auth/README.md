# Native account session authority

Gateway JWT authentication requires both a valid token and its existing Redis
allowlist record, then reloads current account, tenant and role authority.
New login records also store the exact nullable `password_changed_at` value as
`credential_revision`. Authentication compares this revision against the
database, so password changes, administrative resets and account disablement
invalidate older sessions even if their Redis entries have not expired.
The field is an account credential/session invalidation marker; disablement also
advances it so re-enabling an account cannot revive its previous sessions.

This revision is stored only in server-owned session metadata. It is not a
password hash, a public claim, or a newly generated credential. No token format or
database schema change is required. A login racing a reset may return a token
that immediately fails authentication; it cannot regain the old authority.

Legacy session records without the revision use a conservative token-issuance
comparison. Tokens issued before or during the password-change second require a
fresh login. New records use the precise revision rather than whole seconds, so
a fresh login in that same second is valid. Redis/storage failures fail closed.

Password-change responses do not issue replacement tokens: clients must return
to normal login. Existing API keys remain governed by their separate authority;
this change only invalidates JWT login sessions. Requests already admitted before
an account change are not retroactively cancelled.

The revision guard requires compilation and isolated reset, same-second login,
disable/re-enable and legacy-session regression before release acceptance.

## Source-owned HTTP contract publication

The existing five mounted account operations (login, logout, current user,
validate-token and password change) now share route constants, serde request and
success response types with a small declaration module owned by this crate.
LoginResponse/UserProfile remain available at their previous `service` paths by
reexport. Handlers, account/session authority, request ordering and storage effects
are unchanged; there is no new authentication state, endpoint or fallback.

The bounded offline exporter includes these common operations through its existing
pipeline and records hashes of this crate's contract, handlers, service, account and
session owners plus the real Gateway mount. The common implementation manifest
retains Python/hybrid ownership. Neither the published SDK schema nor its profile
partition is rewritten. Five previously absent declarations do not establish full
native API compatibility, and any actual security/status/schema differences remain
visible in the full comparison.

Login is public. Logout with no Authorization header succeeds; a supplied header
must satisfy the existing strict Bearer parser and JWT verification before Redis
revocation/audit; logout does not require a current allowlist record or reload
account/tenant authority. Current-user, validate-token and
password-change require JWT session/account authority and do not accept API keys.
Login and password-change requests retain required string fields, unknown-field
tolerance, duplicate-field serde rejection, a 16 KiB body limit and existing
extractor status/detail behavior. Dynamic email/password normalization and account
policy remain service-owned. Password change returns no new token. Response schemas
preserve nullable profile fields and distinguish permissions from effective
permissions. Undeclared middleware/host outcomes remain explicitly open.

The actual auth library passed an offline, locked single-job Cargo check, including
its HTTP/service code and moved DTOs. The current source-owned exporter was freshly
compiled and linked. Its unchanged path-included auth module passed all four focused
Rust contract tests: request serde behavior, serialized success shapes, route/security
projection and login-only status declarations. The 23 unrelated exporter tests were
filtered out; the full auth/Gateway test suite was not run.

The fresh producer executed twice with identical bytes. The existing wrapper verified
current source digests and composed the retained Python Knowledge owner declarations
through two offline hash-seed processes. Measured coverage is now 43 paths and 63
operations, with 389 published operations still absent from this bounded document;
101 Knowledge owner operations remain separate forwarding metadata. Actual exported
auth schemas passed 9 positive and 35 negative cases. This is source/export evidence,
not live authentication, database/session or provider acceptance.

The full native compatibility comparison was executed and FAILED with 457 diagnostics.
The five auth declarations remove five missing-path findings while exposing six
existing differences: four security-requirement differences and two 422 detail-shape
differences (published array versus the native string). These differences are retained
for owning-contract decisions, not hidden by broadening schemas or changing behavior.
The published SDK schema and Python/hybrid profile partition remain unchanged.

Python exporter-wrapper/source-binding fixtures previously passed 87 tests; scoped
baseline and candidate Ruff checks passed with zero findings. The initial
84-pass/3-failure fixture attempt remains retained: fixture expectations and mutation
proofs were updated for the added source bindings, and lint baseline package context
was corrected without changing runtime Rust source. Exact source, dependencies,
compiler/linker and protected historical artifacts were bound before/after the native
checks; the 8 GiB disk admission floor was preserved. No old binary supplied the
new export. Whole Gateway/integrations production assembly, auth library test execution
and live PostgreSQL/Redis session behavior remain separate acceptance boundaries.

## Gateway credential introspection compatibility (2026-10-15)

Gateway now mounts `router_with_introspection` for GET `/api/v1/auth/me` only.
The injected `CredentialIntrospectionResolver` returns profile/credential metadata,
not a `Principal`, and does not populate the request authorization cache. Its
Gateway implementation accepts cryptographically verified external JWT metadata
without a local profile, or a valid enabled/unexpired database API key. Existing
local accounts still must be active and match the credential's user and tenant;
lookup failures return 503. Session-bearing JWTs consult the configured Redis
allowlist. Configured static-key fallback retains the existing context behavior.
Caller-provided user/tenant headers never establish this identity.

This restores the retained source `auth.py::get_current_user` and
`deps.py::_current_account_authority` contract: an absent local profile is distinct
from an unavailable or disabled account. Profile fields, roles and permissions in
this endpoint are descriptive metadata, including current account profile fields
for a key; they do not expand that key's protected-route grants.

The plain `router` factory remains session/account-only for callers that do not
provide this seam. Validate-token, password change, `AdmissionService`, and
`EndpointIdentityResolver` retain their existing strict session/current-account
policy. In particular this introspection change does not grant external
sessionless JWTs or missing-profile keys access to protected endpoints. The
retained source's broader external-identity federation on those endpoints remains
a separately documented policy difference pending its owning decision.

## JWT signing key rotation

The Gateway defaults to `asymmetric_keyring`. Configure an explicit `active_kid`
and one or more asymmetric keys under `authentication.jwt.keyring`. Supported
signing algorithms are `RS256`, `ES256`, and `EdDSA`. Each entry contains a public
`public_jwk`; only the active entry names a `private_key_env`, whose environment
value is a PKCS#8 PEM private key. Private key bytes never enter settings summaries
or the public JWKS. The active key has no `verify_until`. A retired key has no
private-key environment reference and must set `verify_until` to its absolute Unix
expiry time.

Example shape (the `n` and `e` values below are placeholders, not usable keys):

```yaml
authentication:
  jwt:
    enabled: true
    mode: asymmetric_keyring
    algorithms: [RS256]
    active_kid: gateway-2026-10
    keyring:
      - kid: gateway-2026-10
        algorithm: RS256
        private_key_env: GATEWAY_JWT_ACTIVE_PRIVATE_PEM
        public_jwk:
          kty: RSA
          alg: RS256
          use: sig
          key_ops: [verify]
          kid: gateway-2026-10
          n: <base64url-public-modulus>
          e: AQAB
      - kid: gateway-2026-07
        algorithm: RS256
        verify_until: <absolute-unix-seconds>
        public_jwk:
          kty: RSA
          alg: RS256
          use: sig
          key_ops: [verify]
          kid: gateway-2026-07
          n: <base64url-public-modulus>
          e: AQAB
```

The native environment loader accepts the same settings through
`GATEWAY_AUTHENTICATION__JWT__ALGORITHMS`,
`GATEWAY_AUTHENTICATION__JWT__ACTIVE_KID`, and
`GATEWAY_AUTHENTICATION__JWT__KEYRING` (the last value is a JSON array).
`GATEWAY_JWT_MODE=asymmetric_keyring` selects the secure default explicitly for
environment-only deployments. Keep `JWT_SECRET` and
`GATEWAY_AUTHENTICATION__JWT__SECRET` empty in this mode. The local deployment
environment maps `GATEWAY_JWT_ACTIVE_PRIVATE_PEM` into the Gateway process; point
the active key's `private_key_env` at that variable. When storing PEM in `.env`,
encode line breaks as the two characters `\n`; the native settings loader restores
them before parsing the private key. Secret providers may supply normal PEM
newlines directly. `scripts/new/init-env.sh` encodes newlines from an exported PEM
value when it writes the local `.env` file.

On rotation, deploy the new active key together with the previous public JWK and
set that old entry's `verify_until` later than the last token's `exp`. Account
session tokens last at most 10,800 seconds. The old key remains accepted until
`verify_until`; at that time verification rejects it and JWKS omits it. New
asymmetric tokens include `kid`; an absent or unknown `kid` is rejected. The
public `GET /.well-known/jwks.json` endpoint publishes only live asymmetric public
keys with verify-only key operations.

Existing JWT_SECRET-only deployments must explicitly select
`legacy_single_hs256` using `GATEWAY_JWT_MODE` or `authentication.jwt.mode`.
That migration mode uses exactly one HS256 key and continues to accept old tokens
without a `kid`; it publishes an empty JWKS because an HMAC secret has no public
verification key. There is no automatic fallback from asymmetric configuration
to the legacy secret. The local quickstart example opts into this mode explicitly;
production should use the asymmetric keyring.
In asymmetric mode, leave the legacy `JWT_SECRET` unset/empty; configuring both
legacy secret material and an asymmetric keyring is rejected.

The live two-endpoint smoke script
[`scripts/verify-jwt-key-rotation-http.mjs`](../../../../scripts/verify-jwt-key-rotation-http.mjs)
checks a pre-rotation login against the post-rotation Gateway, new-key issuance,
public-only JWKS, unknown-`kid` rejection, and Redis-backed logout revocation. Run
`node scripts/verify-jwt-key-rotation-http.mjs --dry-run` to check its local
helpers without sending requests. Run it only against two Gateway deployments
sharing the intended test account database and Redis session store by setting
`JWT_ROTATION_BEFORE_URL`,
`JWT_ROTATION_AFTER_URL`, `JWT_ROTATION_EMAIL`, `JWT_ROTATION_PASSWORD`,
`JWT_ROTATION_OLD_KID`, and `JWT_ROTATION_NEW_KID`, then running
`node scripts/verify-jwt-key-rotation-http.mjs`. It prints only pass/fail stage
names, never credentials, tokens or response bodies. Unit tests cover injected-time
key retirement and token expiry without waiting for a real token lifetime.
