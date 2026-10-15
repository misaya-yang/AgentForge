# AS-04 Skills and Knowledge Version Bindings Actor Report

**Phase:** AS-04 — Skills and Knowledge Version Bindings  
**Feature:** AS-F005  
**Status:** passed — all Actor gates, iteration-4 independent Critic, and supported phase claim check passed  
**Date:** 2026-07-18  
**Actor:** primary implementation agent

## Summary

AS-04 now gives Agent Versions exact immutable instruction-only Skill bindings
and normalized Knowledge Dataset bindings without weakening the AS-02 runtime
upper bound. Tenant uploads cannot choose executable/source entrypoints. The
server persists the full canonical SKILL.md, assigns an exact
`db://<skill_id>/<version_id>` identity, hashes all executable instructions and
metadata, and authorizes every load by tenant, user and exact version.

The runtime loads the exact full Skill version into a request-scoped registry
and tool overlay before the current turn's model tool snapshot. A bound Skill is
therefore available on its first turn but cannot leak into another request or
the process-global registry. Skill permissions remain requirements beneath the
immutable AS-02 allowlist; they never register new platform authority.
Disable, delete, revoke, missing version and the independent Agent Skill kill
switch fail before model/tool execution with stable `AGENT_SKILL_UNAVAILABLE`.

Agent Draft and Version Knowledge bindings are normalized rows with stable
Dataset IDs and closed retrieval config. The Gateway Snapshot retains exact
per-Dataset `mode`, `top_k`, `threshold` and `include_images` values with a
deterministic aggregate scheduler. AgentLoop executes every `auto` binding
before the model, exposes the Knowledge tool only for `tool` bindings, and
omits `off` bindings; both paths consume the sealed per-Dataset map, so model
arguments and cache reuse cannot widen it. Authorization is checked at save,
publish and every run through the production DB-backed resource policy wired
by the Assistant composition root. Each run fingerprints the current live catalog and records
`content_mode=live_latest` plus `historical_replayable=false`; this is
provenance/drift evidence, not a claim that historical Dataset content can be
replayed. The catalog hash covers retrieval-effective non-secret config and
transactional content revisions, while credentials, ingestion telemetry and
derived counters do not move provenance. Missing, revoked or deleted bindings fail closed as
`AGENT_KNOWLEDGE_UNAVAILABLE`, and unbound accessible Dataset names do not enter
the Agent context.

All four exact Phase commands pass on the corrected frozen Actor source. Migrations 075 and 076
were also applied to and exercised against the repository-owned local
PostgreSQL service, deterministic and live no-provider flows passed, the
existing Assistant regression gate passed, and all eight services remained
healthy at about 727 MiB. The fresh iteration-4 Critic approved C01-C05 closure,
AS-F005 is `passing`, and the supported Phase claim check exits zero.

## Plan Followed and Scope

- Fixed plan:
  `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-plan.md`.
- Deviation: none from the AS-04 product/architecture contract. Per the user's
  explicit direction, the obsolete validator's unsupported whole-Harness
  strict migration is not treated as a product gate. Required tests, security,
  regression, live evidence and independent Critic are not waived.
- No AS-05 UI, eval/publishing workflow, Hosted/Embed/Runtime API, deployment,
  commit, push, external provider call or API-key change belongs to this slice.

## Main Change Groups

| File/group | Result |
| --- | --- |
| `database/migrations/075_agent_skill_knowledge_bindings.sql`, `076_agent_knowledge_content_revision.sql` | Forward-only tenant/user Skill identities and immutable versions; normalized Draft/Version Skill and Knowledge bindings; transactional statement-level Dataset revisions over explicit content, availability and hierarchical-retrieval fields while excluding telemetry |
| `apps/knowledge-service/.../dataset_service.py`, `persistence/database.py` | Authorized catalog SHA-256 contract over content revision plus allowlisted retrieval-effective non-secret config, and successful-reindex revision advance without changing retrieval/index algorithms |
| `packages/ai-gateway-core/.../skills` | Canonical instruction-only parser/serializer, server-owned identities, authoritative persistence, exact authorization, scoped cache/runtime view and complete-content reconstruction |
| `packages/ai-gateway-core/.../agent_repository.py`, `agent_resource_resolver.py` | Draft/Version normalization, exact Skill sealing and save/publish/run Dataset authorization with stable failures |
| `src/api/v1/skills.py` and composition wiring | Tenant/user-scoped honest CRUD, stable redacted errors, dangerous permission rejection and read-only bundled Skill compatibility |
| `src/api/v1/agent_runtime.py`, `apps/assistant-service/.../chat.py`, `agent_loop.py`, `tool_invoker.py`, `builtin_tools.py`, `core/skills` | First-turn exact Skill load, request-local tool overlay, pre-provider revoke/flag failure, exact per-Dataset retrieval/image propagation and execution, config-aware cache keys, and authoritative bound-only Knowledge provenance |
| AS-04 tests and golden | API/isolation/entrypoint, exact runtime binding, Knowledge ACL/provenance, PostgreSQL immutability and compatibility/revocation evidence |

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 honest Skill persistence/isolation | passed | Required API/isolation gate `26 passed`; PostgreSQL proves same-name cross-tenant/cross-user isolation, immutable versions and failure-before-catalog behavior |
| R2 exact instruction-only execution | passed | Required Skill Runtime gate `6 passed`; forged entrypoints and dangerous permissions rejected; first-turn exact tool uses a per-request overlay and full immutable content |
| R3 authorized normalized Knowledge | passed on iteration-4 source | Required Knowledge/streaming gate `45 passed`; migration `4 passed` proves content and hierarchical edits advance while telemetry does not; catalog `5 passed` proves config sensitivity, credential exclusion and missing-revision denial; resolver `8 passed` proves production policy wiring and exact per-Dataset transport |
| R4 revocation/degradation/compatibility | passed | Stable pre-provider Skill/Knowledge errors, `AGENT_STUDIO_SKILLS_ENABLED=false`, built-in `skill-create` read-only compatibility, AHR gate and live no-provider isolation `6 passed` |

