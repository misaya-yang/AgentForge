# AS-01 Agent Domain Model, Versioned Spec, and RBAC Report

- **Phase:** AS-01 - Agent Domain Model, Versioned Spec, and RBAC
- **Feature:** AS-F002
- **Status:** passed
- **Date:** 2026-07-18
- **Plan:** `docs/agent-studio-prd/reports/as-01-agent-domain-model-versioned-spec-and-rbac-plan.md`
- **Independent Critic:** approved in `docs/agent-studio-prd/reports/as-01-critic-verdict.md`

## Outcome

The additive AS-01 persistence and management slice is implemented. Migration 071 creates tenant-explicit Agent identities, member ACLs, optimistic Drafts, normalized Draft Knowledge bindings, immutable Versions and Version bindings, Publication primitives, append-only publish events, and hash-only API token records. Composite tenant/object foreign keys reject cross-tenant children in the database rather than relying only on API filtering.

`DatabaseAgentRepository` implements tenant-scoped create/list/read/edit, Draft ETag revision updates, validation, immutable Version creation/listing, member changes, last-Owner protection, copy, archive, soft delete, audit writes, and one-time token material with hash-only storage. Typed `/api/v1/agents` routes expose the AS-01 lifecycle and role boundary without wiring Agent runtime execution or UI.

All Actor-required commands pass with no skips: the migration contract is `9 passed`, API/RBAC is `13 passed`, Gateway regression is `14 passed`, and Ruff exits zero. Full live Gateway OpenAPI contains nine Agent paths and does not expose `token_hash` or `secret_ref`. A repository-owned eight-service Compose stack also exercised generated-password login, Agent CRUD/Draft conflict/Version flows, Viewer read-only ACL and cross-tenant non-disclosure against the migrated development database. A fresh independent Critic reran all four required commands, checked the live OpenAPI/runtime and approved AS-01/AS-F002. The orchestrator then ran the strict phase-scoped completion gate, which exited zero with quality score 100. AS-01 is verified and AS-02 is unlocked; AS-F003 through AS-F010 and the terminal whole-demand gate remain incomplete.

## Plan Followed

1. Verified AS-00 was passing and its strict completion gate had exited zero before changing loop state to AS-01/AS-F002.
2. Revalidated the branch and confirmed `071` is the next free root migration after `070_eval_live_regression.sql`.
3. Read only the four Phase-named hot-path groups plus the targeted architecture data/API and PRD role/lifecycle sections required to resolve exact field semantics.
4. Implemented migration, repository, schemas/routes, router registration and the three named tests; no Assistant runtime, Web UI, MCP, Skill, Knowledge execution, Eval publish, deployment, or existing session semantics were changed.
5. Applied migration 071 twice to isolated disposable PostgreSQL schemas and ran direct failure probes before recording the final passing evidence.
6. Under the user's explicit local-container/database/password authorization, validated the complete repository-owned Compose stack, the full ordered migration runner, generated bootstrap-admin login, live Agent HTTP/RBAC behavior and the rebuilt ARM64 Gateway image without reading or changing an API Key.

## Baseline and Dirty-Tree Boundary

- `HEAD`, local `origin/main`, and local `origin/HEAD`: `945eb2225d644093802bf5f9d75ca4d9dbad6a8d`.
- `HEAD..origin/main`: no locally recorded commit or path delta; no fetch, pull, branch switch, stash, or reset was run.
- AS-01 began after unrelated user changes appeared in repository settings, workflows, Assistant memory code and runbooks. Those user-owned paths were preserved.
- Inherited AS-00 changes in `agent_loop.py`, `tool_invoker.py`, and two allowlist tests remain preserved.
- The user separately and explicitly requested open-source container/quickstart hardening in the same worktree. Dockerfiles, Compose, env/init/deploy scripts, README/AGENTS and their script tests are a distinct operator-facing change set; they do not expand the AS-01 Agent domain/API architecture and have separate packaging/runtime evidence below.
- The root `docs/*` ignore rule still makes this Harness local/ignored. No commit or push was requested or performed.

## Additive Schema Contract

