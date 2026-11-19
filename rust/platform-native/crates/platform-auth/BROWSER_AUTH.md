# Browser sessions and API access tokens

Owned by `platform-auth`; Gateway bootstrap is the composition root. This source
implements U-GW-03. Compilation, isolated security tests and real Console/TLS
acceptance are separate gates; implementation alone does not claim acceptance.
No new PostgreSQL schema is needed. Existing Gateway users/password update rights,
login audit and Redis session authority remain required.

## Activation and TLS

Without `AI_PLATFORM_BFF_ORIGINS`, Gateway retains explicit API/CLI-only mode.
Console BFF activation requires all of:

- `AI_PLATFORM_BFF_ORIGINS`: JSON array of exact HTTPS origins, without trailing
  slash, wildcard, path or credentials. Example public configuration:
  `["https://console.example.invalid"]`.
- JWT authentication enabled, existing legacy HS256 secret retained for published
  bearer compatibility, and nonempty configured JWT issuer and audience.
- `AI_PLATFORM_API_EDDSA_CONFIG_FILE`: absolute path to a JSON key configuration.
  Its contents are public metadata and private-key file references, never a
  private key inline. The file may not be group/world writable.
- An HTTPS endpoint on the cookie host (or a TLS proxy preserving the browser
  Origin). No HTTP/dev-cookie downgrade exists; even localhost must use HTTPS.

Public key configuration example (paths and timestamps are illustrative; supply
an actual currently valid window before starting a service):

```json
{
  "active_kid": "api-2026-10",
  "private_key_file": "/run/secrets/api-access-ed25519-private.pem",
  "public_keys": [
    {
      "kid": "api-2026-10",
      "public_key_file": "/run/config/api-access-ed25519-public.pem",
      "not_before": 1790000000,
      "not_after": 1830000000
    }
  ]
}
```

Private keys use Ed25519 PKCS#8 PEM; public keys use SPKI PEM. The active public
key must match the private key. At most two public keys are accepted: current and
previous during rotation. IDs are unique and bounded. The active key must cover
now plus the complete 300-second access-token TTL; signing outside its window
fails. Removing a previous `kid` revokes that key for verification; expiration is
exact. Update the configuration and restart/reload the service graph as one
operator-controlled rotation. No runtime key generation or remote JWK lookup is
performed. Publish only public material at `GET /.well-known/jwks.json`.

Key reads use a no-follow, close-on-exec, nonblocking file descriptor on Unix,
validate regular-file type and a 16 KiB bound on that descriptor, and reject
private keys owned by a different effective user or with any group/world access.
Use mode 0600 or stricter and correct process ownership. Public/config files must
not be group/world writable. A platform without equivalent private-file ACL
validation fails closed rather than silently accepting the file.

## Browser contract

| Operation | Request | Successful response |
| --- | --- | --- |
| `POST /api/auth/session` | Existing `{email,password}` JSON; exact allowed Origin and same-origin/same-site Fetch Metadata required | `{user,force_password_change,expires_in:10800,session_type:"cookie",csrf_token}` plus `Set-Cookie` |
| `GET /api/auth/session` | Cookie; same-origin/same-site Fetch Metadata; supplied Origin must match | `{user,csrf_token,expires_at}`; user is the existing current-user projection |
| `DELETE /api/auth/session` | Cookie, allowed Origin, Fetch Metadata and `X-CSRF-Token` | `{message}` and expired cookie |
| `POST /api/auth/session/password` | Cookie, same protections; existing `{current_password,new_password,confirm_password}` | `{message}` and expired cookie; sign in again |

The cookie is `__Host-agentforge_session`, `HttpOnly; Secure; SameSite=Lax;
Path=/`, without Domain. Its value is random opaque material; Redis uses a hash
of that value as the alias key. The BFF record contains user/tenant, existing
allowlist JTI, exact credential revision, CSRF, issued/idle/absolute expiry. It
contains **no JWT**. The alias and existing live allowlist are bounded to the
original three-hour Console lifetime; idle and absolute bounds are both three
hours to preserve existing behavior. GET restores CSRF/profile state without
extending the absolute deadline. Expired, revoked or unknown records are never
recreated by refresh. Binding the initial allowlist uses an atomic compare-and-
expire on an already-live exact record, preventing logout/expiry resurrection.

Every cookie request rechecks the same current account/tenant, credential revision,
current permissions and Redis revocation as bearer admission. Existing Gateway
adapters require bearer input, so the bridge mints a **request-local 300-second
EdDSA token** bound to that same live JTI after those checks. It neither stores
that token nor sends it to the browser. The bridge runs before host identity,
rate identity and endpoint authority; existing API-key and CLI bearer requests
without browser cookies keep their own path. Mixed cookie/Authorization requests
are rejected. WebSocket handshakes additionally require exact allowed Origin.

Authenticated state-changing requests require the synchronizer token in the
`X-CSRF-Token` header; comparison uses fixed-size digests. Missing/duplicate Origin,
Fetch Metadata, CSRF, duplicate session cookies and cross-site requests fail.
Fresh password login is protected by Origin+Fetch Metadata, and can replace a
stale cookie without requiring a CSRF token from a dead session. BFF responses
are no-store; credentials and Set-Cookie are marked sensitive. JavaScript stores
only CSRF in memory, uses `credentials: include`, and restores from GET after a
reload. No browser bearer/localStorage/sessionStorage auth persistence is valid.

Legacy `/api/v1/auth/*` remains the published bearer surface. With explicit
EdDSA keys it issues five-minute API access tokens; configured HS256 legacy tokens
remain verifiable with their existing expiry, issuer, audience and server-side
revocation constraints. API keys retain existing hashed/scoped validation.
`browser_contract::extend_document` adds the new BFF/JWKS schemas without rewriting
the frozen five-route legacy contract.

## Password work bounds

New passwords use Argon2id v19, 19,456 KiB, two iterations, one lane, 32-byte output
and a fresh OS-random UUID salt. Existing bcrypt verification retains the 72-byte
no-truncation boundary. Only successful bcrypt verification creates an Argon2id
replacement. Compare-and-set updates only the matched tenant/user/password hash
on an active unlocked account; it preserves `password_changed_at`, so an encoding
upgrade does not revoke valid sessions. An actual password change still changes
that exact revision and invalidates sessions.

Password hash, verify and verify+rehash run under the same semaphore in
`spawn_blocking`; the permit survives caller cancellation until CPU work ends.
Configured concurrency is 1–8. Stored PHC work factors are bounded to 64 MiB,
ten iterations and four lanes, putting concurrent verification memory below
512 MiB for one service instance. Malformed, unsupported-version/algorithm and
out-of-bound hashes fail verification; none falls back to another algorithm.

## Verification still required by the primary builder

Run auth unit tests, Gateway bootstrap/auth tests, affected HTTP/OpenAPI and Web
regressions. Security cases include missing/forged CSRF and Origin, cross-site and
WebSocket Origin, duplicate cookies, expired/unknown/revoked sessions, credential
revision change, tenant/account disable, failed bcrypt without rehash, concurrent
password CAS, active/previous/revoked key IDs, signature/audience/issuer/expiry
failures, and private-file permissions. Real Redis/PostgreSQL and HTTPS Console
acceptance remain deferred to the authorized service-integration window. No
private environment or credential value belongs in reports or telemetry.
