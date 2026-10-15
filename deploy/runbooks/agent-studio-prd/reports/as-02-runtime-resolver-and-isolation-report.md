# AS-02 Runtime Resolver and Isolation Actor Report

**Phase:** AS-02 - Runtime Resolver and Isolation  
**Feature:** AS-F003  
**Status:** passed  
**Date:** 2026-07-18  
**Actor:** primary implementation agent

## Summary

AS-02 now connects the AS-01 Agent domain to the existing Assistant without
trusting browser-authored Agent configuration. Gateway resolves an authorized
Draft or immutable Version, produces a canonical Runtime Snapshot, and signs an
identity/session/body/spec/time/nonce-bound `AgentRuntimeEnvelope`. Assistant
recalculates the request and Snapshot hashes, verifies the Envelope, consumes
the nonce through the configured replay store, constructs prompt and execution
state only from the verified Snapshot, and applies a non-expanding capability
and Knowledge upper bound before selection, cache lookup, or invocation.

Preview and published Runtime API routes use closed external schemas. Generic
Assistant requests reject reserved Agent headers and body fields. Sessions,
idempotency keys, checkpoints, runs, and traces carry explicit Agent, Version or
Draft, Publication/channel, runtime-fingerprint, and spec-hash dimensions.
Existing built-in Assistant requests continue to use `agent_runtime=None` and
`capability_allowlist=None`, preserving the legacy behavior and SSE shape.

The first independent Critic returned `changes_requested`. Actor iteration 2
closed its persisted identity, exact Skill subset and evidence-accounting
findings and materially hardened run resume, but the second fresh Critic found
two remaining expansion paths. Actor iteration 3 now closes both with negative
evidence: an Agent loop may finalize or checkpoint only after its own
`start_run` succeeds, and `finish_run` binds memory and SQL updates to session
plus all Agent runtime dimensions; Gateway capability resolver output is now
only an authorization subset and the Snapshot retains the immutable Version
binding's risk, version, schema hash and config.

All five required Actor commands were run on the corrected iteration-3 source
and passed with no skips: Envelope `27 passed`; resolver/isolation/allowlist
`23 passed`; trace/session/golden `45 passed`; AHR-01 through AHR-04 were
`28/77/8/98` and the golden gate passed; live Gateway-to-Assistant isolation
was `6 passed`; and required Ruff exited zero. Migrations 072/073 passed five
PostgreSQL contract tests and remain applied in the authorized local dev stack
with no pending migrations. The durable golden receipt now collects all 14
current evidence nodes with `provider_calls=0`.

The repository-owned eight-service Compose stack is healthy and sampled at
approximately 720 MiB, below the operator's 3.5 GiB stop line. Explicit offline
stub probes proved real Gateway-to-Assistant HTTP/SSE transport with
`provider_calls=0`; both containers were then restored to `stub=false`. With no
model credentials present, the final live Preview Session probe correctly
fails closed as `503 / AGENT_RUNTIME_MODEL_UNAVAILABLE`.

AS-F003 is `passing`: the iteration-3 independent Critic approved this frozen
Actor evidence after independently reproducing the fixes and all required gates.

## Plan Followed

- Plan artifact: `docs/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-plan.md`
- Deviations: no architecture or feature-scope deviation. A live check exposed
  one fail-open readiness defect outside the initial likely-edit list:
  `GatewayModelMeta.is_provider_configured()` treated an enabled DB row as
  executable. `src/main.py` and `src/services/llm/gateway_model_meta.py` were
  minimally changed so Agent Runtime requires the exact provider set loaded by
  the separate Assistant process. The first probe returned 201; only the final
  post-fix `503 / AGENT_RUNTIME_MODEL_UNAVAILABLE` is counted as passing.
- AS-03 MCP/Connector principal implementations, AS-04 Skill/Knowledge
  persistence, Studio UI, publication promotion, Hosted/Embed, deployment,
  commit, and push were not entered.

## Files Changed and Minimal Change

