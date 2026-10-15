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
