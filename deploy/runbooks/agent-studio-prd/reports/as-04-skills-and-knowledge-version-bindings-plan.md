# AS-04 Skills and Knowledge Version Bindings Plan

- **Phase:** AS-04 — Skills and Knowledge Version Bindings
- **Feature Oracle:** AS-F005 only
- **Status:** fixed Phase-contract execution record; Actor complete and iteration-4 independent Critic approved
- **Date:** 2026-07-18
- **Scope rule:** execute the existing AS-04 contract without replanning, shrinking, or entering AS-05 product work

This artifact transcribes the already-approved AS-04 Phase contract into an
execution record. It does not change product scope, architecture, dependencies,
acceptance gates, or completion rules.

## Dependency and Runtime Baseline

- AS-02/AS-F003 is passing with approved Runtime Envelope, capability upper
  bounds, explicit Agent dimensions and Assistant compatibility evidence.
- AS-03/AS-F004 is also passing; AS-05 remains locked until AS-04 passes.
- Current Skill upload registers in memory before persistence and suppresses DB
  failure. CRUD reads a process-global name map, database loading reconstructs
  metadata-only manifests with `db://<name>`, and updates/enables/disables do not
  provide exact immutable content-version semantics.
- Migration 037 has Skill and version rows but lacks the tenant-composite,
  immutable normalized contracts needed by Agent Version bindings. The five
  Phase-owned AS-04 test files do not yet exist and must be added; the existing
  streaming-first regression is present.
- The repository-owned eight-service Compose stack is healthy around 727 MiB.
  Docker work must verify ownership, use `COMPOSE_PARALLEL_LIMIT=1`, run
  serially, monitor memory and stop above 3.5 GiB. Provider API Keys and
  production secret changes remain out of scope.

## Requirement-to-Change Map

| Contract | Bounded implementation | Primary evidence |
| --- | --- | --- |
| R1 honest Skill persistence and isolation | Make database persistence authoritative for tenant uploads; keep platform bundled Skills separate; scope list/get/update/enable/disable/delete and caches by tenant/user/version; return stable failure instead of reporting in-memory success. | Skill API and tenant isolation tests; PostgreSQL migration/live CRUD evidence |
| R2 exact instruction-only Skill execution | Reject tenant `source`, `builtin://`, filesystem, network and executable entrypoint control; normalize uploads to immutable server-owned `db://<skill_id>/<version_id>` rows containing exact manifest and full SKILL.md content; bind/load by `skill_version_id` beneath AS-02 capability limits. | Entrypoint-policy, immutable-version Runtime and non-expansion tests; exact trace/golden evidence |
| R3 authorized Knowledge binding | Add normalized tenant-composite Draft/Version Dataset binding rows and reverse references; preserve stable Dataset IDs/retrieval config; authorize at save, publish and every run; record current live revision/provenance without claiming historical replay. | Migration, repository, Agent Knowledge Runtime and provenance/golden tests |
| R4 fail-closed revocation/degradation | Invalidate or bypass caches after Skill/Dataset disable/delete/revoke; block new Preview/publish/runtime access; distinguish retrieved, unavailable and no-binding states; retain built-in Assistant request-level Skill/KB behavior when Agent bindings are absent. | Revocation/failure matrix, streaming-first and Assistant/KB regressions, rollback evidence |

## Bounded File Groups

- `database/migrations/07*_agent_skill_knowledge_bindings*.sql`
- `packages/ai-gateway-core/src/ai_gateway_core/skills` and the bounded Agent
  repository binding hooks
- `src/api/v1/skills.py` plus only the schemas/composition wiring required for
  tenant-safe persistence
- `apps/assistant-service/src/assistant_service/core/skills`, AgentLoop and
  ToolInvoker exact-version/Knowledge authorization adapters
- Phase-owned API, entrypoint, isolation, Skill Runtime, Knowledge Runtime,
  migration, golden and revocation tests
- AS-04 report/golden plus Oracle, continuity, source, progress and handoff
  writeback after evidence exists

No MCP protocol/security change, bundled Skill content refactor, Knowledge
indexing/retrieval algorithm, Studio UI, publication channel, Hosted/Embed/API
surface, deployment, commit or push belongs to AS-04.

## Fixed Execution Sequence

1. Add forward-only normalized Skill version and Agent Draft/Version Knowledge
   binding schema with tenant-composite references, immutability and revocation
   lookup indexes; prove migration idempotence and cross-tenant denial.
2. Repair Skill upload/CRUD so persistence is authoritative, upload input is
   instruction-only, entrypoints are server-owned exact version URIs, and
   registry/cache keys cannot collide across tenants or users.
3. Load the full immutable manifest/content by exact `skill_version_id` at
   runtime and keep Skill-declared permissions below the AS-02 allowlist.
4. Resolve normalized Dataset IDs/config through current tenant/caller ACL at
   save, publish and run; record live revision/provenance and explicit
   non-replayability when historical reads are unavailable.
5. Enforce disable/delete/revoke/unavailable behavior at every new execution,
   preserve generic Assistant Skill/KB behavior when Agent context is absent,
   and add rollback flags/adapters without deleting historical rows.
6. Run every exact Phase command, supplemental PostgreSQL/migration,
   golden/revocation/regression and low-memory live evidence. Freeze the Actor
   report and request a fresh independent Critic; keep AS-F005 failing until
   approval and the phase claim check.

## Required Validation Gates

| Gate | Exact Phase command | Required outcome |
| --- | --- | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | Honest CRUD persistence, tenant/user isolation, entrypoint/source forgery denial and server-owned instruction-only normalization pass. |
| Skill Runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | Exact immutable full-content loading, old-Version non-drift and non-expanding permissions pass. |
| Knowledge binding | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | Normalized bindings, three-time ACL, config/provenance/revision/non-replayability, revocation and existing KB streaming pass. |
| Lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | Ruff exits zero on the exact Phase paths. |

Supplemental evidence must include isolated/live PostgreSQL migration behavior,
Skill content/hash and Knowledge provenance golden data, revocation/failure
matrix, rollback/feature-flag behavior, existing built-in Skill/Assistant/KB
regressions, Compose ownership/health/memory and an independent Critic.

## Rollback and Stop Conditions

- Disable Agent-bound Skills and Knowledge independently while preserving
  historical content/binding/provenance rows and the built-in Assistant's
  request-level behavior.
- Stop on hidden persistence failure, global cross-tenant registry/cache,
  arbitrary tenant executable/source entrypoints, metadata-only Skill Runtime,
  tool permission expansion, JSON-only Dataset bindings, missing save/publish/run
  ACL, fail-open revocation, deterministic replay claims without historical
  reads, wrong Compose ownership, memory above 3.5 GiB, or a required AS-05
  change.