## Required Validation Evidence

| Gate | Exact command | Final Actor result |
| --- | --- | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | exit 0: `26 passed`, one dependency deprecation warning, zero skips |
| Skill Runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | exit 0: `6 passed`, one dependency warning, zero skips |
| Knowledge binding/streaming | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | exit 0: `45 passed`, one dependency warning, zero skips |
| Exact AS-04 lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | exit 0: `All checks passed!` |

The four commands above were rerun after the last source change; earlier green
runs are not substituted for this final frozen-source result.
One intermediate finalization run of the Skill Runtime gate returned
`2 failed, 4 passed` because the new aggregate-mode verifier rejected a legacy
empty-Dataset Snapshot with `mode=off`. The verifier now preserves a signed root
mode only when there is no per-Dataset map to contradict it; the affected gate
was rerun twice at `6 passed`. The failed run is retained here as corrected
history and is not counted as completion evidence.

## Supplemental Validation and Live Evidence

| Gate/check | Actor result |
| --- | --- |
| PostgreSQL contract | `uv run pytest -q --no-cov tests/database/test_agent_skill_knowledge_migration.py` exited 0: `4 passed`; the isolated schema applies migrations 075 and 076 twice and proves immutability, composite isolation, exact sealing, save/publish/run ACL, revoke behavior, same-count content and hierarchy sensitivity, bulk-statement coalescing and telemetry exclusion |
| Knowledge revision contract | `uv run --package knowledge-service pytest -q --no-cov tests/services/knowledge/test_dataset_revision_fingerprint.py` exited 0: `5 passed`; content/config and secret-free endpoint identity changes move the hash, API-key/URL-credential rotation does not, returned endpoint URLs drop userinfo/query/fragment, missing revision is omitted and successful reindex advances |
| Skill/Agent adjacent regressions | Agent API/Runtime Envelope integration bundle `39 passed`; Runtime resolver/isolation/allowlist bundle `25 passed`; focused resolver map `8 passed` |
| Assistant compatibility gate | `make verify-assistant-runtime-dev` passed: AHR-01 `33`, AHR-02 `77`, AHR-03 `8`, AHR-04 `98`, golden pass rate `1.0` |
| Live Assistant isolation | explicit local stub plus generated local account and valid `qwen3.7-plus` selector: `make test-isolation` exited 0 with `6 passed`, zero skips and zero provider calls; both application containers were then restored to `stub=false` with provider inputs empty |
| Working-tree integrity | `git diff --check` exited zero; no unrelated user-owned changes were reset, stashed or overwritten |
| Runtime ownership/health | every `ai-gateway-*` Compose label points to `/Users/yang/projects/AI--Platfform`; 8/8 services healthy after serial hot update/restart |
| Production composition probe | The running Assistant image imported the production composition root, opened its real PostgreSQL pool, wired both policy services, retained only signed-and-current tools/Dataset authority, rejected forged expansion, observed Dataset soft-delete revocation, and removed all temporary rows; exit 0 with no provider or secret output |
| Runtime memory | backend about 100 MiB, Assistant 111 MiB, PostgreSQL 102 MiB, Knowledge 144 MiB, Qdrant 186 MiB; total stack about 725-727 MiB, below the operator's 3.5 GiB ceiling |

Live local evidence used only repository-owned containers and local database
credentials without printing secret values:

- The project migration tracker applied/recorded 074, 075 and 076 successfully;
  the final status reports no pending migration.
