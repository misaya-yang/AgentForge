# AS-08 Observability, Admin, Data Governance, and Aggregate Gate Actor Report

**Phase:** AS-08 — Observability, Admin, Data Governance, and Aggregate Gate  
**Feature:** AS-F009  
**Status:** Iteration-2 replay blocker remediated; actor gates passed; pending fresh independent Critic  
**Date:** 2026-07-20  
**Actor:** primary implementation agent

## Outcome

AS-08 makes an Agent operable by explicit Agent, Version, Publication, channel
and time dimensions. Owners can inspect redacted traces plus TTFT, tool,
Knowledge and feedback metrics. Admins and Owners can inspect dimensioned,
recursively redacted audit records and manage retention, legal hold,
authoritative Tenant/Agent quotas, cache, credentials and scoped deletion.
Destructive cleanup is durable, retryable, rechecks legal hold at commit and
revokes every tenant runtime path. The original requester or a Tenant Admin can
replay the exact completed receipt after tenant cleanup removes ACL and marks
the Agent deleted; other users cannot read it. Immutable Versions, Publication
history, audit rows and deletion receipts are preserved.

Migration 081 is additive and reentrant. Separate management, publication and
frontend flags provide non-destructive application rollback while preserving
the existing Assistant. A versioned 39-gate manifest now exact-compares itself
with every AS-00 through AS-08 required machine-contract command, and
`make verify-agent-studio` records per-gate logs plus one same-source result and
now fails a required gate whose numeric test summary contains any skip even if
the command exits zero. AS-08 does not issue the terminal AS-09 release
decision.

## Plan and Architecture

- Fixed plan:
  `docs/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-plan.md`.
- Deviations: none. Existing AgentRepository and AgentTraceRepository remain
  the stores; Gateway remains the management/runtime boundary; Eval remains the
  trace detail surface; existing Assistant routes and `__builtin_assistant__`
  are not replaced.
- Execution corrections stayed inside the fixed boundaries: generated evidence
  is excluded from the aggregate source hash while source/tests remain covered,
  and tenant cleanup now makes the deleted state authoritative before mutable
  ACL removal so the existing history/owner invariants remain valid.

## Minimal Change Groups

| Group | Why required | Scope boundary |
| --- | --- | --- |
| Migration and repositories | explicit dimensions, complete metrics, hard quota enforcement, binding audits, retry receipts, indexes and durable cleanup | one additive migration and extensions to existing Agent/Trace repositories; no second operations store |
| API and schemas | analytics, audit, governance, cache, credential and deletion contracts | Agent management/Eval routes only; stable tenant/role authorization and redacted shapes |
| Flags and quickstart | application rollback without schema loss | two server inputs plus existing frontend runtime flag; Assistant routes unchanged |
| Analytics frontend | required Owner/Editor/Viewer operations UI and states | one lazy Agent route plus typed API/types, Eval trace query support and existing navigation links |
| Aggregate runner | immutable whole-demand command for AS-09 | one versioned manifest, one runner, one Make target and exact contract test |
| Tests/evidence | falsifiable operations, migration, compatibility and browser proof | AS-08 focused suites and reports only |

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 Operable and Auditable Agents | passed | exact dimension filters; TTFT/tool/Knowledge/feedback metrics; recursive at-rest redaction; binding/high-risk decision audits; Eval deep link and authenticated live probe |
| R2 Quota and Data Governance | passed | hard Agent/Publication/concurrency/token/MCP/storage ceilings; fail-closed policy lookup; threshold audit; legal-hold race denial; retryable object cleanup; complete user/tenant revocation and cleanup matrix; authorized exact terminal receipt replay |
| R3 Safe Migration and Compatibility | passed | migration reentrancy tests; remediated 081 re-applied with no pending migrations; feature-off browser path; AHR and credentialed isolation cases passed; normal Stub=false runtime restored healthy |
| R4 Versioned Aggregate Gate | passed | manifest v1 SHA-256 `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d`; 39 required commands; exact Phase-derived 5/5 contract including zero-exit skip rejection; validate-only pass |

## Exact Required Validation