| File/group | Why required | Smallest sufficient boundary |
| --- | --- | --- |
| `packages/ai-gateway-core/src/ai_gateway_core/agents/{__init__,runtime}.py` | Canonical Snapshot JSON/hash, signed Envelope, verifier, replay-store contract, verified execution context | Shared by Gateway signer and Assistant verifier so the two processes cannot drift |
| `src/api/schemas/agent_runtime.py`, `src/api/v1/agent_runtime.py`, `src/api/router.py` | Closed Preview/Published inputs, ACL-first resolver, immutable binding-preserving capability intersection, Snapshot normalization, session pinning, signed proxy | Additive Agent-only routes; resolver results may remove bindings but cannot lower risk or replace Version metadata; existing Assistant request schema remains unchanged |
| `src/api/v1/{assistant,_assistant_proxy}.py` | Reject public reserved Agent fields/headers and preserve signed internal proxy boundary | Narrow validation at the two existing external/proxy boundaries |
| `src/main.py`, `src/services/llm/gateway_model_meta.py` | Make model readiness agree with the provider configuration actually loaded by Assistant | Live-discovered fail-closed correction; no provider key is read or returned by the request path |
| `database/migrations/{072_agent_runtime_dimensions,073_agent_runtime_identity_constraints}.sql` | Explicit revocation/dimensions plus forward identity hardening | Nullable legacy rows remain valid; Preview revision is positive; published rows require matching Agent/Version/Publication; checkpoints match run session scope |
| `packages/ai-gateway-core/src/ai_gateway_core/{persistence/database.py,session/database_manager.py,session/models.py}` | Bind and reconstruct sessions/idempotency state with Agent dimensions | Extends existing session storage rather than creating a second store |
| `apps/assistant-service/src/assistant_service/api/routes/{chat,runs_approvals}.py` | Verify Envelope before config/model access, repeat policy intersection, redact internal SSE data and carry expected resume session | Agent-only configuration remains internal; generic chat/resume compatibility is preserved |
| `apps/assistant-service/src/assistant_service/core/agent/runtime_context.py` and substantive `agent_loop.py`, `assistant_service.py`, `gateway/execution_gateway.py`, `agent/middlewares/runtime_memory.py`, `tool_invoker.py`, `trace_writer.py`, `tools/memory_tool.py` changes | Trusted prompt layering, namespaced memory/idempotency/checkpoints, session-bound start/resume/finalization, allowlist/KB enforcement, explicit trace columns, legacy memory compatibility | A failed conflicting start cannot finish or checkpoint another session's run; finish memory/SQL predicates include session and Agent dimensions; `None` preserves built-in Assistant compatibility |
| `packages/ai-gateway-core/src/ai_gateway_core/skills/registry.py`, `core/skills/tool_bridge.py` | Preserve the exact signed Skill subset through DB loading, selection, prompt metadata and tool registration | `None` retains legacy all-Skills behavior; an explicit Agent set can only reduce registry names and normalized `skill_*` tools |
| `tests/api/test_agent_runtime_envelope.py`, `tests/database/test_agent_runtime_migration.py`, `tests/services/assistant/test_agent_{runtime_resolver,runtime_isolation,capability_allowlist,trace_capture,loop_golden}.py` plus existing message-persistence coverage | Required deterministic security, isolation, pinning, trace, golden, and compatibility evidence | Phase-named tests and adjacent regression only |
| `reports/agent-studio/as-02-golden-results.json` | Durable six-case offline golden receipt | Contains case IDs, observed behavior, test node IDs, status, and `provider_calls=0` |
| 115 tracked changes plus new `agent/runtime_context.py` under `apps/assistant-service/src/assistant_service/core` | The exact required Ruff command covers the entire directory and initially reported 579 historical violations | Current deterministic accounting: outside the five files initially identified as semantic, 66/110 match safe Ruff fixes byte-for-byte, 10/110 match unsafe Ruff fixes, and 34/110 contain manual lint or semantic edits requiring review. `tool_bridge.py` became the additional tracked semantic file in iteration 2; `execution_gateway.py` and `runtime_memory.py` are also substantive AS-02 files, not mechanical cleanup |
| `docs/agent-studio-prd` runtime artifacts | Required Actor evidence and cross-phase contract writeback | Only AS-02/AS-F003 state and downstream interfaces are updated |

