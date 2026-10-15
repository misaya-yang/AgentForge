# AS-02 Independent Critic Verdict - Iteration 3

**Phase:** AS-02 - Runtime Resolver and Isolation

**Feature:** AS-F003

**Critic:** independent fresh context reviewer

**Critic Verdict:** approved

**Actor Report:** `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`

**Prior Finding History:** `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-1.md`, `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-2.md`

**Date:** 2026-07-18

## Verdict Summary

AS-02 iteration 3 is approved for orchestrator-controlled phase completion. The
two material iteration-2 findings were independently exercised rather than
accepted from the Actor report:

1. a conflicting same-owner, same-Agent-Version session cannot acquire the run,
   and the Agent loop consequently cannot finish or checkpoint the rightful
   session's run; direct completion also rejects missing, wrong-session, and
   wrong-runtime Agent context, while its SQL predicate binds the session and
   every persisted Agent runtime dimension; and
2. a capability resolver result with the same type and resource key is only an
   authorization decision. The Snapshot retains the original immutable binding,
   so resolver-authored risk, version, schema hash, or configuration cannot
   replace it, and a bound high-risk capability remains denied when the channel
   disables high-risk tools.

The accepted iteration-2 closures for migration identity, the exact Skill
subset, and cleanup accounting remain intact. All five exact Phase-required
commands exited zero with no failures or skips. Migrations 072/073, all 14
golden evidence-node references, adjacent Docgen and resume/API regressions,
Compose ownership and health, the offline live-isolation path, and the final
no-provider fail-closed state were also independently checked.

This approval is not a completion-gate result. AS-F003 remains `failing` until
the orchestrator links this verdict, performs the authorized Oracle/state
transition, and runs the strict phase-scoped completion gate. AS-03 and AS-04
remain locked until that gate exits zero.

## Inputs Reviewed

- Phase contract:
  `docs/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md`
- Oracle item: AS-F003 only in
  `docs/agent-studio-prd/feature-oracle.json`
- Actor evidence:
  `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md`
- Critic template and preserved iteration-1/iteration-2 finding history
- Actual AS-02 diff and focused source for public schemas, Gateway resolution,
  canonical Snapshot/Envelope signing, Assistant verification, prompts,
  capabilities, Skills, Knowledge, memory, sessions, idempotency, runs,
  checkpoints, traces, redaction, model readiness, and legacy compatibility
- Migrations `072_agent_runtime_dimensions.sql` and
  `073_agent_runtime_identity_constraints.sql`, plus their PostgreSQL tests
- `reports/agent-studio/as-02-golden-results.json`
- Repository-owned local Compose runtime, inspected before mutation and after
  both the stub gate and mandatory non-stub restoration

The previous verdicts were used only as finding history, not as current
approval. No unrelated phase or full-demand artifact was treated as AS-02
completion evidence.

## Finding Re-evaluation

| Finding | Iteration-3 assessment | Independent evidence | Required correction |
| --- | --- | --- | --- |
| C-01 - session/run finalization isolation | resolved | A standalone no-write AgentLoop probe created an owner run and checkpoint, attempted the same run through another session, and observed the start rejection. The original run remained `running` with null error, empty usage, null finish time, and exactly the original checkpoint. Six direct wrong/missing-context completion attempts were rejected. SQL inspection confirmed tenant, user, session, Agent, Version or Draft, Publication, channel, runtime fingerprint, and spec hash predicates; the legacy branch adds `agent_id IS NULL`. | none |
| C-05 - immutable capability binding | resolved | A standalone Snapshot probe returned the same type/resource keys with forged risk, version, schema hash, and configuration. The allowed low-risk capability retained the original binding byte-for-byte, the original high-risk binding remained excluded under `high_risk_tools=false`, and no resolver metadata entered the Snapshot. | none |
| C-02 - persisted identity | no regression | Migration 073 still requires positive Preview revision, complete published identity, same-Agent Version/Publication relationships, and matching run/checkpoint session scope. The five PostgreSQL migration tests passed. | none |
| C-03 - exact Skill subset | no regression | The signed and tenant-approved Skill names remain exact through database load, registry list/selection, prompt material, bridge registration, normalized tool name, and invocation. `None` continues to preserve the built-in all-Skill legacy path. | none |
| C-04 - cleanup accounting | no regression | Independent HEAD-to-Ruff byte comparison reproduced 115 tracked core changes plus new `runtime_context.py`; outside the original five semantic files, exactly 66/110 match safe fixes, 10/110 additionally match unsafe fixes, and 34/110 are manual or semantic. Substantive Agent runtime files were reviewed as such rather than relabelled mechanical. | none |

No new material finding was identified.

## Requirement Coverage

