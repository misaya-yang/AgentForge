# Tenant account administration

Status: Native first-version implementation integrated. Workspace checking passed on 2026-10-12; binary linking, regression and authenticated acceptance pending.
Owner: Gateway Control. No new process, schema, token issuer or Agent kernel.

## Existing model and public flow

Each `users.user_id` belongs to exactly one `tenant_id`. Accounts and lowercased
email identities are globally unique. This implementation completes the existing
`/api/v1/users` management flow and its web directory/editor; it does not introduce
an invitation model, tenant switching or multi-tenant membership records.

The established list/get/create/update/delete/enable/disable/reset-password/profile
routes and response shapes remain. List/get use the caller's tenant and existing
`user:list` permission. Every account administration write additionally requires
an exact `admin` or `platform_admin` role. A delegated `user:create`, `user:edit` or
`user:delete` permission alone no longer grants account administration authority.
Empty tenant identity is rejected rather than falling back to `default`.

Self-service profile updates continue to permit only the authenticated account's
display name. Self-administration through other users routes is rejected: use the
profile and authentication password-change flows for your own account. The
reserved `admin` account and any account containing `platform_admin` are protected
from tenant administration, including password resets. These routes cannot assign
`platform_admin`; explicit operator bootstrap owns that initial identity.

## Shared write boundary

`platform-gateway-foundation/src/users_store/guard.rs` owns the common guard used
by every PostgreSQL account mutator. Within one supervised transaction it locks:

1. The active tenant row.
2. The actor account in that tenant, rechecking active status and a current,
   nonexpired `admin`/`platform_admin` assignment also present in `users.roles`.
3. The target account in that same tenant, where one already exists.

The tenant lock serializes native account administration. A caller waiting behind
another administrator must pass the authority check again after acquiring it.
Role removal, disabling and deletion may not remove the last active administrator;
remaining administrators must have current role assignments. Cross-tenant target
IDs remain not-found and never reach an unscoped mutation. Expiry checks use the
current database clock, not the transaction's potentially stale start time.

The profile path takes the same lock order but requires ownership instead of an
administrator assignment. It cannot carry roles, direct permissions or other
administrative fields. Unknown mutation fields fail closed. Status accepts only
`active`/`disabled`; tier accepts `normal`, `premium`, `enterprise` or `admin`.

Create inserts the account and its catalog-validated roles together. There is no
upsert and no separate role-assignment method: identifier/email collisions produce
a conflict and cannot overwrite an existing account, including one in another
tenant. The existing case-insensitive email uniqueness index protects concurrent
creates across tenants. Role and direct-permission edits update canonical assignment
rows atomically and clear legacy `users.permissions` fallback authority. Missing
or expired administrator assignments require operator reconciliation; stale arrays
are not silently treated as authority.

Role names must exist, fit the existing account role column, and cannot include
`platform_admin`. Direct permissions must exist in the permission catalog, with the
existing `admin:*` permission also accepted. Duplicate assignment values do not
create duplicate association rows. Permission-read failures are surfaced rather
than presented to the editor as an empty grant set.

Each successful account mutation writes a same-transaction `login_audit` event
with the action, target user, actor user and tenant. No password/hash, email,
profile content or credential is included. These records are in the existing
account audit table; they are not configuration-audit records. Permission cache
invalidation remains after commit. A callback or transport failure may occur after
a successful commit; callers must refresh rather than automatically retry.

## Credential lifecycle integration

`users.password_changed_at` also acts as a credential/session invalidation marker.
Creation initializes it with `clock_timestamp()`. Password reset and disabling
advance it after acquiring the account row lock. Reset additionally replaces the
password hash, sets `force_password_change=TRUE`, clears `login_attempts` and
`locked_until`, and updates `updated_at`. Disabling and deletion invalidate bound
API keys with `enabled=FALSE`, scoped by user and tenant. Re-enabling does not
restore those keys.

The authentication service must enforce its session's recorded credential revision
against this marker. That companion auth change is required to reject existing JWT
sessions following reset/disable and to prevent a replacement account reusing a
previous user's session after an identifier is deleted and recreated. The timestamp
write alone is not proof that JWT revocation is enforced.

## Shared roles and membership visibility

Role definitions remain global in the existing schema. Definition create/update/
delete requires both the exact `platform_admin` role and a fresh database check of
its active account, active tenant and nonexpired platform-administrator assignment.
Tenant administrators may read the catalog and assign existing non-platform roles
through the guarded account flow; they cannot redefine shared roles.

Role list/get counts and `/roles/{role_name}/users` are filtered to the current
tenant and nonexpired assignments, including for platform administrators. Deleting
a global role still checks assignments across every tenant before proceeding.
This is deliberately a global catalog with tenant-scoped membership, not a new
tenant-owned role-definition model. Existing global role-definition persistence
and invalidation semantics are unchanged; their multi-statement transactional
redesign is outside this account-administration package.

## Web behavior

Directory and editor controls mirror exact administrator and protected-account
rules, hide `platform_admin` assignment, and encode account IDs in request paths.
Changing signed-in account/roles or editor target remounts pending forms. A
single-flight mutation guard prevents duplicate clicks and never retries writes.
A failed or uncertain request blocks another change until an explicit successful
refresh. Batch deletion stops at the first error and refreshes committed progress;
it does not replay earlier deletions. Server authorization remains authoritative.

## Compatibility, verification and rollback

