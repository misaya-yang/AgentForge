# ADR-010: Authoritative tenant identity and Knowledge authorization

Status: Accepted for phase-one implementation, 2026-09-07; updated after epochs 001/002.

Scope: ID-01/02/03 and their schema compatibility boundary. Implementation and final
acceptance status remain in [the vNext runbook](../../deploy/runbooks/agent-platform-vnext/README.md)
and its `loop-state.json`; this ADR does not certify live deployment or rollback.

## Context

Eval used one configured credential while job parameters supplied a different possible tenant.
Knowledge matched bare role names across tenants and read Gateway account/RBAC tables directly.
The fix must preserve explicit user sharing, public datasets and creator ownership while making
identity, role scope and destructive-action confirmation authoritative.

## Decision

1. `/api/v1/auth/me` returns the authenticated UserContext's `tenant_id` in both profile and
   fallback responses. A profile row, request header or Eval job parameter cannot replace it.
   Before candidate creation, Eval introspects the same credential used for execution and requires
   a valid subject and an exact, non-public job tenant match. Missing fields, old servers, redirects
   and lookup failures are rejected; identity is not cached between jobs.
2. V2 `ThreadCreateRequest.expected_tenant_id` is an optional precondition, never an identity
   input. A mismatch fails before session storage with 409
   `AGENT_RUNTIME_TENANT_PRECONDITION_FAILED`. Eval's self-declared identity headers are removed;
   trace attribution remains distinct from authentication.
3. Dataset role permissions apply only when actor and dataset tenants match. Explicit user grants,
   public visibility and creator ownership retain their existing semantics. The public ACL API
   continues to accept/return `subject_type: role`; persistence maps it to `tenant_role` and derives
   `subject_tenant_id` from the dataset. Callers do not choose the stored tenant discriminator.
4. Knowledge resolves Runtime capability actors through Gateway's signed
   `/internal/v2/agent-capabilities/knowledge-actor` endpoint after capability proof validation.
   Gateway loads the account by user and tenant, rejects inactive/missing accounts, and obtains
   current roles from the role assignments including expiry/revocation. An empty role set is
   authoritative; no fallback to stale `users.roles`, JWT roles or caller-provided roles is allowed.
   Knowledge rejects malformed, mismatched or unavailable actor responses instead of inventing
   a normal user. Knowledge no longer needs direct access to Gateway account/RBAC tables.
5. Gateway owns dataset deletion password verification. Both public Knowledge proxy paths reject
   a caller-supplied `_gateway_delete_confirmation`, verify the account in the authenticated tenant,
   remove the password and insert action/dataset/user/tenant/expiry claims. The existing v2 internal
   envelope binds claims to identity, method, path, body and a replay-protected request ID.
   Knowledge requires that verified envelope and matching claims; anonymous/v1 calls cannot pass
   confirmation. Existing dataset permission checks, deletion fences and cleanup ordering remain.

No new token issuer, cross-tenant administrator bypass or Agent kernel is introduced.

## Implemented schema boundary

The frozen `2026_08_post_kb_v1` baseline is unchanged. Its ordered post-baseline manifest contains:

- `001_knowledge_tenant_permissions.sql`: convert existing role grants to `tenant_role`; add
  `subject_tenant_id`, subject-shape CHECK and `(dataset_id, subject_tenant_id)` foreign key.
  Legacy readers querying `role` cannot inherit migrated grants; legacy writers cannot recreate
  unqualified role grants. Explicit user grants keep a null subject tenant. Revoke Knowledge API
  and worker privileges on Gateway users/RBAC tables and their owned sequences, using the
  configured authority role prefix. Unrelated required privileges, including audit insertion,
  are retained.
- `002_gateway_eval_leases.sql`: add Eval outbox owner/claim/lease/heartbeat fields and case
  runtime handles/dispatch state. It is an Eval recovery migration, not a second tenant-ACL
  migration. Legacy running work becomes expired/reconciliation-required without losing history;
  old workers must be quiescent before migration.

The shared `database_revision` contract currently requires epoch **2 exactly**. Authority migration,
verification and repeated initialization use the ordered manifest, recorded changes, frozen epoch
reference fingerprints and postconditions. Knowledge API/worker startup refuses an incompatible
baseline/epoch; lowering the application floor or editing frozen baseline SQL is not a repair.

## Validation and rollback

Source tests cover authenticated tenant preconditions, same/different-tenant ACL roles, explicit
user/public sharing, actor revocation/failure, forged confirmation, v1 rejection and replay binding.
The opt-in PostgreSQL matrix checks fresh/repeated authority operation, two role prefixes,
legacy-reader/writer behavior and actual Knowledge grants. Static/mock passes and generated
reference fingerprints are separate from live migration and end-to-end acceptance receipts.

Both epoch migrations are declared `forward-fix-only`. Do not roll back by changing stored roles
back to unqualified `role`, restoring Gateway account-table grants to Knowledge, or disabling the
schema floor. A separately reviewed frozen release/database rollback must preserve the safe reader
and identity authority boundary. No complete restore or frozen release rollback is claimed here.
