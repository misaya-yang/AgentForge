# AS-08 Independent Critic Verdict

**Phase:** AS-08 — Observability, Admin, Data Governance, and Aggregate Gate  
**Feature:** AS-F009  
**Critic:** fresh independent Critic `/root/as08_critic_iteration2`  
**Critic Verdict:** changes_requested  
**Actor Report:** `docs/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-report.md`  
**Date:** 2026-07-20

## Inputs Reviewed

- The complete `prd-phase-harness` skill and loop/handoff protocol, repository
  `AGENTS.md`, fixed AS-08 Phase contract and fixed execution plan.
- Product requirements section 6.11, iteration-1 Critic C-01 through C-05,
  current Actor report, operations/governance evidence, browser matrix and
  aggregate evidence manifest.
- Current migration 081, Agent/Trace repositories, management/runtime APIs,
  Assistant high-risk tool audit path, analytics UI/types, aggregate runner,
  manifest contract and the tests intended to falsify each original blocker.
- Independent SHA-256 recomputation of every frozen source fingerprint:
  `30/30` matched; `0` missing and `0` mismatched.

## Independent Verification

| Check | Exact command | Result |
| --- | --- | --- |
| Frozen source/test set | SHA-256 comparison of all entries in `reports/agent-studio/as-08-aggregate-manifest.json` with current files | exit 0; `30/30` matched |
| Aggregate manifest validation | `python3 scripts/agent_studio_regression.py --validate-only` | exit 0; 39 required gates; manifest SHA-256 `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d` |
| Aggregate contract | `uv run pytest -q --no-cov tests/contract/test_agent_studio_regression_manifest.py` | exit 0; `5 passed`, `0 failed`, `0 skipped` |
| Operations/governance/migration required gate | `uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/security/test_agent_data_governance.py tests/database/test_agent_studio_migrations.py` | exit 0; `24 passed`, `0 failed`, `0 skipped` |
| C-02 through C-05 negative spot checks | Nine selected real-PostgreSQL, Runtime API, Assistant tool-audit and aggregate-skip node IDs | exit 0; `9 passed`, `0 failed`, `0 skipped` |
| Completed tenant-deletion replay | Disposable isolated operations schema: prepare tenant deletion, finish it, then prepare again with the same tenant/Agent/scope/idempotency key | first call `completed`, `attempt_count=1`; replay raised `AgentNotFoundError: AGENT_NOT_FOUND` |

Before the database checks, all eight local services were healthy and every
Compose `com.docker.compose.project.working_dir` label matched this repository.
The full frontend lint/type/i18n/build/Playwright chain and compatibility chain
remain Actor receipts; this Critic did not rerun them. Their 30 matching
fingerprints establish source identity, not independent execution.

## Iteration-1 Blocker Disposition

- **C-01 closed.** The operations summary now derives TTFT, tool success,
  Knowledge hit, feedback, latency, token/cost and channel distribution from
  explicitly filtered Trace/span/feedback columns. Typed API/UI assertions
  cover the added fields.
- **C-02 closed.** Migration 081 recursively redacts request/response JSON
  before durable storage and repairs existing Agent audit summaries. Explicit
  MCP/Connector/Skill/Knowledge binding events and argument-free high-risk
  tool-policy decisions are dimensioned; high-risk execution fails closed when
  its audit write is unavailable. The selected negative checks passed.
- **C-03 closed.** Saved governance limits are consumed by authoritative
  create/publish/runtime/MCP/storage paths, cross-worker usage comes from
  PostgreSQL plus the existing Redis limiter, lookup failure fails closed, and
  stable quota errors include recovery guidance. The database threshold/cap
  and Runtime lower-limit/fail-closed checks passed.
- **C-04 partially closed, still blocking.** Legal hold is rechecked under
  lock, failed object cleanup is retryable on the same receipt, and the tested
  tenant cleanup disables delivery and removes mutable database state. A
  completed tenant deletion, however, cannot replay its durable receipt.
