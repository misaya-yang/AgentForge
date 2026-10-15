# AS-04 Independent Critic Verdict — Iteration 3 (Preserved)

**Phase:** AS-04 — Skills and Knowledge Version Bindings  
**Feature:** AS-F005  
**Critic:** fresh-context reviewer, independent of the iteration-3 Actor  
**Critic Verdict:** `changes_requested`  
**Actor Report:** `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md`  
**Prior Verdicts:** `as-04-critic-verdict-iteration-1.md`, `as-04-critic-verdict-iteration-2.md`  
**Date:** 2026-07-19

## Decision

AS-04 / AS-F005 is not approved on this frozen source. Iteration 3 credibly
closes AS04-C01, AS04-C02 and AS04-C03, and it repairs the transport and
per-Dataset execution portions of AS04-C04. Exact Skill persistence, isolation,
version loading, entrypoint restrictions, capability upper bounds, normalized
Knowledge rows, ACL checks, authoritative content/config fingerprints and
per-Dataset retrieval configuration all have independently rerun local evidence.

Two release blockers remain at the real Assistant composition/execution
boundary:

1. **AS04-C04 remains open.** The current Agent streaming route always executes
   `AgentLoop`, but `AgentLoop` never runs the automatic retrieval implemented in
   `AssistantService._retrieve_context`. A signed `auto` binding can therefore
   finish successfully with zero retrieval calls. In addition, a Knowledge-only
   binding does not expose `search_knowledge_base`; that tool is filtered by the
   separate signed capability list.
2. **AS04-C05 is new and open.** The Assistant production composition root wires
   neither `agent_runtime_resource_policy` nor `agent_runtime_tenant_policy` nor
   `tenant_tool_policy`. The runtime mapper therefore reduces every signed
   Skill/tool and Dataset set to empty. The existing green tests inject synthetic
   policy objects and do not exercise this production wiring.

The first defect permits silent model-knowledge fallback where automatic bound
Knowledge was configured. The second is fail-closed rather than an authorization
bypass, but it makes the released Agent Skill/Knowledge surface non-functional.
Both violate non-waivable AS-04 requirements. AS-F005 must remain `failing`,
AS-04 must remain unapproved, and AS-05 must remain locked pending remediation
and a fresh independent review.

## Reviewer Independence and Inspected Scope

This review was performed from the contracts and current source, not by accepting
the Actor report as proof. I inspected:

- the AS-04 phase contract, fixed plan, Actor report, both preserved Critic
  verdicts and `reports/agent-studio/as-04-skill-kb-golden.json`;
- product and architecture requirements for exact Skill versions, normalized
  Dataset bindings/configuration, live provenance, policy intersection and
  fail-closed degradation;
- migrations 075 and 076 and the production Document/Segment schemas;
- Skill parser/models/persistence/API/scoped registry/request-local bridge and
  exact-version execution path;
- Agent save/publish/run resource resolution, Gateway Snapshot construction and
  signed per-Dataset configuration;
- Knowledge Dataset catalog fingerprinting, configuration redaction, content
  revision triggers, reindex revision movement and hierarchical retrieval fields;
- Assistant envelope verification, tenant-policy reduction, runtime mapping,
  AgentLoop tool selection, Knowledge provenance, automatic and tool retrieval,
  cache/trace identity, and the actual `main.py` composition root;
- the relevant working-tree diff and history. The checkout contains extensive
  pre-existing changes and no clean AS-04 commit boundary, so ownership was not
  inferred from `git status`; review conclusions use path-scoped source and
  behavior.

I did not edit product source, tests, migrations, Docker state, the golden, Actor
report, Feature Oracle, loop state, handoff, progress log or continuity files.
The only write made by this Critic is this canonical verdict.

## Findings and Prior-Finding Disposition