Existing schemas, route paths and response bodies are preserved. Authorization is
intentionally stricter; previous tests expecting cross-tenant role membership,
platform-role assignment, empty-tenant fallback, partial account creation or
unprivileged account writes must change. `UsersDatabase` now carries actor IDs for
delete/reset, `UserMutation` carries profile intent, and the standalone assignment
method is removed. Alternative database implementations must implement the same
write guarantees. New role-store scoped/default authority methods fail closed.

Required before acceptance:

- Direct Rust compilation and external contract suites for the changed auth,
  foundation and host crates, using the repository's authorized hosted/Docker gates.
- TypeScript app/node checks and frontend lint/build from `docs/harness/commands.md`.
- Isolated PostgreSQL exercises: cross-tenant IDs; empty identity; ordinary
  user/admin/platform-admin; expired grants; self/protected targets; concurrent
  mutual demotion/disabling/deletion; duplicate ID/email; audit rollback; failed
  role insertion; cancelled/uncertain commits; bound key invalidation.
- Authentication integration: sessions minted before/after reset, disable/re-enable
  and delete/recreate cannot bypass the credential revision boundary.
- Browser journeys: loading errors, protected controls, duplicate clicks, failed
  writes, batch partial progress, explicit refresh, account change and navigation.

No build, test, migration, runtime, browser or credential operation was executed
while preparing this source change. No verification pass is claimed here.
There is no database migration. Rolling code back restores weaker administration
rules and must not be presented as a security-preserving rollback. API keys disabled
by these operations stay disabled; do not bulk re-enable them during rollback.

## Source-owned native account contract (2026-10-13)

The bounded exporter declares the existing nine account lifecycle operations on
six common paths. `native_users_contract.rs` owns the mounted route inventory,
shared validation field sets/limits, and the existing projection defaults and
success envelopes. `users_api.rs` retains authorization ordering, email/domain
normalization, projection checks, database calls, mutation ordering and error
rendering. The producer never reads the published schema. Common/Python ownership,
`sdk/openapi.json`, and `sdk/openapi-profiles.json` remain unchanged.

The self-profile resolver hook is distinct, but production wiring delegates it
to the same AdmissionService used by the administration resolver. Both permit
JWT or database-bound API-key authority. Profile does not use endpoint RBAC;
it still requires nonempty tenant/account identity and the exact caller's ID.
It ignores administrative request fields and changes only display name. Its
existing response does not derive service-access fields from metadata; ordinary
account reads do. These real behavior differences are retained in the declaration.

Request bodies ignore unknown keys. Create accepts a local email part, expands
it using the configured domain, trims/lowercases it, and requires that domain.
Display-name/department lengths count Unicode characters. Omitted fields preserve
values. Explicit null display name/department is forwarded; null roles, direct
permissions and service-access values leave those values unchanged. Empty arrays
replace assignments/policy lists. Null status/tier reaches the existing PostgreSQL
validation guard and is rejected there. No stricter HTTP DTO or response validator
is introduced. Request declarations leave normalizing/dynamic validation explicit.

`project_user` emits a closed set of validated fields. Its nullable date strings
have no guaranteed date-time format, and status/tier/mode responses remain strings.
Unknown source-row columns are omitted, while dynamic rows elsewhere in the
export remain open. All nine successful operations retain HTTP 200. Request 422
responses contain detail arrays; storage guard 422 responses contain detail strings.
Duplicate email preflight remains 400, while create races and last-administrator
protection can be 409. Admission/RBAC error envelopes are not replaced with account
detail envelopes. Host cancellation and additional response possibilities remain
explicitly undeclared. Errors can occur after mutation or cache invalidation;
readback is required before deciding whether another write is safe.

The freshly compiled and source-bound increment declares 33 paths and 43
operations, with 409 of the 452 published operations absent. The unchanged
full-native release comparator returns FAIL431. Relative to the preceding
Services FAIL419, six formerly missing Users paths are now declared; it reports
nine Users authentication differences and nine 422 `detail` field diagnostics.
The latter are the comparator's observed result: it follows `$ref`/`allOf`, but
not the declared `oneOf` branches. Each branch still requires `detail`; runtime
request errors retain detail arrays and storage guard errors retain detail strings.
No runtime detail field was removed. The mixed value shapes and authentication
requirements remain explicit rather than being rewritten to satisfy the comparator.

Executed offline evidence: 17 exporter tests, 26 foundation contract tests
(including ten Users handler fixtures and the real storage denial-envelope
fixture), seven existing atomic registration regressions, and 55 Python native
export verifier tests passed. Canonical Python lint passed. A two-process source
export is deterministic, all source bindings match, and the exporter and test
ELFs are bound to actual compiler inputs. These are bounded fixtures using
controlled identity/database collaborators; they do not establish transactional
PostgreSQL, session revocation, authenticated HTTP/browser or CL-01–06 acceptance.

Required checks in the approved serialized external build environment:

- `cargo test --locked --offline -p ai-platform-openapi`
- `cargo test --locked --offline -p ai-platform-gateway-foundation --lib contract_tests`
- `uv run --all-packages --extra test pytest -q --no-cov tests/contract/test_native_openapi_export.py tests/contract/test_openapi_profiles.py`
- Fresh independent source-bound native export, deterministic-process comparison,
  compiler dependency/source binding and the unchanged full-native release gate

The listed full Python-profile suite and live/runtime gates remain pending; the
executed subset and its limits are stated above.