- A production-composition probe ran inside the healthy Assistant container
  against the actual local PostgreSQL service. It verified real
  `_configure_agent_runtime_resource_policies` wiring, exact non-expanding tool
  policy, current Dataset ACL and deletion revocation. Its uniquely scoped rows
  were deleted in `finally`, and a follow-up count was zero.
- A real PostgreSQL rollback probe produced revisions
  `0 -> 1 -> 1 -> 2 -> 3 -> 3 -> 4` for Document insert, Document
  progress/error/timing/count telemetry, Segment insert, hierarchical
  `level/parent/summary/page` edit, Segment hit/count/hash telemetry, and
  Document content edit. The entire transaction rolled back. Isolated tests
  separately prove bulk insert/delete coalescing and same-count text edits.
- Authenticated Skill CRUD passed upload, redacted list, exact detail, metadata
  update, enable/disable and soft delete. `exec:shell` PATCH returned 422.
  Updating title/instructions produced a new exact version and changed the
  canonical content hash.
- Exact database load, request-scoped bridge and invocation returned the bound
  instructions while the global tool registry remained empty.
- A private Dataset without permission was rejected at Agent save; after a
  grant, Draft/Version rows were normalized. Permission removal denied publish,
  and Dataset soft deletion denied a new run without provider execution.
- The bundled `skill-create` remained redacted in list output, readable and
  testable through its legacy surface, and read-only through tenant mutation
  routes.

No real external model, embedding provider or production Knowledge deployment
was called. Those inputs are unnecessary for the Phase's authorization,
immutability and provenance contracts; `provider_calls=0` is explicitly
recorded rather than presented as provider readiness evidence.

## Golden, Security, and Failure Matrix

- Durable golden:
  `reports/agent-studio/as-04-skill-kb-golden.json`.
- Every case records its exact deterministic/PostgreSQL test nodes and preserves
  `historical_replay_claimed=false`.
- Entrypoint/source, executable permission, tenant/user/version collision,
  partial Dataset authorization, disable/delete/revoke, kill switch, missing
  live catalog and built-in compatibility paths are all negative-gated.
- API errors and reports contain no API keys, provider tokens, database
  passwords, JWTs or generated `.env` values.

## Rollback and Residual Risk

- `AGENT_STUDIO_SKILLS_ENABLED=false` removes exact Agent Skill execution while
  retaining the sealed binding and built-in Assistant behavior. The test proves
  failure before the model sees tools.
- Existing Knowledge feature controls and the normalized resolver remove Agent
  Dataset access without deleting historical binding/provenance rows. Current
  authorization and soft-delete state always override the immutable reference.
- Skill/Knowledge caches are request/scoped and authorization is rechecked; no
  rollback deletes migration history.
- Live Knowledge state `available` means the bound Dataset was present in the
  current catalog before retrieval. It does not mean a retrieval succeeded;
  retrieval completion remains a separate runtime event.
- Historical Dataset content replay remains unavailable. The recorded revision
  fingerprint supports provenance and drift detection only.

## Iteration-1 Critic Remediation

The first independent Critic reran every prescribed command with zero skips
but returned `changes_requested`. Its preserved verdict is
`docs/agent-studio-prd/reports/as-04-critic-verdict-iteration-1.md`.

| Finding | Iteration-2 correction and evidence |
| --- | --- |
| AS04-C01 same-count content edits retained one metadata-derived hash | Migration 076 adds a transactional per-Dataset `content_revision`; statement-level transition-table triggers advance once per affected Dataset for meaningful Document/Segment insert, update or delete, ignore telemetry, and include hierarchical retrieval fields. The successful-reindex path advances explicitly. AgentLoop requires an exact `sha256:` fingerprint for every bound Dataset and otherwise records unavailable before provider execution. PostgreSQL `4 passed`, Knowledge catalog `5 passed`, required Knowledge/streaming `45 passed`, live rollback probe `0 -> 1 -> 1 -> 2 -> 3 -> 3 -> 4`, and AHR `33/77/8/98` plus golden all pass. |

No historical content or deterministic replay claim was added. The correction
is a revision-contract change only; Knowledge indexing and retrieval algorithms
were not refactored.

## Iteration-2 Critic Remediation

The second fresh Critic independently reran the four prescribed commands plus
the PostgreSQL and catalog suites with `76 passed`, zero skips, and returned
`changes_requested`. Its preserved verdict is
`docs/agent-studio-prd/reports/as-04-critic-verdict-iteration-2.md`.