- **C-05 closed.** A required gate that exits zero while reporting a non-zero
  numeric skip summary is marked failed; the contract negative test passed.

## Blocking Finding

| ID | Severity | Requirement/gate | Finding | Reproduction and minimum correction boundary |
| --- | --- | --- | --- | --- |
| C-04-R2 | high | R2 stable deletion outcomes; AS-OPS-005; fixed plan's durable idempotent deletion receipt | A successful `tenant` cleanup deletes every Agent ACL row and sets `agents.deleted_at`, but a retry of the public deletion endpoint always calls `prepare_agent_data_deletion` first. That method executes `_authorized_agent` before looking up the existing receipt; `_authorized_agent` requires `deleted_at IS NULL` and a surviving membership. Consequently a legitimate retry after a lost success response returns `AGENT_NOT_FOUND` instead of the immutable `completed` receipt. The fresh isolated-PostgreSQL reproduction produced `first_status=completed attempt_count=1`, then `replay_exception=AgentNotFoundError code=AGENT_NOT_FOUND`. | Production path: `src/api/v1/agents.py:947-957`; authorization-before-receipt lookup: `agent_repository.py:5551-5573`; active-only predicate: `agent_repository.py:574-595`; terminal cleanup removes ACL and marks the Agent deleted at `agent_repository.py:6166-6187`. Resolve an existing matching receipt before active-Agent authorization, while authorizing replay only for the original `requested_by` or a current Tenant Admin and preserving not-found behavior for other principals. Return the exact terminal receipt without re-running storage/database effects. Add a real-PostgreSQL/API negative test that completes tenant deletion, retries the same idempotency key, receives the same `completed` deletion ID/counts with unchanged `attempt_count`, and proves another user cannot read it. |

This is not a formatting or optional robustness request. Network loss after the
database commit is an ordinary retry boundary; returning 404 makes a completed
privacy operation externally ambiguous and contradicts the Actor's durable,
idempotent receipt claim.

## Requirement Coverage

- **R1 Operable and Auditable Agents:** supported by current implementation,
  the 24-test required gate and the focused audit negative checks.
- **R2 Quota and Data Governance:** `changes_requested`. Quotas, legal-hold
  finalization, storage retry and cleanup coverage are materially improved, but
  terminal tenant-deletion replay is not stable or idempotent.
- **R3 Safe Migration and Compatibility:** supported within the independently
  rerun migration gate and Actor compatibility evidence boundary. Migration
  081 remained reentrant/additive; no destructive rollback was found.
- **R4 Versioned Aggregate Gate:** supported. All 39 required Phase commands,
  named non-omission gates, 30 frozen fingerprints and zero-exit skip rejection
  are intact.

## Minimal-Change, Rollback, and Evidence Assessment

The remediation remains within the fixed AS-08 architecture: existing
repositories, Gateway/Assistant boundaries, additive migration and versioned
aggregate runner. No unrelated architecture drift was found. This Critic
modified no product source, tests, Oracle, loop state, handoff or Actor
evidence; only this verdict and its identical canonical copy.

Application flags still provide a non-destructive rollback surface and the
Actor receipts report built-in Assistant compatibility. Production monitoring,
external-provider quality, public deployment and real-user deletion remain
unclaimed. The isolated replay reproduction proves only the local PostgreSQL
repository/API state machine and is sufficient to falsify the durable receipt
claim.

## Verdict Rationale and Handoff

`changes_requested`. C-01, C-02, C-03 and C-05 are closed, and most of C-04 is
closed, but the exact completed-tenant-deletion retry fails on the frozen
source. AS-09 must remain locked. After the narrow receipt-replay fix and a
real negative regression test pass, rerun the affected operations/governance
gate, refresh the frozen fingerprints/evidence and request a fresh independent
Critic. AS-08 still does not issue the terminal whole-demand release decision.