| Requirement / Oracle step | Assessment | Evidence |
| --- | --- | --- |
| R1 / AS-F003 step 4 - authorized deterministic resolver and forgery/replay denial | supported | Browser schemas are closed and generic Assistant routes inspect raw bodies and reserved headers. Gateway performs tenant/ACL resolution and signs tenant, caller, Agent, Version-or-Draft, Publication/channel, session, request body, canonical Snapshot, spec, time, and nonce. Assistant recalculates body/Snapshot hashes, checks identity and time, and atomically consumes replay state; replay-store uncertainty fails closed. Envelope suite: 27/27. |
| R2 / AS-F003 step 1 - layered prompt and non-expanding capability/Knowledge boundary | supported | Prompt order remains platform, Agent, channel, capability, then lower-trust memory/RAG/conversation/external data. Resolver results can select only original immutable bindings. Tenant policy, Skill selection/registration/invocation, Knowledge dataset checks, visible tool definitions, cache lookup, and executor access can only reduce the signed upper bound. Resolver/isolation/allowlist suite: 23/23, including the exact two-Skill case and C-05 negative case. |
| R3 / AS-F003 steps 2 and 3 - session/evidence pinning | supported | Session, memory, idempotency, run, checkpoint, trace, Version/Publication, runtime fingerprint, and spec dimensions are explicitly scoped. Publication resolution honors a persisted Version pin and rejects revocation. C-01's full conflicting-start/finalization path now preserves the rightful run. Trace/session/golden suite: 45/45; migration suite: 5/5. |
| R4 - built-in Assistant compatibility | supported | `agent_runtime=None`, `capability_allowlist=None`, and `allowed_skill_ids=None` retain the legacy path. AHR groups, live non-stream/SSE isolation, message persistence, Docgen, and resume/API regressions passed. |

## Exact Required Validation Evidence

Every command below is copied exactly from the AS-02 Phase contract and was
run against the reviewed iteration-3 source.