The 66 safe-auto and 10 unsafe-auto Ruff-equivalent files are not represented
as Agent Studio feature work. The remaining 34 tracked files are not blanket-labelled
mechanical: they were reviewed as manual lint/semantic changes, with focused
run/resume/API contracts (`45 passed`), AHR coverage and the all-workspace
Docgen suite retained as adjacent regressions. An initial Docgen attempt under
the Assistant-only sub-package environment failed collection because
`reportlab` and `openpyxl` were unavailable; it is not counted. The correct
workspace command,
`uv run --all-packages pytest -q --no-cov tests/assistant/docgen`, ran and
passed all 135 tests. The repository script suite also passed 94 tests after
the Compose/env changes.

## Requirement Results

| Requirement | Result | Evidence |
| --- | --- | --- |
| R1 Authorized deterministic resolver | passed | Gateway repository resolution is tenant/ACL-first; external schemas forbid trusted overrides; 27 Envelope/forgery/readiness/non-expansion tests; live cross-Agent and cross-tenant session probes returned non-disclosing 404; no-key model readiness returns stable 503 |
| R2 Layered prompt and capability boundary | passed | Verified Snapshot is the sole config source; platform > Agent > channel > capability > memory/RAG > conversation/external ordering; resolver output authorizes only original binding keys and cannot replace risk/version/schema/config; exact Skill IDs/tool names constrain DB loading, selection, prompt metadata, registration and invocation; 23 Assistant resolver/isolation/allowlist tests |
| R3 Session and evidence pinning | passed | Resume compares expected session with persisted run and checkpoint; a failed start cannot enter run finalization/checkpointing; finish memory/SQL updates bind session and Agent dimensions; migration 073 enforces complete/same-Agent/session identities; 45 trace/session/golden tests and 5 migration tests |
| R4 Legacy compatibility | passed | Agent context/allowlist remain `None` for built-in Assistant; AHR groups pass; live nonstream/SSE isolation contract is 6/6 with no skip; generic Assistant forgery is rejected without changing ordinary response fields |

## Validation Evidence

