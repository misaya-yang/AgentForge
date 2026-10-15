# AS-02 Critic Verdict

**Phase:** AS-02 - Runtime Resolver and Isolation

**Feature:** AS-F003

**Critic:** independent fresh context reviewer

**Critic Verdict:** changes_requested

**Actor Report:** `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`

**Date:** 2026-07-18

## Verdict Summary

AS-02 is not approved. All five prescribed validation commands passed without skips, the golden receipt is structurally valid, and the local runtime was restored healthy after an authorized stub-only isolation run. Those positive results do not cover three material acceptance gaps found by independent code review and focused reproduction:

1. run-resume authorization does not bind the current session to the persisted run and checkpoint;
2. migration `072_agent_runtime_dimensions.sql` accepts incomplete published-channel identity shapes and does not enforce same-Agent relationships among Agent, version, and publication dimensions; and
3. enabling one resolved skill allows downstream selection and prompt injection from other enabled tenant/user skills outside the resolved capability set.

These gaps affect R2 and R3 directly. AS-F003 must remain failing until they are corrected, covered by negative tests, and independently re-reviewed. AS-03 and AS-04 remain locked, and the phase completion gate must not run on this verdict.

## Inputs Reviewed

- `docs/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md`
- `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`
- `docs/agent-studio-prd/reports/critic-verdict-template.md`
- AS-F003 in `docs/agent-studio-prd/feature-oracle.json`
- `docs/agent-studio-prd/next-window-prompt.md`
- the complete AS-02 worktree diff and relevant source, migration, test, and golden-result files
- `reports/agent-studio/as-02-golden-results.json`
- the repository-owned local Compose runtime, including ownership labels, health state, stub/provider state, and the no-key Preview Session response

## Requirement Coverage

| Requirement | Assessment | Evidence |
|---|---|---|
| R1: Gateway is the sole resolver; external schemas are closed; Snapshot/Envelope is canonical and signed | supported | Closed schema/key checks, canonical serialization, whole-envelope signature and hash verification, identity/time validation, replay fail-closed behavior, authorized Preview/publication resolution, and pre-execution verification were inspected. Envelope/API tests passed. |
| R2: prompt, capability, and knowledge scope can only narrow downstream | changes requested | Tool and dataset enforcement are deny-first, and prompt precedence is explicit. However, the skills path converts an exact resolved skill list into a boolean and then selects/injects from all enabled registry skills. See C-03. |
| R3: session/evidence pinning | changes requested | Session binding, trace roots, and Agent-dimension evidence exist, but resume does not assert the current session and the migration accepts incomplete/mismatched persisted identity shapes. See C-01 and C-02. |
| R4: legacy compatibility | supported | Required assistant runtime, persistence, trace, golden, and compatibility suites passed. No legacy bypass was found in the reviewed paths. |

## Findings

### C-01 - Run resume is not pinned to the current session

**Severity:** high

**Requirements:** R3; AS-F003 oracle step 2

**Files:** `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`, `tests/services/assistant/test_agent_trace_capture.py`

`AssistantExecutionGateway.prepare_run_resume` accepts `run_id`, tenant, user, approval, and Agent runtime dimensions, but it accepts no expected/current `session_id`. `_agent_dimensions` and `_agent_dimensions_match` likewise omit the session. `start_run` persists a session, yet the database conflict guard compares tenant, user, and Agent dimensions without comparing the session.

A focused in-memory reproduction started a run and checkpoint for `session-a`, then called `prepare_run_resume` with the same tenant, user, and Agent dimensions. It returned:

```text
{'status': 'ready', 'reason': None, 'checkpoint_session_id': 'session-a', 'resume_api_accepts_session_id': False}
```

The existing pinning test changes Agent/version dimensions; it does not exercise two sessions using the same Agent Version. Consequently, a run/checkpoint can be declared resumable without proving that the requesting session owns it.

**Required correction:** thread the expected/current `session_id` through resume preparation; compare it with both the persisted run and checkpoint; include it in persistence conflict guards; and add negative tests for `session-a` versus `session-b` with otherwise identical tenant, user, Agent, version/publication, channel, and runtime/spec dimensions.