| Gate | Exact command | Exit | Pass / skip / fail |
| --- | --- | --- | --- |
| gateway-envelope | `uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py` | 0 | 27 passed / 0 skipped / 0 failed |
| resolver-isolation | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py tests/services/assistant/test_agent_capability_allowlist.py` | 0 | 23 passed / 0 skipped / 0 failed |
| trace-session | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | 0 | 45 passed / 0 skipped / 0 failed |
| runtime-gate | `make verify-assistant-runtime-dev && make test-isolation` | 0 | AHR-01 28, AHR-02 77, AHR-03 8, AHR-04 98, golden pass; live isolation 6 passed / 0 skipped / 0 failed |
| lint | `uv run ruff check packages/ai-gateway-core/src/ai_gateway_core/agents src/api/v1/assistant.py src/api/v1/_assistant_proxy.py src/api/v1/agent_runtime.py src/api/schemas/agent_runtime.py apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py tests/api/test_agent_runtime_envelope.py tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py` | 0 | `All checks passed!` |

The pytest runs emitted only the existing Starlette/httpx deprecation warning;
there was no skip or test failure.

## Supplemental Validation Evidence

| Check | Exit/result | Assessment boundary |
| --- | --- | --- |
| `uv run pytest -q --no-cov tests/database/test_agent_runtime_migration.py` | exit 0; 5 passed; no skips | Re-exercises additive legacy compatibility and the C-02 negative identity/session constraints in disposable PostgreSQL schemas. |
| `bash scripts/new/migrate.sh --status` | exit 0; 69 applied including 071/072/073; zero rollback records; no pending migration | Authorized local development database only; not a production migration claim. |
| Golden JSON inspection | six required cases passed; offline deterministic; `provider_calls=0`; 14 unique evidence-node references | The 14 referenced nodes all collected with exit 0. This proves deterministic evidence integrity, not provider answer quality. |
| `uv run --all-packages pytest -q --no-cov tests/assistant/docgen` | exit 0; 135 passed; no skips | Adjacent regression for the broad Docgen/manual Ruff group. |
| `uv run pytest -q --no-cov tests/api/test_gateway_capability_matrix.py tests/contract/test_find_active_command.py tests/contract/test_migrated_routes_equivalence.py` | exit 0; 45 passed; no skips | Adjacent run/resume and public/internal API compatibility. |
| `uv run pytest -q --no-cov tests/scripts` | exit 0; 94 passed; no skips | Open-source startup/Compose script compatibility used by the Actor's scope accounting. |
| `git diff --check` | exit 0 | No whitespace errors in the reviewed worktree diff. |
| `docker compose config --quiet` | exit 0 | Base Compose renders from the generated local environment. |
| strict Harness validation without completion gate | exit 0; quality score 100 | Structural Harness readiness only; deliberately not completion proof. |

## Security, Privacy, and Failure Assessment

- Gateway remains the only browser-facing Draft/Version resolver. Public
  request models cannot carry trusted model, Prompt, capability, Snapshot,
  Version, Publication, hash, or runtime-envelope material, and generic
  Assistant routes reject the reserved raw body/header forms.
- Snapshot validation is closed and recursively rejects secret-shaped keys.
  Envelope verification checks the full signed identity and request material,
  recalculates hashes, validates TTL/freshness, and consumes a nonce only after
  signature and identity validation. Replay-store failure returns a stable
  unavailable error rather than executing.
- Capability, Skill, and Knowledge enforcement is deny-first. Tool authorization
  and dataset narrowing happen before a cache hit or executor access. Resolver
  absence, exceptions, malformed results, or asynchronous policy uncertainty
  produce an empty effective set.
- Agent memory and idempotency scopes include tenant, Agent, Version-or-Draft,
  channel, and session where applicable. User-memory mode uses a scoped
  principal and remains tenant-separated by the memory service call.
- Public SSE filtering removes internal Snapshot, Prompt, policy, allowlist,
  runtime, and credential fields. Trace writes use bounded recursive redaction
  and explicit Agent dimensions; protected Agent instructions were absent from
  the reviewed persistence probe.
- No external provider call was made. No credential, shared secret, session
  token, or local account detail was printed or written by this review.

## Minimal-Change Assessment

The canonical Snapshot/Envelope, Gateway runtime routes, Assistant verification,
session/run/checkpoint/trace dimensions, migrations, Skill subset, and model
readiness changes are AS-02-relevant. Expansion into `src/main.py` and
`src/services/llm/gateway_model_meta.py` is justified by the live-discovered
requirement that an enabled database provider row must not masquerade as an
executable provider in the separate Assistant process.

The 115-file tracked core cleanup is broader than the feature slice, but it is
not misrepresented in the current Actor evidence: 66/110 files are safe-Ruff
equivalent, 10/110 additionally match unsafe Ruff output, and 34/110 remain
manual/semantic. `tool_bridge.py`, `execution_gateway.py`, and
`runtime_memory.py` are correctly treated as substantive. Focused source review
plus Docgen 135, resume/API 45, scripts 94, required trace/session 45, and AHR-04
98 found no unsupported semantic drift in the reviewed AS-02 boundary. Unrelated
dirty and separately authorized packaging work remains outside this approval.

This Critic changed only
`docs/agent-studio-prd/reports/as-02-critic-verdict.md`.

## Runtime Restore and Rollback Assessment

Before runtime mutation, all eight repository services were healthy, every
Compose owner label pointed to `/Users/yang/projects/AI--Platfform`, Gateway and
Assistant were non-stub with the reviewed provider inputs absent, and aggregate
memory was approximately 709 MiB.

For the live isolation portion, Assistant and Gateway were recreated serially
from existing images with `COMPOSE_PARALLEL_LIMIT=1`, stub mode explicitly true,
and provider inputs explicitly empty. The complete current Assistant source or
Gateway `src` tree and complete `ai_gateway_core` package were hot-copied into
the corresponding service and restarted. No image build occurred. The live
gate then passed 6/6 without skips at approximately 720 MiB.

After the gate, Assistant and Gateway were recreated serially with stub mode
false and provider inputs still empty, fully hot-copied again, restarted, and
health-checked. Final state was:

- eight of eight services running and healthy;
- every service owned by this repository checkout;
- Gateway and Assistant both non-stub with reviewed provider inputs absent;
- aggregate memory approximately 720 MiB, below the 3.5 GiB stop line; and
- a safe local login probe succeeded, while Preview Session failed closed as
  `503 / AGENT_RUNTIME_MODEL_UNAVAILABLE`.

No build, prune, image/container/volume deletion, provider call, destructive
migration rollback, deployment, commit, or push occurred. The tested feature
flag remains the application containment path; migrations are additive and
forward-hardening, so destructive rollback was neither needed nor authorized.

## Rollback and Handoff Decision

The AS-02 rollback boundary is supportable: disable Agent runtime resolution,
retain nullable legacy-compatible dimensions and immutable evidence, and leave
built-in Assistant on the `None` context/allowlist path. The downstream
Capability adapter, signed Envelope, pinning, and policy-reason contracts are
supported for handoff only after the orchestrator completes the phase gate.

The orchestrator may use this approved verdict as the missing independent
evidence. It alone may link the canonical verdict, transition AS-F003, run the
strict AS-02 completion gate, and unlock AS-03/AS-04 if that gate passes. This
Critic did none of those state transitions.

## Whole-Demand Regression Assessment

AS-02 is not the terminal phase. Whole-demand same-build regression remains an
AS-09 responsibility. This verdict covers AS-F003 and inherited Assistant
runtime regressions only; it makes no production deployment, real-provider
quality, or full-product completion claim.

## Verdict Rationale

Approval is warranted because both previously exploitable paths were closed in
the actual control flow and independently reproduced as closed, all non-waivable
R1-R4 isolation and compatibility boundaries have executed evidence, every
required command passed with zero skips, the final runtime was restored to the
required fail-closed state, and no new material security or semantic-drift gap
was found. There is no waiver in this verdict.
