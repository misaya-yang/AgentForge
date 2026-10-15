# AS-02 Independent Critic Verdict - Iteration 2

**Phase:** AS-02 - Runtime Resolver and Isolation

**Feature:** AS-F003

**Critic:** independent fresh context reviewer

**Critic Verdict:** changes_requested

**Actor Report:** `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`

**Prior Critic History:** `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-1.md`

**Date:** 2026-07-18

## Verdict Summary

AS-02 is not approved. Actor iteration 2 materially corrected the first
Critic's C-01 session checks, C-02 persisted identity constraints, C-03 exact
Skill subset, and C-04 evidence accounting. All five exact Phase-required
commands passed in this review with no skips, migrations 072/073 passed their
five PostgreSQL tests, the golden receipt and all 12 current evidence nodes
collected, and the authorized local runtime was restored healthy with
`stub=false`, provider inputs absent, and Preview failing closed as
`503 / AGENT_RUNTIME_MODEL_UNAVAILABLE`.

Those positive results do not cover two independently reproduced material
failures:

1. the C-01 correction protects start/resume comparisons, but a conflicting
   session's failed `start_run` can still drive the Agent loop's unconditional
   `finally` path and mark the correct session's existing run failed; and
2. Gateway capability intersection accepts all mutable fields from a resolver
   result when only its type/ID matches a Version binding, allowing the resolver
   to lower a high-risk binding to low risk and inject version/config so it
   survives `high_risk_tools=false`.

These violate R3 and R2 respectively. AS-F003 must remain `failing`, AS-03 and
AS-04 remain locked, and the phase completion gate must not run on this verdict.

## Inputs Reviewed

- Phase contract:
  `docs/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md`
- Oracle item: AS-F003 only in
  `docs/agent-studio-prd/feature-oracle.json`
- Actor evidence:
  `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`
- First-Critic history:
  `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-1.md`
- Critic template:
  `docs/agent-studio-prd/reports/critic-verdict-template.md`
- Disputed architecture boundaries: sections 3.3 and 4.1 of
  `docs/agent-studio-prd/architecture-contract.md`
- Actual AS-02 runtime diff/source/tests, including canonical Snapshot/Envelope,
  Gateway resolver and public schemas, Assistant verification/prompt/tool/memory,
  run/checkpoint/session/trace persistence, migrations 072/073, Skill registry
  and bridge, and phase-named negative tests
- Durable eval receipt:
  `reports/agent-studio/as-02-golden-results.json`
- Repository-owned Compose runtime: owner labels, eight-service health, memory,
  CORS validity, stub/provider state, live isolation transport, and final no-key
  Preview response

The AS-01 direct dependency Critic verdict is `approved`. No unrelated phase,
full product document, or terminal whole-demand artifact was used as completion
evidence.

## Iteration-1 Finding Re-evaluation

| Prior ID | Iteration-2 result | Independent evidence |
|---|---|---|
| C-01 session-bound resume | partially corrected; residual high finding remains | `prepare_run_resume` now requires current session for Agent runs and compares it to both persisted run and checkpoint. The in-memory and SQL `start_run` conflict guards include session, and migration 073 adds the composite run/checkpoint session FK. However, the conflicting-start finalization reproduction below still lets the wrong session terminate the correct run. |
| C-02 persisted identity | corrected | Forward-only migration 073 requires Preview revision >= 1; hosted/embed/api rows require Version + Publication and no Draft; composite FKs bind Version and Publication to the same Agent and Version; checkpoint scope includes run/tenant/user/session. Five migration tests passed and 071/072/073 are applied with no pending migration. |
| C-03 exact Skill subset | corrected for the reviewed contract | `allowed_skill_ids` retains exact signed/policy-approved names; DB loading uses `name = ANY(...)`; list/selector/prompt material and bridge registration receive the exact subset; invocation receives normalized names from the same `skill_tool_name` function. The two-Skill negative test passed. `None` defaults still preserve built-in Assistant all-Skill/legacy behavior. |
| C-04 cleanup accounting | corrected | Current worktree has exactly 115 modified tracked files under Assistant `core` plus new `agent/runtime_context.py`. Excluding the original five semantic tracked files, an independent HEAD-to-Ruff comparison reproduced exactly 66/110 safe-fix matches, 10/110 unsafe-fix matches, and 34/110 manual/semantic files. `tool_bridge.py` is the iteration-2 semantic addition; `execution_gateway.py` and `runtime_memory.py` are correctly identified as substantive. |

