# AS-08 Independent Critic Verdict

**Phase:** AS-08 — Observability, Admin, Data Governance, and Aggregate Gate  
**Feature:** AS-F009  
**Critic:** fresh independent Critic `/root/as08_critic_iteration1`  
**Critic Verdict:** changes_requested  
**Actor Report:** `docs/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-report.md`  
**Date:** 2026-07-20

## Inputs Reviewed

- The complete `prd-phase-harness` skill and loop/handoff evidence protocol,
  repository `AGENTS.md`, the fixed AS-08 Phase contract and execution plan.
- Actor Phase report, operations/governance evidence, browser matrix, aggregate
  evidence manifest and all four durable screenshots.
- Current migration, Agent/Trace repositories, management/runtime APIs and
  schemas, analytics UI/tests, aggregate manifest/runner/contract test, Compose
  flags and the relevant inherited runtime quota/audit paths.
- Product requirements section 6.11 because the implementation and evidence
  disputed the promised metrics, quota and deletion behavior.
- Independent SHA-256 recomputation of all `source_fingerprints`: `23/23`
  matched current files; `0` missing and `0` mismatched.

## Independent Verification

| Check | Exact command | Result |
| --- | --- | --- |
| Frozen source/test set | Python SHA-256 comparison of every entry in `reports/agent-studio/as-08-aggregate-manifest.json` against the current file | exit 0; `23/23` matched |
| Aggregate manifest validation | `python3 scripts/agent_studio_regression.py --validate-only` | exit 0; 39 required gates; manifest SHA-256 `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d` |
| Aggregate contract | `uv run pytest -q --no-cov tests/contract/test_agent_studio_regression_manifest.py` | exit 0; `4 passed`, `0 failed`, `0 skipped` |
| Operations/governance/migration | `uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/security/test_agent_data_governance.py tests/database/test_agent_studio_migrations.py` | exit 0; `20 passed`, `0 failed`, `0 skipped` |

The full frontend lint/type/i18n/build/Playwright chain, AHR compatibility
chain, local migration status, live authenticated probe and credentialed
isolation cases are Actor receipts only; this Critic did not rerun them. Their
matching fingerprints establish which source they name, but do not convert
them into independent receipts.

## Findings

| ID | Severity | Requirement/gate | Finding | Reproduction and minimum correction boundary |
| --- | --- | --- | --- | --- |
| C-01 | high | R1; AS-OPS-001; actionable metrics | `get_agent_operations_summary` aggregates sessions, run success, total latency, tokens, cost and channel/version/publication breakdown, but omits required first-token latency even though `agent_traces.first_token_latency_ms` is explicit, and never joins Trace spans, Runtime feedback or retrieval evidence for tool success, KB hit and feedback metrics. The API fixture and browser cards omit the same dimensions, so the passing tests cannot prove AS-OPS-001. | Inspect `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py:2853-2918` against `database/migrations/060_agent_trace_eval.sql:28,80-96`, `database/migrations/079_agent_channel_runtime.sql:31-56` and `docs/agent-studio-prd/product-requirements.md:180`. Add explicit/queryable TTFT, tool success, KB hit and feedback aggregates with tenant/Agent/Version/Publication/channel/time filters; expose them in the typed API/UI and add real repository plus browser assertions. Do not substitute free-form metadata-only counts. |
| C-02 | high | R1; audit/redaction compliance; AS-OPS-002 | The migration trigger marks `redaction_state.sensitive_fields=removed` but does not remove or transform sensitive values in `request_summary` or `response_summary`. The migration test inserts `authorization: synthetic-secret` and asserts only the flag, so a raw secret remains durably stored while the row claims it was removed. In addition, generic `draft_update`/`version_create` events do not record individual MCP/Skill/KB binding actions, and the high-risk tool decision path is not projected into the dimensioned Agent audit stream. | `database/migrations/081_agent_studio_operations_governance.sql:34-58` only assigns dimensions and the flag; `tests/database/test_agent_studio_migrations.py:1636-1664` supplies the raw value without reading it back. Querying that inserted row's `request_summary->>'authorization'` reproduces the leak. `AgentRepository._audit` at `agent_repository.py:601-632` protects only its own caller path. Sanitize before durable write (and safely repair existing Agent audit summaries), assert the raw value is absent at rest and on read, and emit explicit dimensioned audit events for binding and high-risk policy decisions. |
| C-03 | high | R2; quota/abuse controls; fail-closed configured limits; AS-OPS-004 | The governance API persists `principal_*`, IP, Publication and alert-threshold values, but Runtime resolution returns only `agent_publications.policy`, and `_enforce_channel_limits` reads that Publication JSON. No production path reads `agent_governance_policies` to cap Runtime traffic, and `alert_threshold_percent` has no consumer. The promised Tenant Admin quotas for Agent count, channels, concurrency, tokens, MCP calls and storage are absent. A lower value saved in the analytics UI therefore does not constrain traffic: the configured control fails open even though the inherited AS-07 Redis limiter itself fails closed when unavailable. | Compare `agent_repository.py:4218-4385,4736-4895` with `src/api/v1/agent_runtime.py:700-756`; repository-wide lookup of the governance fields finds storage/schema/UI but no enforcement consumer. Add tenant/Agent governance caps as hard upper bounds in the authoritative create/publish/runtime/MCP/storage paths, stable over-limit errors with recovery guidance and threshold/abuse evidence. Add negative tests proving a saved lower cap is enforced across workers and that policy lookup/backend failures fail closed. |
| C-04 | critical | R2; deletion/revocation/legal hold; AS-OPS-005 | The deletion state machine violates three non-waivable claims. First, `finish_agent_data_deletion` does not re-read legal hold, so a request prepared before hold activation can still delete under an active hold. Second, storage failure sets terminal `failed`; the migration guard makes failed rows immutable and `prepare` returns the existing failed row, so the Actor's “durable retryable” claim is false and partial object cleanup can become unrecoverable. Third, tenant scope only revokes API tokens: it leaves Hosted/Embed Publications active, leaves MCP/Connector grants, Draft/ACL/Publication state and can leave long-term Agent memory whose derived principal is not discoverable from a remaining session. | Follow `agent_repository.py:5155-5206,5278-5608` and the terminal guard at `081_agent_studio_operations_governance.sql:165-194`. A deterministic test can prepare while hold=false, enable hold, then finish and observe completion; a failing object deletion followed by the same idempotency key remains terminal failed; a tenant-scope seed retains an active Hosted Publication and grants. Make cleanup retryable/idempotent, recheck and lock legal hold at finalization, disable/revoke every runtime path before cleanup, and add a complete per-scope matrix covering Draft/ACL/Publication, all tokens/grants, sessions/runs/memory, feedback/idempotency, attachments/objects, caches/derived indexes and preserved minimum audit/history. |
| C-05 | high | R4; strict completion gate | The aggregate runner treats exit code zero as a passed gate and has no structured skip policy. The Actor's exact `make test-isolation` receipt already demonstrates `2 skipped` while returning zero; a default `make verify-agent-studio` can therefore report all 39 gates passed without all required tests executing, contrary to the fixed completion rule. The later separate 2/2 credentialed run does not repair the aggregate runner's same-run acceptance logic. | `scripts/agent_studio_regression.py:95-136,170-200` records only process status; the Actor report's compatibility row records the two skips. Make the release aggregate produce/consume structured test receipts and fail on required skips, or run a release-strict isolation target that fails when credentials/runtime prerequisites are absent. Add a contract test proving a zero-exit skipped required test makes the aggregate result fail. |

