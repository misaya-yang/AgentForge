# AS-08 Observability, Admin, Migration, and Aggregate Gate Plan

- Phase: `AS-08`
- Feature: `AS-F009`
- Status: executing
- Source of truth: `docs/agent-studio-prd/phase-08-observability-admin-migration-and-release-gate.md`

This is the fixed execution transcription required by AS-08. It does not alter
the approved architecture, dependencies, scope, acceptance gates, or validation
commands, and it does not enter the terminal AS-09 release decision.

## Critical path

1. Add one forward-only, reentrant operations migration that gives Agent audit
   rows explicit Agent/Version/Publication/channel dimensions, persists a
   tenant/Agent governance policy and append-only deletion receipts, and adds
   the composite indexes/constraints needed for filtered traces, quotas,
   retention and legal hold without mutating immutable Versions or prior audit
   history.
2. Extend the existing AgentTraceRepository and AgentRepository rather than
   introducing a second operations store. Provide tenant- and role-scoped,
   paginated Agent analytics, traces and redacted audits; compute metrics from
   explicit columns; expose retention-limited state; and record every sensitive
   policy, archive, token/grant revoke and deletion action.
3. Implement fail-safe governance operations for Agent/channel quotas, archive,
   token and channel-grant revocation, retention cleanup, user-scoped deletion,
   tenant-scoped deletion, legal hold, attachment/object cleanup and cache
   invalidation. Keep immutable Versions, Publication history and audit/deletion
   receipts. A legal hold blocks destructive cleanup, and partial external
   storage cleanup remains a durable retryable request rather than a false
   success.
4. Add separate server mutation/public-runtime and frontend flags. Flag-off
   hides Agent creation/public entry points and rejects new Agent mutations while
   retaining authorized read-only evidence and leaving `/assistant`,
   `/knowledge`, `/eval` and `/share` functional. Application rollback retains
   all additive schema and `__builtin_assistant__` behavior.
5. Add `/agents/:agentId/analytics` with explicit Version/Publication/channel/
   time filters, summary metrics, paginated redacted Trace links, audits, quota/
   retention/legal-hold controls, empty/retention-limited/error states and exact
   Owner/Editor/Viewer/Admin visibility. Cover desktop/mobile, keyboard, focus,
   axe, console and browser-network redaction.
6. Create a versioned regression manifest plus `scripts/agent_studio_regression.py`,
   a contract test that derives every required AS-00 through AS-08 gate from the
   fixed Phase contracts and fails on omission, and `make verify-agent-studio`
   that executes only the checked manifest. Never delete or weaken an earlier
   gate to make the aggregate pass.
7. Run every AS-08 required validation command against final source, collect
   operations/governance, migration/rollback, browser and aggregate evidence,
   then request a fresh independent Critic. Only approved evidence plus the
   supported AS-08 claim check may unlock AS-09; no terminal completion verdict
   is issued here.

## Evidence boundary

Local PostgreSQL/Redis/storage and deterministic browser fixtures may prove the
operations contract without external credentials. Production dashboards,
provider quality and public deployment are not claimed. Tenant isolation,
redaction, least privilege, legal hold, complete revocation/deletion, additive
migration, feature-flag Assistant compatibility and aggregate-manifest
completeness are non-waivable.