### C-02 - Migration 072 permits incomplete and relationally inconsistent Agent identity

**Severity:** high

**Requirements:** R3; AS-F003 oracle steps 2 and 3

**File:** `database/migrations/072_agent_runtime_dimensions.sql`

The migration's shape constraint requires Preview rows to have a draft and no version/publication. For every non-Preview channel it requires only `agent_version_id IS NOT NULL`; it does not require `publication_id IS NOT NULL` or `agent_draft_revision IS NULL`. Its separate tenant-scoped foreign keys also prove only that each referenced row exists in the tenant, not that the version and publication belong to the persisted `agent_id`. Migration 071 exposes composite identities that can support stronger relational enforcement, but migration 072 does not use them.

A rolled-back database transaction selected a valid local Agent/version and inserted an `assistant_runs` row with channel `api`, a valid Agent/version, `publication_id = NULL`, and `agent_draft_revision = 999`. The insert was accepted:

```text
{'published_shape_without_publication_and_with_draft_was_accepted': True}
```

The current migration tests cover additive columns, a cross-tenant foreign key, revocation, and publication resolver pinning/revocation. They do not reject an incomplete published identity or same-tenant cross-Agent relationships.

**Required correction:** tighten the published-channel shape to require the complete Version/publication identity and exclude draft identity; enforce valid positive Preview revisions; enforce same-Agent relationships among Agent, version, and publication using appropriate composite constraints/foreign keys; and add negative migration tests for incomplete and same-tenant mismatched identities.

### C-03 - Skill selection can expand beyond the resolved capability set

**Severity:** medium

**Requirement:** R2; AS-F003 oracle step 1

**Files:** `apps/assistant-service/src/assistant_service/api/routes/chat.py`, `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`, `packages/ai-gateway-core/src/ai_gateway_core/skills/registry.py`

`_build_agent_runtime_config` reduces the signed capability list to `skills_enabled = any(capability.type == "skill")`; it does not retain an exact allowed skill-ID set. When that boolean is true, the Agent loop loads all active tenant/user skills, registers all enabled skills through the bridge, calls `select_for_query` across the whole registry, and injects selected skill metadata/instructions into the dynamic user context. `SkillRegistry.select_for_query` has no resolved allowlist argument.

Tool invocation later applies the resolved function allowlist, so an unbound skill tool call is denied. That does not undo the earlier expansion of skill selection and prompt context. The phase requires every downstream selector to preserve or reduce the signed capability set.

**Required correction:** carry the exact resolved skill capability identifiers/tool names into runtime configuration; restrict registry loading/selection, bridge registration, metadata, and instruction injection to that set; and add a two-skill test where only one skill is bound and the other can neither influence the prompt nor become callable.

### C-04 - The actor report overstates the mechanical-cleanup classification

**Severity:** low

**Concern:** evidence accuracy and minimal-change accounting

The actor report groups 114 changed files under `apps/assistant-service/src/assistant_service/core` as pre-existing mechanical Ruff cleanup outside a short semantic-file list. That classification omits substantive AS-02 changes in at least `core/gateway/execution_gateway.py` and `core/agent/middlewares/runtime_memory.py`.

A deterministic comparison against Ruff applied to the HEAD versions found that, after excluding five already identified semantic files, 66 of 109 files matched safe Ruff fixes byte-for-byte and another 10 matched Ruff unsafe fixes byte-for-byte. The remaining 33 include manual lint-motivated rewrites and semantic files requiring individual review. The broad cleanup is explainable by the prescribed Ruff scope, and no regression appeared in the executed suites, but it is not accurately described as entirely mechanical.

**Required correction:** revise the implementation accounting so substantive AS-02 files and manually rewritten files are not represented as mechanically generated. Retain or add targeted regression coverage for manual changes not exercised by the required AS-02 suites; a wholesale cleanup rollback is not requested by this verdict.