| Gate | Exact command | Final result |
| --- | --- | --- |
| operations-governance | `uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/security/test_agent_data_governance.py tests/database/test_agent_studio_migrations.py` | exit 0; 24 passed, 0 failed, 0 skipped |
| aggregate-manifest | `uv run pytest -q --no-cov tests/contract/test_agent_studio_regression_manifest.py` | exit 0; 5 passed, 0 failed, 0 skipped |
| analytics-frontend | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-analytics.spec.ts --config playwright.opensource.config.ts` | exit 0; lint 0 errors/17 inherited warnings; type, i18n and build passed; browser 5 passed, 0 skipped |
| compatibility | `make test-isolation && make verify-assistant-runtime-dev && make validate-example-config` | exit 0; isolation 4 passed plus 2 credential skips in this exact uncredentialed invocation, AHR 33/77/10/98 plus golden passed, example config passed. The same isolation target was then executed with an ignored disposable local account and provider-free Stub: 6 passed, 0 skipped; both services were restored healthy with Stub=false |

The two skips in the exact compatibility invocation are not counted as passes.
The separate credentialed local execution supplies actual passing receipts for
those same two cases without using an external provider, while the other four
cases passed in the exact command.

## Supplemental and Durable Evidence

- Operations/governance/migration evidence:
  `reports/agent-studio/as-08-operations-governance.md`.
- Aggregate contract and 30 frozen source/test fingerprints:
  `reports/agent-studio/as-08-aggregate-manifest.json`.
- Browser/accessibility/redaction matrix:
  `reports/agent-studio/as-08-browser-matrix.md`.
- Screenshots: `reports/agent-studio/as-08-screenshots/`.
- Focused Ruff for the AS-08 Python paths passed, and `git diff --check`
  reported no whitespace errors.
- `python3 scripts/agent_studio_regression.py --validate-only` exited 0 and
  reported 39 required gates with the frozen manifest hash.
- All eight repository-owned local services were healthy after final hot
  update; Gateway and Assistant were explicitly verified with
  `ASSISTANT_E2E_STUB_LLM=false` after the credentialed black-box receipt.

## Security, Privacy, and Rollback

Audit dimensions are stored in explicit columns and recursive redaction occurs
before durable projection; metrics use explicit trace/span/feedback columns,
not free-form metadata. Viewer/Editor reads remain tenant-scoped and redacted;
audit and every policy, cache, credential or deletion mutation require
Owner/Admin authorization. Configured lower quotas are authoritative and
policy-storage failures fail closed. Legal hold is locked and rechecked before
destructive cleanup. Object deletion must finish before database cleanup can be
terminal, and every nonterminal retry increments a durable attempt receipt.
Completed tenant cleanup can remove the mutable ACL because migration 081
preserves last-owner checks for active Agents but exempts already-deleted Agent
history; a same-key replay returns the immutable receipt without incrementing
or re-running effects.

Disabling Agent Studio creation/publication/frontend entry points is an
application rollback only. Additive schema, immutable Versions, Publications,
audits and traces remain readable, and Assistant, Knowledge, Eval and Share
stay mounted. Schema repair is forward-only; no DROP/TRUNCATE/down migration,
volume/image reset, external provider credential or production mutation was
used.

## Evidence Boundary and Handoff

The browser suite uses deterministic API fixtures for UI and role evidence;
real PostgreSQL and authenticated local HTTP probes separately establish
persistence and authorization. This Phase does not claim production dashboard
wiring, external-model quality, public deployment or real-user deletion.

The first independent Critic returned C-01 through C-05. Iteration 2 closed
C-01/C-02/C-03/C-05 and most of C-04, then reproduced one remaining C-04-R2
boundary: a completed tenant deletion could not replay after ACL teardown. The
current snapshot resolves the terminal receipt before active-Agent
authorization, restricts it to the original requester/Tenant Admin, orders the
deleted state before ACL cleanup and updates the owner-invariant migration.
The exact operations gate remains `24 passed`; a focused real PostgreSQL test
and authenticated live HTTP replay both pass with unchanged `attempt_count=1`.
AS-F009 remains `failing` until a fresh independent Critic validates the 30
current fingerprints and this final narrow fix. Approval will permit Oracle and
harness writeback and unlock AS-09; only AS-09 may execute the immutable
39-gate aggregate and issue a terminal release decision.