## Requirement Coverage

- **R1 Operable and Auditable Agents:** `changes_requested`. Explicit Agent,
  Version, Publication, channel and time predicates, pagination and response
  redaction exist, but the required operational metric set is incomplete and
  the database audit projection can falsely label raw sensitive material as
  removed. Binding and high-risk decision audit coverage is also incomplete.
- **R2 Quota and Data Governance:** `changes_requested`. The inherited Redis
  Publication limiter remains fail-closed, and the focused legal-hold/storage
  tests pass, but the new admin quota values are not authoritative, legal hold
  has a finalization race, failed cleanup is not retryable, and tenant/user
  cleanup and revocation are incomplete.
- **R3 Safe Migration and Compatibility:** partially supported. Migration 081
  is additive/reentrant in the independent 20-test run, and separate
  runtime/management/publish/frontend flags exist. The audit-at-rest defect is
  within that migration, and frontend/AHR/live compatibility remain Actor-only
  receipts. No destructive rollback was found.
- **R4 Versioned Aggregate Gate:** manifest completeness is independently
  supported: exact Phase-derived equality covers all 39 required AS-00 through
  AS-08 commands and the named critical gates, and source-stability excludes
  generated reports while retaining source/tests. Release acceptance is still
  blocked because required skips can be counted as passed.

## Security, Privacy, and Failure Assessment

Tenant/Agent authorization predicates and explicit trace/audit dimensions are
present, and the API redacts trace previews. Those controls do not compensate
for a durable audit payload that can retain a raw secret while claiming it was
removed, configured quota values that are not enforced, or deletion that can
bypass a newly active legal hold. These are privacy, abuse-cost and data-loss
boundaries explicitly marked non-waivable by AS-08.

## Minimal-Change Assessment

The frozen 23-file AS-08 set is internally stable and its additions are mostly
within the fixed Phase paths. The overall worktree contains extensive inherited
AS-00 through AS-07 and open-source packaging changes without a clean AS-08-only
commit baseline, so this Critic does not attribute every dirty file to AS-08.
The Critic modified no product source, test, Oracle, loop state, handoff or
evidence artifact; only this verdict and its identical canonical copy.

## Rollback and Handoff Assessment

The runtime, management, publish and frontend flags provide an application
rollback surface while retaining additive schema and existing Assistant
routes. Actor compatibility receipts report normal Assistant behavior and
Stub=false restoration. AS-09 must remain locked: deletion/legal-hold and quota
controls are not safe to hand off, and the aggregate cannot yet enforce the
user's zero-skip completion boundary.

## Whole-Demand Regression Assessment

AS-09 same-build whole-demand regression has not run and is not claimed. The
manifest is complete, but AS-08 must close C-01 through C-05, rerun affected
required gates on a new frozen source set, and obtain a fresh independent
Critic before the terminal aggregate can provide valid release evidence.

## Verdict Rationale

`changes_requested`. The independent commands confirm that the reported 20
focused tests, 4 manifest tests, 39-entry manifest and 23 fingerprints are real.
They also expose shallow assertions that miss required product behavior. Raw
audit material, disconnected quota settings, incomplete/non-retryable deletion
with a legal-hold race, missing operations metrics and skip-blind aggregate
acceptance each violate a non-waivable AS-08 or terminal completion gate.
