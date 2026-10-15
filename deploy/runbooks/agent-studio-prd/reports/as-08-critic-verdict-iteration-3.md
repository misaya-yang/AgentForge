# AS-08 Independent Critic Verdict

**Phase:** AS-08 — Observability, Admin, Data Governance, and Aggregate Gate  
**Feature:** AS-F009  
**Critic:** fresh independent Critic `/root/as08_critic_iteration3`  
**Critic Verdict:** approved  
**Actor Report:** `docs/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-report.md`  
**Date:** 2026-07-20

## Review Boundary

This iteration reviewed only the remaining iteration-2 blocker `C-04-R2` and
checked the 30 frozen fingerprints for material source drift. Closed findings
`C-01`, `C-02`, `C-03` and `C-05` were not reopened. No product source, test,
Oracle, runtime document or evidence artifact was modified.

The review covered repository `AGENTS.md`, the complete `prd-phase-harness`
skill and loop protocol, the fixed AS-08 Phase, iteration-2 verdict, latest
Actor/operations/aggregate evidence, migration 081, the public deletion route,
the repository prepare/finish deletion state machine and the updated isolated
PostgreSQL tenant-deletion test.

## Independent Verification

| Check | Exact command or inspection | Result |
| --- | --- | --- |
| Frozen source/test set | Independent SHA-256 recomputation of every `source_fingerprints` entry in `reports/agent-studio/as-08-aggregate-manifest.json` | exit 0; `30/30` matched; `0` missing; `0` mismatched |
| Focused completed-tenant replay | `uv run pytest -q --no-cov tests/database/test_agent_studio_migrations.py::test_tenant_deletion_disables_delivery_and_scrubs_mutable_state` | exit 0; `1 passed`, `0 failed`, `0 skipped` |
| Exact operations/governance/migration required gate | `uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/security/test_agent_data_governance.py tests/database/test_agent_studio_migrations.py` | exit 0; `24 passed`, `0 failed`, `0 skipped` |

Before PostgreSQL execution, all eight `ai-gateway-*` services were healthy and
each Compose `com.docker.compose.project.working_dir` label matched
`/Users/yang/projects/AI--Platfform`. The tests used disposable isolated schemas.

## C-04-R2 Disposition

`C-04-R2` is closed.

- `prepare_agent_data_deletion` resolves the matching durable receipt before
  active-Agent authorization. A terminal `completed` or `blocked` receipt is
  returned only when the authenticated caller is its original `requested_by`
  principal or has the tenant-admin context supplied by the API; a different
  user receives `AgentNotFoundError: AGENT_NOT_FOUND`.
- The public route returns any terminal prepared receipt immediately, so it
  does not call storage cleanup or `finish_agent_data_deletion` again.
- Tenant finalization marks the Agent deleted before removing `agent_members`.
  Migration 081 permits ACL removal only after `deleted_at` is set while its
  ordinary last-owner and deferred owner-invariant triggers still protect an
  active Agent. The exact required gate includes and passed the active-Agent
  last-owner negative test.
- The focused PostgreSQL node completed tenant deletion, replayed the same
  tenant/Agent/scope/idempotency key as the original requester, received the
  same deletion ID, `completed` status and `deleted_counts`, and kept
  `attempt_count=1`. It also proved another tenant user receives not-found and
  that the Agent is deleted with zero ACL rows while immutable Version history
  remains.
- Migration 081 makes terminal receipts immutable. Combined with the route's
  terminal short-circuit and unchanged replayed counts/attempt count, no
  storage or relational deletion effect is rerun on replay.

No material regression appeared in the frozen source set or the exact required
gate. The implementation stays within the fixed AS-08 repository, migration
and API boundaries.

## Verdict

`approved`. The only remaining iteration-2 blocker is reproducibly closed and
the affected required gate passes without failures or skips. This Critic
approves AS-08 / AS-F009 for the authorized Oracle and Harness writeback and
AS-09 handoff. AS-08 still does not issue the terminal whole-demand release
decision; that remains AS-09's responsibility.