## Validation Evidence

All five phase-prescribed commands were executed exactly and passed without skips:

| Gate | Result |
|---|---|
| `uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py` | 26 passed |
| `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py tests/services/assistant/test_agent_capability_allowlist.py` | 22 passed |
| `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | 41 passed |
| `make verify-assistant-runtime-dev && make test-isolation` | AHR-01: 28 passed; AHR-02: 77 passed; AHR-03: 8 passed; AHR-04: 94 passed; golden gate passed; isolation: 6 passed |
| prescribed Ruff command over gateway, core, assistant, and AS-02 tests | passed: `All checks passed!` |

Additional independent checks:

- `uv run pytest -q --no-cov tests/database/test_agent_runtime_migration.py`: 3 passed, no skips.
- `uv run --all-packages pytest -q --no-cov tests/assistant/docgen`: 135 passed.
- `git diff --check`: passed.
- Golden JSON schema/phase/feature/status/case-set validation: passed; all six expected cases report passed and `provider_calls = 0`.
- All 11 golden `evidence_tests` node IDs collected successfully using the assistant-service package and `--no-cov`.
- Focused no-write resume probe: reproduced C-01.
- Focused database transaction, rolled back: reproduced C-02.

The passing suites establish substantial positive behavior, but they do not contain the negative cases in C-01 through C-03. A green gate therefore does not override the reproduced requirement failures.

## Security, Isolation, and Evidence Assessment

- Envelope authenticity and closed-schema enforcement are strong in the reviewed path: signature, hashes, canonicalization, identity binding, freshness, and replay checks occur before model/config/assistant acquisition.
- Preview/publication resolution performs authorization before producing runtime identity, and revoked versions are rejected.
- Tool, dataset, tenant, and MCP checks are deny-first and occur before cached results are returned.
- Secret/provider values were neither printed nor persisted by this review.
- R2 is incomplete because downstream skill selection can widen beyond the signed capability set.
- R3 is incomplete because resume omits session identity and the database permits invalid published identity shapes.
- The golden artifact is a structurally valid receipt with resolvable evidence nodes, but it does not exercise a real provider and cannot substitute for the missing negative isolation cases.

## Runtime and Rollback Assessment

Before runtime mutation, all eight repository services were healthy and the gateway/assistant Compose ownership labels pointed to `/Users/yang/projects/AI--Platfform`. The required isolation command was run after serially recreating only gateway and assistant in stub mode with provider inputs explicitly blank, using the current source and no image build. Both services were then serially restored to `ASSISTANT_E2E_STUB_LLM=false`, source-refreshed, restarted, and health-checked.

Final state:

- all eight services healthy;
- gateway and assistant ownership labels correct;
- gateway and assistant stub mode false;
- named provider inputs absent;
- sampled stack memory approximately 725 MiB;
- a no-key Preview Session probe returned HTTP 503 with `AGENT_RUNTIME_MODEL_UNAVAILABLE`, as required for the non-stub/no-provider state.

Migration 072 is additive and the feature flag provides an operational containment path, but destructive rollback was neither authorized nor exercised. The local replay backend is memory-backed and the current assistant process is single-worker; a multi-replica deployment must use the documented shared replay backend. This is a residual deployment condition, not a substitute for correcting C-01 through C-03.

## Oracle and Handoff Decision

AS-F003 must remain `failing`:

- oracle step 1 is incomplete because skill selection can expand beyond the resolved capability list;
- oracle step 2 fails because same-Agent-Version run resume is not session-bound;
- oracle step 3 has positive migration coverage but lacks and currently fails the required persisted-identity negative shape;
- oracle step 4 is supported by the passing forgery, freshness, and replay coverage.

The implementation is therefore not ready for phase approval or downstream handoff. Correct C-01 through C-03, update the evidence classification in C-04, add the specified negative tests, rerun all AS-02 gates, regenerate the golden receipt if its evidence nodes change, and obtain a new independent critic verdict. Whole-demand regression remains a terminal AS-09 responsibility and was not claimed here.