## Findings

### C-01 - Conflicting session can still terminate the correct persisted run

**Severity:** high

**Requirements:** R3; AS-F003 step 2; session/run/checkpoint isolation

**Files:**
`apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`,
`apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`

Iteration 2 correctly added the expected/current `session_id` to resume checks,
compared it against both run and checkpoint, added session to the in-memory and
SQL start conflict guards, and added a session-scoped checkpoint FK. The
remaining failure is on finalization:

- `AgentLoop` enters its `try`, calls `start_run`, catches a conflicting-session
  `PermissionError`, and still enters `finally`.
- `finally` unconditionally calls `finish_run` and then saves a terminal
  checkpoint even though this invocation never acquired the run.
- `finish_run` validates only `run_id + tenant_id + user_id`; its in-memory
  path mutates the matching record directly, and its SQL `UPDATE` does not
  include `session_id` or Agent dimensions.

An independent no-database reproduction created the same run for `session-a`,
attempted the same tenant/user/Agent/Version run ID from `session-b`, caught the
expected conflict, and then exercised the exact finalization call made by the
Agent loop. The result was:

```json
{
  "conflicting_start_rejected": true,
  "persisted_session_id": "session-a",
  "persisted_status_after_conflicting_finally": "failed",
  "persisted_error_after_conflicting_finally": "conflicting caller"
}
```

The SQL path has the same scope omission. The new checkpoint FK can reject a
wrong-session DB checkpoint, but it does not undo the earlier status update and
does not protect the in-memory mirror.

**Required correction:** only finalize/checkpoint a run after this invocation
successfully acquired/started that exact run; bind `finish_run` to session and
Agent runtime dimensions in both memory and SQL; and add an Agent-loop-level
negative test proving a same-tenant/user/Agent/Version `session-b` conflict
cannot change the status, error, usage, terminal time, or checkpoints of
`session-a`.

### C-05 - Capability resolver can mutate a bound capability and bypass channel risk policy

**Severity:** high

**Requirements:** R2; AS-F003 step 1; architecture contract 4.1

**File:** `src/api/v1/agent_runtime.py`

`_effective_capabilities` correctly rejects resolver-returned type/ID pairs not
present in the immutable Version bindings. For matching pairs, however, it
appends the resolver's entire `raw` dictionary rather than materializing the
authorized item from the original binding. The resolver can therefore replace
`risk`, `resource_version`, `schema_hash`, and `config` while retaining the same
type and ID.

An independent no-write reproduction used the existing immutable
`mcp-danger` binding with `risk=high` and a Publication policy of
`high_risk_tools=false`. The resolver returned the same type/ID with
`risk=low`, a forged version, and injected config. Gateway accepted it into the
Snapshot:

```json
{
  "channel_high_risk_tools": false,
  "accepted_capability": {
    "id": "mcp-danger",
    "risk": "low",
    "version": "resolver-forged-version",
    "config": {"resolver_injected": true}
  }
}
```

This is expansion, not set intersection: a high-risk Version binding that the
channel must deny becomes executable Snapshot material. The existing forged-ID
test does not cover same-ID metadata substitution.

**Required correction:** treat resolver output as authorization keys/policy
reasons only and build the Snapshot capability from the immutable original
binding; do not permit risk lowering or version/schema/config replacement.
Add negative tests for same-ID risk downgrade and version/schema/config
mutation, including `high_risk_tools=false`.

## Requirement Coverage

| Requirement | Assessment | Evidence |
|---|---|---|
| R1 authorized deterministic resolver | supported | Preview/Published schemas are closed; generic Assistant reserved Agent fields/headers are rejected; Gateway resolves before signing; signature covers the complete canonical Envelope; Assistant recalculates Snapshot/body hashes, checks tenant/caller/Agent/Version-or-Draft/Publication/channel/session/spec/time/nonce, and atomically consumes replay state fail-closed. Envelope suite: 26/26. |
| R2 layered prompt and non-expanding capability/KB boundary | changes requested | Prompt precedence, exact Skill subset, Dataset intersection, selector/invoker/cache upper bounds and policy uncertainty are otherwise supported. C-05 proves Gateway can expand a same-ID capability's security metadata before those downstream bounds are formed. |
| R3 session and evidence pinning | changes requested | Session/version/publication pins, explicit trace columns, resume session comparisons, migration 073 and checkpoint FK are supported. C-01 proves finalization is not scoped to the session that acquired the run. |
| R4 built-in Assistant compatibility | supported | `agent_runtime=None`, `capability_allowlist=None`, and `allowed_skill_ids=None` preserve the legacy path. AHR, persistence/trace/golden, live nonstream/SSE isolation and adjacent API regressions passed. |