| Gate ID | Exact command/check | Exit/result | Durable output |
| --- | --- | --- | --- |
| gateway-envelope | `uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py` | exit 0; `27 passed`; no skips | Includes canonical hash, mutation, expiry, replay, body/session substitution, public forgery, model policy intersection, same-ID capability metadata mutation denial, explicit stub, and DB-enabled/runtime-unconfigured readiness cases |
| resolver-isolation | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py tests/services/assistant/test_agent_capability_allowlist.py` | exit 0; `23 passed`; no skips | Includes the two-Skill exact selection/prompt/registration/invocation denial case |
| trace-session | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | exit 0; `45 passed`; no skips | Includes conflicting-session AgentLoop finalization denial, SQL session/runtime predicates, missing/cross-session resume and checkpoint-session drift denials, explicit dimensions, compatibility and six golden cases |
| runtime-gate | `make verify-assistant-runtime-dev && make test-isolation` | exit 0; AHR-01 `28`, AHR-02 `77`, AHR-03 `8`, AHR-04 `98`, golden pass; isolation `6 passed`; no skips | Final iteration-3 run used explicit stub, local generated credentials and explicit model selector; historical skipped/failed iteration-2 attempts are not counted |
| lint | `uv run ruff check packages/ai-gateway-core/src/ai_gateway_core/agents src/api/v1/assistant.py src/api/v1/_assistant_proxy.py src/api/v1/agent_runtime.py src/api/schemas/agent_runtime.py apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py tests/api/test_agent_runtime_envelope.py tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py` | exit 0; `All checks passed!` | Exact Phase command; no path exclusion or ignore relaxation |
| migration contract | `uv run pytest -q --no-cov tests/database/test_agent_runtime_migration.py` | exit 0; `5 passed`; no skips | Rejects incomplete published identity, non-positive Preview revision, same-tenant cross-Agent Version/Publication and checkpoint session drift; also covers idempotency/revocation/legacy rows |
| migration status | `bash scripts/new/migrate.sh --status` | exit 0; 69 applied including 071/072/073; no rollback records; no pending migrations | Preflight counts for invalid shape, relational mismatch and checkpoint scope were all zero before applying 073 |
| Docgen adjacent regression | `uv run --all-packages pytest -q --no-cov tests/assistant/docgen` | exit 0; `135 passed` | Covers the mechanically linted Docgen tree in the correct workspace environment |
| open-source script regression | `uv run pytest -q --no-cov tests/scripts` | exit 0; `94 passed` | Compose/env/startup script compatibility |
| resume/API adjacent regression | `uv run pytest -q --no-cov tests/api/test_gateway_capability_matrix.py tests/contract/test_find_active_command.py tests/contract/test_migrated_routes_equivalence.py` | exit 0; `45 passed` | Covers public/internal resume delegation, approval/checkpoint behavior and route compatibility |
| Compose and diff | `docker compose config --quiet`; `git diff --check` | both exit 0 | Base Compose renders; current worktree has no whitespace errors |

## Browser, Runtime, Eval, Security, and Migration Evidence

- **Browser matrix/screenshots:** not applicable. AS-02 adds no Web route or UI;
  API/OpenAPI/SSE integration is the Phase contract.
- **Runtime/Trace/golden table:**
  `reports/agent-studio/as-02-golden-results.json` contains exactly normal,
  no-tool, KB/tool trace, prompt injection, permission denial, and resource
  unavailable cases; all are `passed`, reference 14 real collected test node
  IDs including both iteration-3 negative cases, and record `provider_calls=0`.
- **Live local HTTP:** with explicit stub enabled on both processes and every
  provider input forced empty, login was 200, Preview Session 201, Preview chat
  200, and SSE emitted `text_delta,done`. Cross-Agent session reuse and a
  cross-tenant signed user both returned 404; generic Assistant forgery and
  Preview schema forgery returned 422. The three runtime paths were present in
  full Gateway OpenAPI. This is transport/auth/isolation proof, not provider or
  answer-quality proof.
- **Discarded runtime attempts:** the first final-source gate skipped two live
  cases because account variables were absent; the second injected the account
  but failed those cases because no explicit stub model selector was supplied.
  A recreated Assistant also initially rejected a shell-mangled CORS JSON
  value. All were corrected without changing credentials or product policy and
  none is represented as passing evidence; only the subsequent 6/6 run counts.
- **Final readiness state:** after restoring both containers to `stub=false`,
  login remained 200 and Preview Session returned
  `503 / AGENT_RUNTIME_MODEL_UNAVAILABLE`; both services are healthy and expose
  `stub_enabled=False`. No provider call occurred.
- **Security/compliance:** signature covers issuer, tenant, caller, Agent,
  Version/Draft, Publication/channel, session, body hash, Snapshot hash, spec
  hash, issued/expiry time, and nonce. Assistant recalculates hashes and the
  replay store consumes the nonce. Secret-shaped Snapshot configuration is
  rejected. Protected Snapshot/Prompt/policy/credential fields are removed
  from SSE and trace payloads. Tenant/ACL lookup precedes Snapshot load.
  Capability and Dataset uncertainty is deny-empty; invocation checks occur
  before cache/executor access.
- **Migration/idempotency/rollback:** 072 remains additive and 073 is a
  forward-only constraint hardening. Legacy rows may keep all dimensions null;
  Preview uses a positive draft revision; published channels require matching
  Agent/Version/Publication identity. A composite checkpoint/run scope FK and
  runtime guards reject session drift. Revocation records remain explicit and
  immutable.
- **Console/network summary:** no browser console applies. Live HTTP responses
  were inspected only for status, stable code, and public SSE event types; no
  password, JWT, shared secret, API key, Snapshot, or protected Prompt was
  printed.

## Feature Oracle Updates

| Feature | Old status | Current pre-Critic status | Actor evidence |
| --- | --- | --- | --- |
| AS-F003 | failing | passing | This report, `reports/agent-studio/as-02-golden-results.json`, and the approved iteration-3 independent Critic verdict |

The Actor did not self-approve. AS-F003 transitioned only after the independent
iteration-3 Critic approved the frozen slice and reproduced every required gate.

## Independent Critic

- Requested critic scope: Phase/Oracle contract, actual frozen diff, all five
  exact required commands, migration/runtime/golden receipts, fail-closed
  readiness fix, cross-Agent/tenant isolation, prompt/tool/KB boundaries,
  built-in Assistant compatibility, broad mechanical Ruff cleanup, rollback,
  and downstream interface handoff.
- Iteration-1 Critic artifact:
  `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-1.md`.
- Iteration-1 verdict: `changes_requested` (C-01 session resume, C-02 persisted
  identity, C-03 exact Skill subset, C-04 evidence accounting).
- Iteration-2 Critic artifact:
  `docs/agent-studio-prd/reports/as-02-critic-verdict-iteration-2.md`.
- Iteration-2 verdict: `changes_requested`; C-02/C-03/C-04 were approved, C-01
  retained a wrong-session finalization path, and C-05 proved a same-ID
  capability metadata substitution could bypass the channel risk policy.
- Actor iteration-3 corrections: finalization/checkpoint acquisition and exact
  finish scope close C-01; immutable binding materialization closes C-05. Both
  are negatively tested and included in the golden receipt.
- Canonical re-review artifact:
  `docs/agent-studio-prd/reports/as-02-critic-verdict.md` (`approved`).

## Compliance, Rollback, and Residual Risk

- **Compliance gates:** Gateway-only resolution, closed external schemas,
  canonical signed Envelope, Assistant hash/nonce verification, ACL-before-load,
  deny-overrides-allow, unknown-readiness fail-closed, explicit memory policy,
  prompt layering, trace redaction, session pinning, and built-in compatibility
  all have executed tests or live evidence above.
- **Rollback tested:** `AGENT_STUDIO_RUNTIME_ENABLED=false` is covered by the
  feature-flag test and leaves the built-in Assistant path untouched. The safe
  application rollback is to disable/remove the additive Agent runtime routes
  while retaining nullable dimensions, immutable Version/audit data, and
  revocation history. No destructive schema rollback was run or authorized.
- **Blockers/waivers:** none. The independent Critic approved the corrected
  slice; the orchestrator owns the single phase completion-gate invocation.
- **Residual risk:** no real provider availability or response-quality claim is
  made because the user excluded API Key modification and no live provider call
  was authorized. Provider readiness is sampled at process startup to match the
  current Assistant registry; a provider configuration change therefore
  requires service recreation/restart and fails closed until then. Actual
  MCP/Connector credential principals and Skill/Knowledge authorization
  adapters remain AS-03/AS-04 work; their absent AS-02 resolvers yield empty
  capabilities and cannot expand execution.
- **Local dev data:** two `AS02 Live ...` Agents and their validation sessions
  remain in the authorized local dev database. They contain no API Key or
  production data and were not destructively deleted.

## Runtime Artifact Writeback

- `feature-oracle.json`: AS-F003 is `passing`; Actor, golden, and approved
  canonical Critic evidence are linked.
- `loop-state.json`: the next orchestrator write advances execution to AS-03.
- `progress-log.md`: AS-02 Actor evidence and live readiness correction appended.
- `agent-handoff.md`: AS-02 is approved for downstream handoff.
- `continuity-ledger.md`: signed Envelope, execution context, pinning, explicit
  dimensions, readiness, and downstream Capability adapter contracts appended.
- `source-packet.md`: AS-02 verified code facts and evidence boundary appended.
- `next-window-prompt.md`: the next write selects AS-03 as the target phase.

## Handoff

The iteration-3 independent Critic approved C-01/C-05 and reproduced the five
required gates. AS-F003 is therefore ready for the orchestrator's completion
gate and AS-03/AS-04 dependency unlock.