| Finding | Iteration-3 correction and evidence |
| --- | --- |
| AS04-C02 config-only retrieval changes retained one catalog hash | The catalog fingerprint now hashes a closed, recursive projection of retrieval-effective Dataset config. API keys and credential-like fields are excluded rather than redacted into the digest; embedding endpoint identity retains only scheme/host/port/path, excluding URL userinfo/query/fragment. Tests prove mode/threshold/rerank and endpoint changes move the hash, credential rotation does not, and a config-only catalog change moves the next Agent run provenance hash. |
| AS04-C03 Document telemetry moved content revision and mutable catalog telemetry polluted the run hash | Migration 076 compares explicit Document/Segment projections: progress, errors, timing, derived counts/hashes, hit counters and audit timestamps are excluded; content, availability, citations, images and hierarchical fields are included. AgentLoop hashes only the authoritative catalog fingerprint plus sealed retrieval config, not `updated_at` or counts. PostgreSQL and run-hash tests prove both positive and negative sides, including the live rolled-back sequence. |
| AS04-C04 only the first Dataset retrieval config survived Snapshot/runtime and image policy was not executed | Agent Spec rejects unknown fields and invalid/duplicate Dataset identities. Gateway emits an exact sorted `retrieval.by_dataset` plus deterministic aggregate mode. Assistant verifies exact Dataset/key coverage, excludes `off` bindings from execution, applies `auto` bindings to automatic RAG, retains `tool` bindings for KB calls, passes the sealed map through invocation metadata, includes it in cache/trace identity, and applies each active Dataset's `top_k`, `threshold` and `include_images` regardless of model arguments. Snapshot, resolver and both executor-path tests cover distinct mixed-mode Datasets. |

The Actor also inspected the production Segment schema while reproducing C03
and added `level`, `parent_segment_id`, `summary`, `page_start` and `page_end`
to the positive revision projection because those fields participate in
hierarchical retrieval. This closes an adjacent instance of the same finding;
it does not expand into AS-05.

## Iteration-3 Critic Remediation

The third fresh Critic independently reran the prescribed and supplemental
suites with `91 passed` across six invocations, zero skips, and returned
`changes_requested`. Its preserved verdict is
`docs/agent-studio-prd/reports/as-04-critic-verdict-iteration-3.md`.

| Finding | Iteration-4 correction and evidence |
| --- | --- |
| AS04-C04 the production AgentLoop did not actually schedule `auto` retrieval, and Knowledge-only `tool` mode exposed no callable KB tool | AgentLoop now directly schedules every exact `auto` binding before the provider through the existing KB executor, applying sealed `top_k`, `threshold` and `include_images` values per Dataset. A `tool` binding makes the KB tool visible for model selection; `off` remains absent. Mixed-mode and all-auto-failure tests prove exact fan-out and stable pre-provider failure (`model_calls=0`). The original provider-free reproduction now records one catalog call, one retrieval call with the exact Dataset config, one model call and `run_finished`. |
| AS04-C05 production `main.py` supplied no current resource/tenant policy, reducing signed Agent resources to empty | The Assistant composition root now wires a DB-backed Agent resource policy and the existing tenant tool policy immediately after mandatory DB connection. Request mapping resolves the verified caller against current tenant tool policy and Dataset ACL, can only intersect the signed Snapshot, and fails closed on unavailable policy or revoked/missing bound Knowledge. The focused production-composition test and running-container PostgreSQL probe prove exact Skill/platform/Knowledge survival, forged expansion denial and current revocation. |

The final Actor source passes the required `26/6/45` suites, focused resolver
`8`, migration `4`, catalog `5`, adjacent resolver/allowlist bundle `25`, and
AHR `33/77/8/98` with golden pass rate `1.0`.

## Feature Oracle and Independent Critic

| Feature | Current status | Actor recommendation | Evidence |
| --- | --- | --- | --- |
| AS-F005 | `passing` | completed after independent Critic approval and supported claim check | this report, canonical Critic verdict and `reports/agent-studio/as-04-skill-kb-golden.json` |

- Requested Critic scope: the fixed AS-04 Phase and Feature Oracle, migration,
  parser/repository/API/runtime implementations, exact request-scoped tool
  authority, Knowledge save/publish/run ACL, provenance honesty, all four
  required commands, PostgreSQL/compatibility evidence, rollback and minimal
  scope.
- Canonical Critic artifact:
  `docs/agent-studio-prd/reports/as-04-critic-verdict.md`.
- Current canonical verdict: `approved` on iteration 4. Iterations 1, 2 and 3
  are preserved as `changes_requested`; the orchestrator consumed the approval,
  transitioned AS-F005 and unlocked AS-05 only after the supported claim check.

## Handoff

AS-04 is complete. Preserve all four Critic artifacts and the final Actor/golden
evidence. AS-05 may consume the exact Skill/Knowledge catalog, effective
configuration and failure contracts; it must not weaken Runtime authorization,
claim historical Knowledge replay, or enter AS-06 before its own independent
Critic and supported claim check.