## Required Validation Evidence

All five exact commands from the Phase file were independently executed on the
reviewed source. None failed or skipped.

| Gate | Exact command | Exit/result |
|---|---|---|
| gateway-envelope | `uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py` | exit 0; 26 passed; 0 skipped |
| resolver-isolation | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py tests/services/assistant/test_agent_capability_allowlist.py` | exit 0; 23 passed; 0 skipped |
| trace-session | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | exit 0; 43 passed; 0 skipped |
| runtime-gate | `make verify-assistant-runtime-dev && make test-isolation` | exit 0; AHR-01 28, AHR-02 77, AHR-03 8, AHR-04 96, golden gate pass; live isolation 6 passed; 0 skipped |
| lint | `uv run ruff check packages/ai-gateway-core/src/ai_gateway_core/agents src/api/v1/assistant.py src/api/v1/_assistant_proxy.py src/api/v1/agent_runtime.py src/api/schemas/agent_runtime.py apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py tests/api/test_agent_runtime_envelope.py tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py` | exit 0; `All checks passed!` |

Passing required gates demonstrate substantial positive behavior but do not
exercise C-01's conflicting-start/finally sequence or C-05's same-ID metadata
substitution. They therefore cannot override either reproduction.

## Additional Independent Evidence

| Check | Result | Boundary |
|---|---|---|
| `uv run pytest -q --no-cov tests/database/test_agent_runtime_migration.py` | exit 0; 5 passed; 0 skipped | Independently covers positive additive dimensions and the iteration-2 negative shape/relationship/session constraints. |
| `bash scripts/new/migrate.sh --status` | exit 0; 69 applied including 071/072/073; 0 rollback records; no pending migration | Local authorized dev database only; no production migration claim. |
| Golden JSON structural/case validation | exit 0; six required cases, all passed, `provider_calls=0` | Offline deterministic receipt, not provider quality proof. |
| Golden `evidence_tests` collection | exit 0; all 12 current node IDs collected | The preserved iteration-1 artifact says 11; the current JSON actually contains 12. |
| `uv run --all-packages pytest -q --no-cov tests/assistant/docgen` | exit 0; 135 passed | Adjacent regression for broad Docgen/manual Ruff changes; not automatic proof of zero semantic drift. |
| `uv run pytest -q --no-cov tests/api/test_gateway_capability_matrix.py tests/contract/test_find_active_command.py tests/contract/test_migrated_routes_equivalence.py` | exit 0; 45 passed | Adjacent resume/API regression; it does not exercise C-01's wrong-session finalization. |
| Deterministic C-04 comparison | 115 tracked + one new file; outside original five: 66 safe, 10 unsafe, 34 manual/semantic | Reproduced from HEAD with the current Ruff version. Manual review, not counts alone, identified C-01 in `execution_gateway.py`. |
| `git diff --check` | exit 0 | No whitespace errors in the current worktree diff. |
| `docker compose config --quiet` | exit 0 | Base Compose renders with the local generated environment. |

## Discarded or Skipped Runs

- No Phase-required command failed or skipped in this Critic iteration.
- The Actor report's earlier skipped/failed runtime attempts are historical
  Actor evidence and were not counted as Critic passes.
- The first C-04 diagnostic wrapper was discarded because a zsh loop variable
  named `path` shadowed zsh's `PATH` array. It touched only a temporary audit
  directory. The corrected comparison was rerun from HEAD and produced the
  66/10/34 result above.
- An initial non-required CORS diagnostic queried a nonexistent
  `BACKEND_CORS_ORIGINS` variable and was discarded. The actual
  `ASSISTANT_CORS__ALLOW_ORIGINS` and `KNOWLEDGE_CORS__ALLOW_ORIGINS` JSON values
  parsed successfully before runtime validation.

Neither discarded diagnostic was represented as passing evidence and neither
changed product files, tests, runtime data, or the worktree.

## Security, Privacy, and Failure Assessment

- Canonical Snapshot/Envelope integrity, closed external schemas, caller/session
  binding, replay fail-closed behavior, secret-shaped Snapshot rejection and
  public SSE/trace redaction are supported by code inspection and executed
  tests.
- Prompt layers remain platform > Agent > channel > capability > allowed
  memory/RAG > conversation/external data.
- Tool and Dataset denial occurs before cache or executor access; the exact
  Skill correction now preserves one bound Skill through load, selection,
  prompt material, bridge and invocation while excluding the second Skill.
- C-05 remains before those enforcement points: it lets Gateway form an
  expanded signed upper bound, so Assistant faithfully enforcing the resulting
  Snapshot cannot repair it.
- C-01 permits cross-session integrity damage to run status/evidence even though
  the corrected resume and database relation checks themselves deny the wrong
  session.
- No API key, password, token, shared secret, Snapshot, protected Prompt, or
  credential value was printed or written by this review. No live provider call
  was made or authorized.

## Minimal-Change Assessment

The AS-02 resolver, Envelope, session, trace, migration, Skill subset and
readiness changes are phase-relevant. The live-discovered expansion into
`src/main.py` and `src/services/llm/gateway_model_meta.py` is justified by the
fail-closed provider-readiness contract. Iteration 2 also correctly reclassifies
`tool_bridge.py`, `execution_gateway.py`, and `runtime_memory.py` as semantic.

The 115-file core cleanup is broader than the feature slice. Its current
66/10/34 accounting is accurate, and Docgen 135 plus resume/API 45 reduce but
do not eliminate semantic-drift risk. The green suites are not treated as
automatic proof; the manual review found C-01 within the 34-file group. Other
separately authorized open-source packaging and pre-existing dirty work were
preserved and are not attributed to AS-02 completion.

This Critic changed only
`docs/agent-studio-prd/reports/as-02-critic-verdict.md`.

## Runtime Restore and Rollback Assessment

Before mutation, the repository Compose runtime was 8/8 healthy, all running
containers were owned by `/Users/yang/projects/AI--Platfform`, Gateway and
Assistant were `stub=false`, reviewed provider inputs were absent, and sampled
stack memory was approximately 714 MiB.

For the required live isolation gate, only Assistant and Gateway were serially
recreated from the existing base images with `COMPOSE_PARALLEL_LIMIT=1`,
`ASSISTANT_E2E_STUB_LLM=true`, explicit valid CORS JSON, and provider inputs
blank. Current host `src/.` plus the complete `ai_gateway_core/.` were copied to
Gateway; current `assistant_service/.` plus complete `ai_gateway_core/.` were
copied to Assistant. Both were restarted and healthy before the 6/6 isolation
run. Sampled stack memory remained approximately 719 MiB, far below 3.5 GiB.

Afterward, both services were serially recreated with `stub=false` and provider
inputs still blank, the same current source trees were copied again, and both
were restarted. Final verification showed:

- all eight services running and healthy;
- every running service owner label equals
  `/Users/yang/projects/AI--Platfform`;
- Gateway and Assistant `stub=false`;
- reviewed provider API-key inputs absent;
- valid Assistant CORS JSON;
- sampled stack memory approximately 720 MiB; and
- secure login/list probes succeeded, two existing `AS02 Live` candidates were
  found, and Preview Session returned
  `503 / AGENT_RUNTIME_MODEL_UNAVAILABLE`.

No image build, prune, volume/image/data deletion, destructive rollback,
deployment, real-provider call, commit, or push occurred. The feature flag is a
tested application containment path; migrations are additive/forward hardening
and were not destructively rolled back.

## Rollback and Handoff Decision

Migration 073 is a valid forward-only correction to C-02, and disabling Agent
runtime preserves the legacy built-in Assistant path. These do not waive C-01
or C-05. Required next work is limited to the two findings above, their focused
negative tests, regeneration of affected evidence if node IDs change, all five
exact AS-02 gates, and another independent fresh-context Critic review.

AS-F003 remains `failing`. AS-03 and AS-04 remain locked. The orchestrator must
not link this verdict as approval or run the phase completion gate.

## Whole-Demand Regression Assessment

AS-02 is not the terminal phase. Whole-demand same-build regression remains an
AS-09 responsibility. This verdict evaluates only AS-F003 and inherited
Assistant/runtime regressions; it makes no full-product, deployment, provider
quality, or production-readiness claim.