| ID | Disposition | Severity | Finding / evidence | Required correction |
| --- | --- | --- | --- | --- |
| AS04-C01 | closed in its original scope | — | Migration 076 advances an authoritative per-Dataset content revision for covered same-count retrieval-content changes; the catalog emits a strict fingerprint; AgentLoop rejects missing/malformed fingerprints before provider execution. Required migration/catalog/Agent regressions pass. | Preserve the current positive/negative revision projection and fail-closed catalog contract. |
| AS04-C02 | closed | — | `_dataset_revision_fingerprint` now hashes a closed, recursive, non-secret projection of retrieval-effective Dataset configuration. Config-only changes move both catalog and next-run hashes; credential rotations do not. The independently rerun five-test fingerprint suite passes. | Preserve the explicit allowlist and secret exclusions. Residual hardening: reject credentials embedded in endpoint paths if that input form is supported, because URL identity intentionally retains the path. |
| AS04-C03 | closed | — | Migration 076 compares explicit production-shaped Document/Segment retrieval projections, including hierarchical fields, while excluding progress/error/timing/count/hash/audit telemetry. AgentLoop hashes bound IDs, sealed retrieval config and authoritative fingerprints, not catalog timestamps/counts. Required PostgreSQL and Agent regressions pass. | Preserve the explicit projection and telemetry-negative tests when schemas change. |
| AS04-C04 | **open** | **high, release-blocking** | Snapshot exact-key validation, mixed `auto`/`tool`/`off` mapping, off exclusion, tool executor config enforcement, image flags and cache/trace identity are repaired. However, the only green “auto RAG” test calls `AssistantService._retrieve_context` directly. The production streaming path calls `_execute_agent_loop`, and AgentLoop has no automatic retrieval step; a focused run completed with `retrieve_calls=0`. Also, Knowledge bindings are independent rows, but the KB tool is visible only if separately present in the capability allowlist, so a valid Knowledge-only tool binding cannot execute. | Implement `auto` retrieval in the actual AgentLoop/Agent route, using only auto-bound Datasets and their sealed configs. Make the internal KB tool available from an active authorized Knowledge binding, or explicitly define and validate a capability dependency at save/publish time. Add composition-level mixed-mode tests proving auto retrieves, tool retrieves only on a call, off is neither exposed nor executed, and per-Dataset/image config reaches the real executor/trace. |
| AS04-C05 | **open (new)** | **high, release-blocking** | `_build_agent_runtime_config` initializes both policy intersections to empty. `_agent_runtime_tenant_policy` can return a resource policy from app state or `AssistantService.tenant_tool_policy`, but `main.py` assigns none. A production-shaped focused probe mapped a signed KB capability and Dataset to `effective_tools=[]`, `effective_datasets=[]`, `kb_mode=off`. Tests pass only because they inject `Policy`/`_Policy` doubles. The legacy `_ResolvedTenantPolicy` adapter also always returns an empty Dataset set. | Wire a production resource-policy implementation at Assistant startup that implements both `allowed_tool_names` and `allowed_dataset_ids`, rechecks current tenant/resource authority, and can only reduce the signed Snapshot. Add a composition-root/route test without hand-injected policy proving an authorized exact Skill/tool and Dataset survive, forged expansion is ignored, and revoke/delete/unavailable state fails before provider execution. |

### AS04-C04 runtime proof

`AssistantService.chat_stream` identifies AgentLoop as the only execution path
(`core/assistant_service.py`, lines 1235-1249) and passes the sealed Knowledge
map into `AgentLoopConfig` (lines 1983-2108). `AgentLoop` loads catalog
provenance and then proceeds to prompt/tool selection; `auto` is only a prompt
instruction to prefer `search_knowledge_base`
(`core/prompts/system_prompt_v2.py`, lines 1228-1244). The automatic retrieval
implementation at `AssistantService._retrieve_context` is used by the non-stream
path and by a direct unit test, not by the Agent streaming route.

The Critic ran this provider-free probe against the actual `AgentLoop`: one
valid `AgentRuntimeExecutionContext` matching the verified mapper's shape, one
authoritative `auto` Dataset, a Knowledge double recording catalog/retrieval
calls, and a model double returning a direct answer. The command was
`uv run --package assistant-service python -` with the probe supplied on stdin.
Exit was 0 and the exact material result was:

```text
{'catalog_calls': 1, 'retrieve_calls': 0, 'model_calls': 1, 'terminal_events': ['run_finished']}
```

This is not a claim that every auto-mode question must retrieve. It proves that
the implemented Agent route has no automatic scheduler at all, despite the
Actor claim that auto bindings are applied to automatic RAG and despite the
separate non-stream implementation treating `RAGMode.AUTO` as pre-retrieval.
For a question explicitly asking about the bound internal policy, the run
silently completed from the model without a retrieval completion or failure.

The second half of C04 follows from the same real path. `_get_streaming_tools`
filters definitions through the exact capability allowlist
(`core/agent/agent_loop.py`, lines 2461-2523), while Gateway Knowledge bindings
do not add `search_knowledge_base` to Snapshot capabilities. The phase's own
successful Knowledge fixture contains `capabilities=[]`; therefore a
Knowledge-only `tool` binding exposes no KB tool unless an undocumented second
binding is added.

### AS04-C05 composition proof

The production root constructs `AssistantService` without a tenant/resource
policy (`apps/assistant-service/src/assistant_service/main.py`, lines 375-388).
Its constructor default stores `tenant_tool_policy=None`
(`core/assistant_service.py`, lines 645-670), and no production assignment to
`app.state.agent_runtime_resource_policy` or
`app.state.agent_runtime_tenant_policy` exists in the reviewed tree.

The Critic ran a provider-free probe through the actual verifier,
`_agent_runtime_tenant_policy`, and `_build_agent_runtime_config`, using the same
production-shaped absent-policy state. The command was
`uv run --package assistant-service python -` with the probe supplied on stdin.
Exit was 0 and the exact result was:

```text
{'signed_capabilities': ['search_knowledge_base'], 'signed_datasets': ['dataset-a'], 'policy': None, 'effective_tools': [], 'effective_datasets': [], 'kb_mode': 'off'}
```

This behavior is intentionally deny-all under uncertainty, so it does not
create cross-tenant access. It does contradict the Actor's runtime-usable Skill
and Knowledge claims and leaves no successful production path for the feature.

## Independent Validation