| Entity | Tenant/composite boundary | Mutation rule | AS-01 evidence |
| --- | --- | --- | --- |
| `agents` | `(tenant_id, agent_id)` identity; live slug unique per tenant | metadata mutable; archive/delete are explicit soft states | repository/API tests plus tenant-filtered list/get |
| `agent_members` | composite FK to Agent; tenant/principal/member key | Owner/Editor/Viewer; trigger rejects last Owner removal/demotion and retargets `owner_id` to a remaining Owner | live PostgreSQL last-Owner test and RBAC HTTP tests |
| `agent_drafts` | composite FK to same-tenant Agent; one Draft per Agent; composite current-Draft FK | revision increases atomically; stale expected revision fails without overwrite | repository live-DB test and HTTP ETag conflict test |
| `agent_draft_knowledge_bindings` | composite FKs to same-tenant Draft and Dataset | mutable with Draft; tenant Dataset presence required | direct cross-tenant insert rejection |
| `agent_versions` | composite FK to Agent and exact source Draft | write-once database trigger; deterministic schema/spec hash | live immutable mutation and later-Draft drift tests |
| `agent_version_capabilities` | composite FK to Version; tenant/resource reverse index | write-once with Version | direct cross-tenant and immutable mutation tests |
| `agent_version_knowledge_bindings` | composite FKs to Version and Dataset | write-once with Version | direct cross-tenant and immutable mutation tests |
| `agent_publications` | same-tenant Agent and same-Agent Version composite FKs | mutable pointer/status primitive for later publish Phase | direct cross-tenant insertion rejection; no publish behavior claimed |
| `agent_publish_events` | same-tenant Publication/Agent/Version composite FKs | append-only trigger | live immutable delete rejection |
| `agent_api_tokens` | same-tenant Publication composite FK | hash-only persistence; revoke fields remain mutable | raw/hash inequality and redacted return test |

Migration 071 adds no destructive operation, does not alter or reinterpret `service_id`, and does not rewrite existing Assistant rows. It uses the existing ordered root migration runners (`database/cli.py` and `scripts/new/migrate.sh`) and was tested twice per disposable schema. The application rollback contract is to disable/remove the new routes while retaining the additive tables, immutable Versions, and audit history.

## API, Concurrency, and Role Contract

| API | Viewer | Editor | Owner / tenant Admin | Stable behavior |
| --- | --- | --- | --- | --- |
| `GET/POST /agents` | list authorized objects | list/create own | list/create | tenant-scoped cursor page; name/status/Owner/channel filters |
| `GET/PATCH/DELETE /agents/{agent_id}` | read | read/edit metadata | read/edit/soft-delete | inaccessible and cross-tenant UUIDs return the same 404 contract |
| `GET/PUT /agents/{agent_id}/draft` | read | read/edit | read/edit | strong ETag; missing precondition 428; stale revision 409 `AGENT_DRAFT_CONFLICT` with current revision |
| `POST /agents/{agent_id}/validate` | read-only validation | same | same | field/resource error list; no runtime execution |
| `GET/POST /agents/{agent_id}/versions` | list | list | list/create | create pins exact Draft revision/spec hash; no update/delete API exists |
| `GET/PUT/DELETE /agents/{agent_id}/members/...` | list | list | manage | last Owner cannot be removed/demoted; principal must exist in tenant for database repository |
| `POST /agents/{agent_id}/copy` | denied | denied | create safe copy | new ID/Draft/sole Owner; excludes ACL, sessions, memory, versions, publications, tokens, secrets and all unresolved runtime/resource bindings |
| `POST /agents/{agent_id}/archive` | denied | denied | archive | Draft becomes read-only; Publication disable choice is explicit and audited |

Every mutation response includes a `request_id`. Public schemas reject credential/secret fields in Agent Spec. Version list/response shapes omit resolved internal configuration and all token hashes. Tenant Admin authority is still tenant-bound; it does not bypass the tenant predicate.

## Files Changed

### Product code

- `database/migrations/071_agent_studio_domain.sql`
- `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py`
- `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/__init__.py`
- `src/api/schemas/agents.py`
- `src/api/v1/agents.py`
- `src/api/router.py`

