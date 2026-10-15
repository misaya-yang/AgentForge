# AS-04 Independent Critic Verdict — Iteration 4

**Phase:** AS-04 — Skills and Knowledge Version Bindings  
**Feature:** AS-F005  
**Critic:** fresh-context reviewer, independent of the iteration-4 Actor  
**Critic Verdict:** `approved`  
**Actor Report:** `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md`  
**Prior Verdicts:** iterations 1, 2 and 3, all preserved as `changes_requested`  
**Reviewed HEAD:** `945eb2225d644093802bf5f9d75ca4d9dbad6a8d` plus the inherited dirty worktree  
**Date:** 2026-07-19

## Decision

AS-04 / AS-F005 is approved on the reviewed iteration-4 source. I found no
remaining release-blocking defect after independently reading the production
composition root, request mapper, policy services, Agent resource resolver,
AgentLoop, Knowledge executor, exact Skill runtime, migrations and tests, and
after personally rerunning every required and requested supplemental gate.

Iteration 4 closes both iteration-3 blockers:

- AS04-C04 is closed because the actual streaming `AgentLoop`, before its first
  provider call, schedules every `auto` Dataset through the existing Knowledge
  executor. That executor fans out by Dataset and applies each sealed `top_k`,
  `threshold` and `include_images`. A `tool` binding exposes the internal
  Knowledge tool for model selection, while `off` bindings are absent from the
  executable Dataset set. All-auto retrieval failure emits stable
  `AGENT_KNOWLEDGE_UNAVAILABLE` and returns before the model.
- AS04-C05 is closed because production `main.py`, immediately after its
  mandatory database connection, creates the DB-backed Agent resource policy
  and existing tenant tool policy, stores/injects them into the real Assistant
  composition, and the Agent route resolves the verified caller against current
  tenant tool policy and Dataset ACL. The mapper only intersects the signed
  Snapshot: authorized platform tools, exact Skills, bound Datasets and the
  implicit Knowledge tool survive; forged expansion is discarded; missing,
  revoked or policy-unavailable bound Knowledge fails closed.

The Actor correctly left AS-F005 `failing`. The orchestrator may transition it
only after consuming this approval and completing the supported Phase claim
check. This Critic did not edit the Feature Oracle, loop state, handoff,
progress, continuity, golden, tests or product source.

## Scope and Independence

I reviewed the current source rather than accepting the Actor report as proof:

- AS-04 Phase contract, AS-F005 Oracle, fixed plan, iteration-4 Actor report,
  all three preserved Critic verdicts and the durable golden JSON;
- Skill API/parser/persistence, exact-version authorization, scoped registry,
  request-local bridge/invoker and capability upper-bound behavior;
- normalized Draft/Version Skill and Dataset bindings, composite tenant
  constraints, save/publish/run authorization and revocation behavior;
- Gateway Snapshot construction and Assistant verified-runtime mapping;
- `main.py`, `chat.py`, `tenant_tool_policy.py`, the Agent resource resolver,
  the actual streaming AgentLoop and `KBSearchExecutor` fan-out;
- content-revision triggers, Dataset fingerprinting, secret-free configuration
  projection, provenance/non-replayability and Assistant compatibility paths.

The checkout contains extensive inherited modifications and no clean AS-04
commit boundary, so a phase-only Git diff cannot be reconstructed reliably.
Conclusions therefore use path-scoped current-source and executed-behavior
evidence, not change ownership inferred from `git status`. No AS-05 Studio UI,
publication channel, deployment, Docker mutation, commit or push was performed.

## Required Test Receipts

Every exact Phase command was run personally on the reviewed source. No required
test was skipped.

| Gate | Exact command | Exit | Result |
| --- | --- | ---: | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | 0 | 26 collected; 26 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| Skill runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | 0 | 6 collected; 6 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| Knowledge binding/streaming | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | 0 | 45 collected; 45 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| Exact AS-04 lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | 0 | `All checks passed!` |

## Supplemental and Focused Test Receipts

All requested supplemental suites also ran with zero skips. The six primary
pytest gate invocations above and below contain 94 passing tests in total.