All required AS-04 commands and three focused supplemental suites were rerun on
the reviewed source. No required or supplemental test was skipped.

| Gate | Exact command | Exact result |
| --- | --- | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | exit 0; 26 collected; `26 passed, 1 warning in 0.41s`; zero skips |
| Skill runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | exit 0; 6 collected; `6 passed, 1 warning in 0.48s`; zero skips |
| Knowledge binding/streaming | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | exit 0; 43 collected; `43 passed, 1 warning in 0.74s`; zero skips |
| Exact AS-04 lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | exit 0; `All checks passed!` |
| PostgreSQL migration contract | `uv run pytest -q --no-cov tests/database/test_agent_skill_knowledge_migration.py` | exit 0; 4 collected; `4 passed, 1 warning in 0.96s`; zero skips |
| Knowledge revision fingerprint | `uv run --package knowledge-service pytest -q --no-cov tests/services/knowledge/test_dataset_revision_fingerprint.py` | exit 0; 5 collected; `5 passed, 1 warning in 0.49s`; zero skips |
| Runtime resolver | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | exit 0; 7 collected; `7 passed, 1 warning in 0.40s`; zero skips |

Across six pytest invocations, **91 tests passed with zero skips**. Each pytest
invocation reported the same Starlette TestClient deprecation warning. Ruff is
reported separately and passed. Green focused tests prove their assertions;
they do not substitute for a production-composition test or make an unused
helper part of the Agent streaming path.

## Requirement, Security, Regression, and Minimal-Scope Assessment

| Area | Critic assessment |
| --- | --- |
| R1 / Skill persistence and isolation | Locally covered. Persistence is honest, tenant/user/version scoped, uploads are instruction-only with server-owned identities, and executable/source forgery is rejected. |
| R2 / exact Skill execution | Component behavior is locally covered: exact immutable content/version, request-local overlay, kill switch and capability non-expansion pass. End-to-end production usability remains blocked by C05. |
| R3 / authorized Knowledge binding and provenance | Persistence, ACL resolution, exact per-Dataset transport, fingerprinting and tool-executor config are locally covered. Runtime use remains incomplete under C04 and entirely removed by the missing C05 composition policy. |
| R4 / fail-closed revocation and degradation | Missing fingerprints/resources fail closed before provider use. C05 also fails closed, but deny-all is not successful feature behavior. C04 permits a successful auto-mode terminal run without retrieval, creating the silent model-knowledge substitution risk R4 forbids when bound evidence is required. |
| Security/privacy | No authorization expansion or cross-tenant bypass was found in the reviewed paths. Skill permissions remain below the signed/policy upper bound, Dataset IDs/configs are exact, unbound IDs are rejected, and credentials are excluded from the reviewed hash fields. Endpoint URL paths remain part of non-secret identity; deployments must not embed credentials in paths. No secret value was read into this verdict or printed by the review. |
| Regression | The required and focused local suites are green with zero skips. No browser work is required for this no-UI phase. The existing tests lack a real Assistant startup/Agent-route happy path and therefore miss both blockers. |
| Minimal scope | The relevant AS-04 changes are mostly confined to the planned binding, resolver, Knowledge fingerprint and Assistant runtime paths. Because the shared checkout has a large inherited dirty tree and no phase commit, a clean change-only diff cannot be proven. The Critic preserved all inherited changes and wrote only this verdict. |
| Rollback | Skill kill-switch, immutable binding rows and fail-closed resource state are retained. They do not correct C04/C05. No destructive rollback or migration reversal was performed. |

## Evidence Boundary

The Critic personally observed the seven commands and two provider-free probes
above. No Docker, live runtime, browser, external provider, deployment,
production database, commit or push action was performed. Actor claims about
Compose ownership/health, live PostgreSQL probes, authenticated CRUD, memory
use, AHR aggregate gates and live isolation remain Actor-reported; they are not
used to close C04 or C05.

The explicitly obsolete whole-Harness strict migration validator was not run,
consistent with the user's narrow waiver. No current AS-04 test, security,
provenance, regression or independent-Critic gate was waived.

## Required Transition

- AS04-C01: closed in its original iteration-1 scope.
- AS04-C02: closed.
- AS04-C03: closed.
- AS04-C04: open, release-blocking.
- AS04-C05: open, release-blocking.
- AS-F005: keep `failing`; do not add passing evidence.
- AS-04: keep active pending remediation and fresh independent review.
- AS-05: keep locked.
- Preserve both historical verdicts and this canonical verdict as review
  history; do not rewrite prior findings.

## Verdict Rationale

Iteration 3 materially fixes the prior provenance and per-Dataset transport
defects, and its component-level suites are strong. Approval nevertheless
requires the configured resources to survive the real production policy
intersection and the configured Knowledge mode to execute on the real Agent
route. Today the absent composition policy removes every resource, while an
isolated AgentLoop auto binding can complete without any retrieval and a
Knowledge-only tool binding has no KB tool. Because production usability,
fail-closed Knowledge honesty and exact runtime execution are non-waivable,
the only evidence-supported verdict is `changes_requested`.