### Phase tests

- `tests/database/test_agent_studio_migrations.py`
- `tests/api/test_agents_api.py`
- `tests/security/test_agent_studio_rbac.py`

### Evidence and continuity

- AS-01 plan/report and targeted updates in `feature-oracle.json`, `loop-state.json`, `progress-log.md`, `source-packet.md`, `continuity-ledger.md`, `agent-handoff.md`, and `next-window-prompt.md`.

## Validation Evidence

| Check | Exact command | Final result | Gate status |
| --- | --- | --- | --- |
| Migration contract | `uv run pytest -q --no-cov tests/database/test_agent_studio_migrations.py` | Exit 0; `9 passed`; no skips. Migration executed twice in each isolated schema; direct cross-tenant, immutable, last-Owner, repository revision and hash-only token probes ran against PostgreSQL. | passed |
| Agent API and RBAC | `uv run pytest -q --no-cov tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py` | Exit 0; `13 passed`; no skips. | passed |
| Required lint | `uv run ruff check src/api/v1/agents.py src/api/schemas/agents.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py tests/database/test_agent_studio_migrations.py tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py` | `All checks passed!` | passed |
| Gateway regression | `uv run pytest -q --no-cov tests/integration/test_gateway_boot.py tests/security/test_management_api_authorization.py` | Exit 0; `14 passed`; no skips; three existing TestClient/AsyncMock warnings. | passed |
| Independent Critic | Fresh-context review in `docs/agent-studio-prd/reports/as-01-critic-verdict.md` | Approved after independently obtaining migration `9 passed`, API/RBAC `13 passed`, Ruff clean and Gateway regression `14 passed`, all with zero skips; live OpenAPI, Compose ownership/memory and diff hygiene also passed. | passed |
| AS-01 completion gate | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --strict --completion-gate --phase AS-01 --quality-score` | Exit 0; `Harness validation passed`; quality score `100 (excellent)`. | passed |
| Full live Gateway OpenAPI | HTTP inspection of `http://127.0.0.1:8080/openapi.json` | Nine `/api/v1/agents` paths present; required error/Draft/Version/member schemas present; `token_hash` and `secret_ref` absent. | supplemental passed |
| Live Agent lifecycle | Authenticated HTTP against the repository-owned Compose Gateway | Unauthenticated list 401; generated-password admin login 200; create 201; list/get/Draft/update/validate all 200; stale ETag 409 `AGENT_DRAFT_CONFLICT`; Version create 201 and list 200. | supplemental passed |
| Live ACL and tenant isolation | Admin-created second user plus Agent membership and a second local tenant | Viewer login/read 200; Viewer Draft mutation 404 `AGENT_NOT_FOUND`; removal hides the Agent; after moving the user to another tenant, object read is 404 and list contains no foreign Agent. | supplemental passed |
| Ordered quickstart migration | `scripts/new/migrate.sh --auto` against a fresh isolated database, then rerun | 67 forward migrations applied successfully; second run reported the database up to date. Gateway bootstrap against that database created a usable generated-password admin hash. | supplemental passed |
| Open-source packaging | Four exact script suites plus Compose/Bash/config checks | `79 passed`; base, build, dev and KBMS Compose renderings, Bash syntax, `make validate-example-config`, targeted Ruff and diff hygiene all exit zero. | supplemental passed |
| Rebuilt Gateway image | Serial source build and Compose recreation | Build exits zero; image is `linux/arm64`, runs as `appuser`, contains the bootstrap contract, and the recreated healthy container serves admin login and Agent reads from image `sha256:182915465264…`. | supplemental passed |
| Diff hygiene | `git diff --check` | Exit 0 across the current worktree. | supplemental passed |
| Browser | Not applicable: no Web route or frontend source changed. | Agent endpoints are proven through OpenAPI and TestClient instead of screenshots. | not applicable |
| Runtime/provider eval | Not applicable: AS-01 has no model execution. | No provider key or model call was used. | not applicable |

