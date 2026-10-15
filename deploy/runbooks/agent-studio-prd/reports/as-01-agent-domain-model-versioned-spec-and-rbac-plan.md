# AS-01 Agent Domain Model, Versioned Spec, and RBAC Execution Plan

- **Phase:** AS-01 - Agent Domain Model, Versioned Spec, and RBAC
- **Feature:** AS-F002
- **Status:** in progress
- **Date:** 2026-07-18
- **Contract source:** `docs/agent-studio-prd/phase-01-agent-domain-model-versioned-spec-and-rbac.md`

## Scope Lock

This is the Phase contract rendered as an execution checklist, not a new product plan. AS-01 adds only the additive Agent persistence, repository, API, RBAC, OpenAPI, and test slice required by AS-F002. It does not wire Agent execution, modify Assistant runtime behavior, add Studio UI, or implement MCP, Skills, Knowledge execution, Eval publishing, or delivery channels.

AS-00 is verified: AS-F001 is passing, its fresh independent Critic approved the final evidence, and the strict AS-00 completion gate exited zero with quality score 100.

## Baseline and Ownership

- Target branch: `main` at `945eb2225d644093802bf5f9d75ca4d9dbad6a8d`; the locally recorded `origin/main` remains the same commit.
- Next free root migration number: `071`, after `070_eval_live_regression.sql`.
- Existing unrelated user changes are present outside AS-01 paths, including repository settings, workflows, Assistant memory code, runbooks, and script tests. They are preserved and excluded from this Phase's diff, validation, staging, and rollback.
- Inherited AS-00 files remain dirty by design and are preserved as the dependency implementation.

## Contract-Derived Execution Order

1. Add `071_agent_studio_domain.sql` with tenant-explicit Agent, member, Draft, normalized binding, Version, Publication, publish-event, and hashed token tables; use composite tenant/object foreign keys, additive indexes, immutable-Version and last-Owner database guards, and idempotent DDL.
2. Add `DatabaseAgentRepository` with tenant-scoped queries, role checks, atomic create/copy/Draft/version mutations, deterministic spec hashes, normalized bindings, soft delete/archive behavior, audit writes, and one-time raw token generation with hash-only persistence.
3. Add typed Agent schemas and `/api/v1/agents` routes for create/list/read/edit/copy/archive/soft-delete, Draft ETag/If-Match updates, validation, immutable Version creation/listing, and member ACL management; register the router without changing existing routes.
4. Add the three exact required test files. Exercise the migration twice against an isolated PostgreSQL schema, direct cross-tenant child inserts, immutable Version and last-Owner guards, API CRUD/pagination/copy/archive/conflict behavior, role permissions, redaction, and cross-tenant non-disclosure.
5. Run every Phase-required command plus OpenAPI inspection and diff hygiene. Record exact results in the Actor report, update AS-F002 only on a genuine pass, then obtain a fresh independent Critic before running the AS-01 strict completion gate.

## Stable Data and API Shape

- Agent identity is `(tenant_id, agent_id)` and tenant-local slug; UUID alone never authorizes access.
- Each Agent has one mutable Draft with monotonically increasing revision and a strong ETag; stale writes return `409` with `AGENT_DRAFT_CONFLICT` and the current revision.
- Version content, normalized capability/Knowledge bindings, schema version, and deterministic spec hash are immutable after insertion.
- Owner, Editor, and Viewer permissions are enforced in the service; the database prevents removal or demotion of the last Owner.
- Copy creates a new identity, Draft, and sole Owner membership while excluding ACLs, sessions, memory, publications, tokens, secrets, and unresolved resource bindings.
- Delete is soft; archive and publication policy changes are explicit and audited. No destructive migration or physical history deletion is introduced.
- Token persistence contains only a SHA-256 hash; raw token material may be returned once by a future authorized call but is never serialized by read/list APIs.

## Validation and Evidence

Required commands are copied exactly from the Phase contract:

```bash
uv run pytest -q --no-cov tests/database/test_agent_studio_migrations.py
uv run pytest -q --no-cov tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py
uv run ruff check src/api/v1/agents.py src/api/schemas/agents.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py tests/database/test_agent_studio_migrations.py tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py
uv run pytest -q --no-cov tests/integration/test_gateway_boot.py tests/security/test_management_api_authorization.py
```

The migration test uses only the authorized local dev PostgreSQL container and an isolated disposable schema. Redis, Qdrant, Gateway, Assistant, browser, and provider services are unnecessary for this Phase. Docker memory is sampled before and after the command and must remain below the user's 3.5 GiB operational stop line. No API Key is read, modified, or required.

OpenAPI evidence must show Agent routes and stable conflict/error schemas without `token_hash`, secret values, or internal resource references. Browser screenshots are not applicable because AS-01 adds no frontend route.

## Minimal Change and Rollback

The only product edit paths are the migration, Agent repository, Agent schemas/routes, router registration, repository export, and the three named tests. Application rollback disables/removes the Agent router and repository construction while retaining additive tables and immutable audit/version history. Any schema repair is forward-only; the Phase does not drop tables, truncate data, rewrite `service_id`, or mutate existing Assistant rows.