| Check | Exact command | Exit | Result |
| --- | --- | ---: | --- |
| PostgreSQL migration contract | `uv run pytest -q --no-cov tests/database/test_agent_skill_knowledge_migration.py` | 0 | 4 collected; 4 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| Knowledge revision fingerprint | `uv run --package knowledge-service pytest -q --no-cov tests/services/knowledge/test_dataset_revision_fingerprint.py` | 0 | 5 collected; 5 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| Production runtime resolver | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py` | 0 | 8 collected; 8 passed / 0 skipped / 0 failed; one Starlette deprecation warning |
| C04 mixed-mode node | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agentloop_streaming_first_contract.py::test_agent_mixed_knowledge_auto_retrieves_and_tool_mode_stays_model_driven` | 0 | 1 collected; 1 passed / 0 skipped / 0 failed |
| C04 all-auto failure node | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agentloop_streaming_first_contract.py::test_agent_auto_knowledge_failure_stops_before_model_fallback` | 0 | 1 collected; 1 passed / 0 skipped / 0 failed |

I additionally reran four provider-free focused nodes in one invocation:

- per-Dataset sealed executor configuration;
- Assistant per-Dataset mapper configuration;
- production composition DB-backed policy wiring; and
- bound-Knowledge failure without a runtime resource policy.

That invocation exited 0 with 4 passed / 0 skipped / 0 failed. The focused
results are duplicate, targeted reproductions and are not added to the 94-test
primary-suite total.

`git diff --check` exited 0. `reports/agent-studio/as-04-skill-kb-golden.json`
parsed successfully as JSON and reported 10 cases, `status=passed`,
`provider_calls=0` and `historical_replay_claimed=false`.

## C01-C05 Disposition

| Finding | Iteration-4 disposition | Independent basis |
| --- | --- | --- |
| AS04-C01 | closed in its original scope | Migration 076 maintains an authoritative per-Dataset revision for retrieval-effective content/hierarchy changes; malformed or missing catalog fingerprints make bound Knowledge unavailable. Migration, catalog and Agent provenance tests pass. |
| AS04-C02 | closed | The catalog hashes a closed allowlist of retrieval-effective non-secret configuration. Config-only changes move the fingerprint; credential rotation does not. URL userinfo, query and fragment are excluded from endpoint identity and redacted output. |
| AS04-C03 | closed | Document/Segment triggers compare explicit retrieval-effective projections and exclude progress, error, timing, count/hash, hit and audit telemetry. Agent run provenance hashes authoritative fingerprints plus sealed config rather than mutable catalog telemetry. |
| AS04-C04 | closed | The production streaming AgentLoop now performs pre-provider auto retrieval. `KBSearchExecutor` fans out every auto Dataset using its sealed values; tool mode remains model-driven; off is not executable. Both specified provider-free nodes pass, including `model_calls=0` on total auto failure. |
| AS04-C05 | closed | `main.py` calls `_configure_agent_runtime_resource_policies` after DB connect and injects the returned tenant policy into `AssistantService`; the real DB-backed resource service resolves current tool policy and caller-specific Dataset ACL. Production-composition tests prove platform/Skill/implicit-KB/Dataset survival, non-expansion and revoke/missing-policy denial. |

## Security, Isolation, Compatibility and Provenance Judgment

- Tenant Skill uploads remain instruction-only and server-normalized to exact
  `db://<skill_id>/<version_id>` identities. Source, bundled, filesystem,
  network and executable forgery is rejected; persistence errors are not
  reported as success.
- Skill catalog/runtime access is scoped by tenant, user and exact version.
  Full canonical content and hash are checked at runtime; disabled, deleted,
  revoked, missing or corrupt exact versions fail before model/tool execution.
- Skill-declared permissions remain requirements beneath the AS-02 signed and
  current-policy upper bound. Selection, bridge registration, visible tool
  definitions, cache lookup and invocation can only reduce authority.
- Dataset authorization is tenant- and verified-caller-scoped at save, publish
  and run. Current soft deletion/revocation overrides immutable bindings, and
  accessible-but-unbound Dataset names cannot enter Agent context.
- The internal Knowledge tool is derived only from an active authorized Dataset
  binding, then intersected with current policy. Policy outputs containing
  forged tools or Dataset IDs are intersected back to the signed Snapshot.
- `auto` evidence is inserted as untrusted lower-priority context, not platform
  instructions. A failed all-auto retrieval cannot silently fall back to a
  model answer. Tool-mode calls remain bounded to authorized Dataset IDs and
  sealed retrieval configuration.
- Provenance records live latest content and retrieval-effective fingerprints,
  excludes credential values and operational telemetry, and explicitly retains
  `historical_replayable=false`. It does not claim historical content replay.
- Agent-only mapping is reached through the signed Agent route; generic
  Assistant input remains unable to inject reserved Agent fields. Existing
  bundled `skill-create` read/test and streaming terminal-contract cases pass;
  no Agent context preserves the built-in Assistant boundary.
- Reviewed code/tests and the golden contain no live API key, provider token,
  database password, JWT or generated `.env` value. This review did not read or
  change provider credentials.

## Residual Risks and Evidence Boundary

- No real external model, embedding provider or production Knowledge deployment
  was called. This approval is not a provider-readiness, deployment-readiness or
  production-load claim; `provider_calls=0` is intentional Phase evidence.
- Historical live Dataset content remains non-replayable. Fingerprints support
  provenance and drift localization only; deterministic replay still requires
  fixed fixtures or retained retrieval evidence.
- The secret-free endpoint identity intentionally retains URL path. Deployments
  should continue forbidding credentials embedded in URL paths; the reviewed
  code removes conventional userinfo, query and fragment credentials.
- Tenant policy and live catalog/ACL checks can intentionally deny or reduce a
  signed capability. Availability/latency of those dependencies remains an
  operational concern, but uncertainty does not expand authority; bound
  Knowledge uncertainty returns the stable unavailable error before provider
  execution.
- I did not independently rerun Docker/Compose health, the Actor's live
  PostgreSQL production probe, AHR aggregate, local service memory or any live
  HTTP/provider smoke. Those Actor receipts are not used as proof of external
  readiness and are not required to close C04/C05 on this local Phase gate.
- Terminal same-build whole-demand release regression remains AS-09 work. This
  Phase approval cannot substitute for AS-09 or authorize deployment.

The obsolete unsupported whole-Harness `validate_harness_prd.py --strict` was
not run, consistent with the user's narrow waiver. No current test, security,
isolation, provenance, regression or independent-review gate was waived.

## Final Verdict

`approved`

AS04-C01 through AS04-C05 are closed on the reviewed iteration-4 source. The
required and supplemental local gates all pass with zero skips, the production
Agent composition now retains authorized exact resources without expansion,
and the actual AgentLoop executes/fails Knowledge according to the sealed
per-Dataset mode before provider use. AS-F005 may be promoted by the
orchestrator after this artifact is consumed and the supported claim check
passes; AS-05 must not be treated as unlocked solely by this file.