The first implementation-window migration attempt produced five fixture setup errors because its async fixture scope did not match the configured event-loop scope. After function-scoping the disposable schema, a second run was `4 passed, 1 failed` because the test rolled back before forcing an initially deferred composite FK. The first API run was `11 passed, 1 failed` because a legacy-secret copy fixture correctly hit the new public Spec rejection. In the resumed full-runtime window, applying migration 071 to the persistent dev schema exposed a test query that counted the same trigger in both `public` and the disposable schema; the first run was `1 failed, 8 passed`. The assertion now scopes the trigger lookup to `current_schema()`. Only the final `9 passed`, `13 passed`, `14 passed` and Ruff results above are counted as passing evidence.

The final repository-owned Compose sample ran PostgreSQL, Redis, Qdrant, Knowledge, Assistant, MCP docgen, Gateway and Frontend healthy with correct `com.docker.compose.project.working_dir=/Users/yang/projects/AI--Platfform` labels. Their aggregate sampled memory was approximately 718 MiB, far below the 3.5 GiB operational stop line; serial Gateway build host RSS was also monitored and stayed well below the limit. Local infrastructure/bootstrap secrets came only from the ignored generated `.env` and were never printed. No usable provider key was present, read, changed or required, and no model/provider call is claimed.

## Regression and Security Assessment

- Every production repository object query contains `tenant_id`; tenant Admin bypasses only membership, never tenant scope.
- Cross-tenant direct inserts are rejected for members, Drafts/current Draft, Draft Knowledge bindings, Versions, Version capability/Knowledge bindings, Publications, events, and tokens.
- Object ACL denial and cross-tenant absence collapse to `AGENT_NOT_FOUND`, preventing UUID existence disclosure.
- Draft updates lock Agent and Draft rows and compare the expected revision before changing spec/bindings. The stale-write test proves the newer edit remains intact.
- Agent row locking serializes Version numbering. Version, normalized Version bindings, and publish events are protected by database mutation triggers.
- Last-Owner mutation locks the Agent row, rejects a singleton Owner change, and retargets denormalized `owner_id` when another Owner remains.
- Copy uses the production sanitization function and fails closed on all bindings until later Phases provide a resource-authorization resolver.
- API-token raw material is generated with secure randomness and SHA-256 persisted; no list/read serializer contains the hash.
- Existing Gateway boot and management authorization tests pass. Existing Assistant runtime paths and `__builtin_assistant__` data were not edited.

## Feature Oracle Updates

- AS-F002 transitions to `passing` only after the fresh independent Critic approved migration safety, tenant/RBAC semantics, concurrency, immutability, copy/redaction behavior, all required command results, and minimal-change scope.
- This Actor report and the separate Critic verdict are the two durable Oracle artifacts. The strict phase-scoped completion gate exited zero with quality score 100, so AS-02 is unlocked.
- AS-F003 through AS-F010 remain failing; no runtime, UI, publish, channel, operations, or whole-demand completion is claimed.

## Minimal Change and Rollback

The AS-F002 implementation itself is confined to the Phase-listed migration, repository, API/schema/router, tests and evidence paths. The approximately 2,500 Agent product-code/DDL lines reflect the complete entity graph, composite constraints, role/concurrency boundary and typed API required by AS-F002; no Agent runtime or UI was added. The operator-authorized open-source packaging work is tracked separately in the same dirty worktree and changes startup/image/configuration surfaces only; it neither changes the Agent API contract nor advances AS-02 functionality.

Application rollback removes the Agent router registration and repository entry points while retaining additive tables and history. Schema rollback is forward-only: do not drop Version, event, token, or audit data. The existing Assistant remains on its original routes and `service_id` semantics throughout.

## Blockers, Deviations, and Handoff

- No Actor-side implementation or test blocker remains.
- The fresh independent AS-01 Critic approved the final frozen slice after independently rerunning all required commands, and the strict AS-01 completion gate exited zero with quality score 100.
- The complete Compose runtime is now owned by this repository root and all expected containers carry the matching working-directory label. The earlier label-free/single-PostgreSQL limitation is superseded.
- Unrelated user-owned dirty paths remain present and must continue to be preserved.
- Next action: cold-start AS-02 / AS-F003 from its Phase contract without changing the approved architecture or entering AS-03/AS-04 work.
