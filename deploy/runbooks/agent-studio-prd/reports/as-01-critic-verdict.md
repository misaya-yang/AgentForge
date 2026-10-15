# AS-01 Critic Verdict

**Phase:** AS-01 - Agent Domain Model, Versioned Spec, and RBAC  
**Feature:** AS-F002  
**Critic:** independent fresh context reviewer  
**Review mode:** fresh post-freeze review; Actor results independently rerun  
**Actor report:** `docs/agent-studio-prd/reports/as-01-agent-domain-model-versioned-spec-and-rbac-report.md`  
**Date:** 2026-07-18  
**Critic Verdict:** approved

## Decision

AS-01 and AS-F002 are approved at the independent Critic gate. The four exact
Phase commands all completed successfully with no skipped tests, the live
Gateway OpenAPI surface matched the Agent contract, and the reviewed migration,
repository, schemas, routes, and tests did not expose a blocking correctness,
tenant-isolation, authorization, concurrency, secret-handling, or rollback
defect.

This approval is scoped only to AS-01/AS-F002. It does not mark the full Agent
Studio demand complete, does not update the Oracle or loop state, and does not
substitute for the strict AS-01 completion gate or the later AS-09 same-build
regression.

## Inputs Reviewed

- `docs/agent-studio-prd/context-profile.json`
- `docs/agent-studio-prd/loop-state.json`
- `docs/agent-studio-prd/phase-01-agent-domain-model-versioned-spec-and-rbac.md`
- the updated AS-01 Actor report
- the AS-F002 Oracle entry only
- `database/migrations/071_agent_studio_domain.sql`
- `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py`
- `src/api/schemas/agents.py`
- `src/api/v1/agents.py`
- the Agent router/repository export wiring
- the three Phase-owned migration, API, and RBAC test files

The verdict was formed from a fresh source review and fresh command execution,
not by adopting an earlier Critic verdict.

## Requirement and Feature Coverage

| Area | Independent assessment |
| --- | --- |
| R1: tenant-safe domain model | Satisfied. Agent-owned tables carry tenant identity and use composite tenant/object references. Agent membership is constrained to same-tenant active user principals; unsupported group principals fail closed. |
| R2: optimistic Draft and immutable Version | Satisfied. Draft writes require a strong revision precondition and reject stale revisions without mutation. Version numbers are serialized, Version rows and bindings are sealed, and later Draft edits do not mutate a created Version. |
| R3: RBAC and lifecycle | Satisfied. Production repository coverage exercises Viewer, Editor, Owner, and tenant-admin behavior; object access is tenant-scoped; copy creates a safe new Draft with a sole Owner; archive/delete disable publications and audit the lifecycle action. |
| R4: additive migration | Satisfied. Migration 071 is transactional and additive for the reviewed slice. It does not introduce a destructive table operation or alter the inherited service identity contract. |
| AS-F002 create/edit/version | Satisfied by live PostgreSQL repository tests and API tests, including stale `If-Match` rejection and immutable Version behavior. |
| AS-F002 share/cross-tenant | Satisfied by live production-repository role-matrix and cross-tenant denial coverage plus database constraints. |
| AS-F002 copy/archive/delete | Satisfied by repository/API coverage, including copy sanitization and publication shutdown during archive/delete. |
| Secret safety | Satisfied for this phase. Closed Spec shapes, recursive secret-shaped-key rejection/redaction, safe-copy projection, hash-only token persistence, and Secret-free audit payloads were reviewed and exercised. |

## Independent Verification

All four commands required by the Phase document were run exactly as written:

1. `uv run pytest -q --no-cov tests/database/test_agent_studio_migrations.py`
   - PASS: 9 passed, 0 skipped.
   - One non-blocking Starlette `TestClient`/httpx deprecation warning.
2. `uv run pytest -q --no-cov tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py`
   - PASS: 13 passed, 0 skipped.
   - One non-blocking Starlette `TestClient`/httpx deprecation warning.
3. `uv run ruff check src/api/v1/agents.py src/api/schemas/agents.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py tests/database/test_agent_studio_migrations.py tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py`
   - PASS: `All checks passed!`
4. `uv run pytest -q --no-cov tests/integration/test_gateway_boot.py tests/security/test_management_api_authorization.py`
   - PASS: 14 passed, 0 skipped.
   - Three non-blocking warnings: the same `TestClient` deprecation and two
     pre-existing out-of-scope unawaited-`AsyncMock` warnings in dashboard/dependency code.

Supplemental evidence:

- `git diff --check` passed with no diagnostics.
- `git diff --no-index --check -- /dev/null <file>` produced no whitespace
  diagnostics for each untracked Phase-owned Agent file. Exit status 1 was the
  expected content-difference status for comparison against `/dev/null`.
- After Compose ownership was checked, every running `ai-gateway-*` dependency
  inspected was labeled with
  `com.docker.compose.project.working_dir=/Users/yang/projects/AI--Platfform`.
- The running stack was healthy and consumed about 720 MiB in the final sample,
  comfortably below the 3.5 GiB ceiling. No container, image, or volume was
  mutated.
- The live Gateway at `127.0.0.1:8080` exposed nine Agent path groups covering
  list/create, get/update/delete, archive, copy, Draft, members, validation, and
  Versions. Agent response schemas did not expose `token_hash` or
  `secret_ref`, and Draft update declares the conflict response.

## Findings

No blocking finding remains in the reviewed AS-01/AS-F002 scope.

| ID | Severity | Finding | Disposition |
| --- | --- | --- | --- |
| AS01-C-01 | Informational | The required Phase tests and lint passed with no skips, and the live OpenAPI surface matched the reviewed routes. | Supports approval. |
| AS01-C-02 | Low | The Gateway regression command emitted one dependency deprecation warning and two out-of-scope `AsyncMock` runtime warnings. They did not fail or skip tests and are not introduced by the AS-01 slice. | Non-blocking follow-up outside this verdict. |
| AS01-C-03 | Informational | Oracle/state transition, the strict AS-01 completion gate, and AS-09 same-build regression remain downstream orchestration work. | Intentionally untouched by the Critic. |

## Scope, Minimality, and Adjacent Changes

The reviewed Agent implementation is limited to the Phase-owned migration,
repository, schemas, routes, wiring, and tests. Inherited AS-00 content,
unrelated user-owned dirty paths, and separately authorized packaging/runtime
work were excluded from the feature judgment.

Adjacent bootstrap/auth changes were checked only far enough to determine
whether they invalidated the Gateway authorization contract. The targeted
Gateway regression passed 14/14 and the live Gateway OpenAPI remained
available. Those adjacent changes are not counted as AS-01 feature evidence and
were not expanded into a packaging review.

## Security and Rollback Assessment

- Tenant predicates and composite database references defend both application
  and persistence boundaries.
- Object roles are evaluated by the production repository; tenant-admin bypass
  remains tenant-scoped.
- Strong ETag parsing and locked revision checks prevent silent lost updates.
- Version sealing prevents mutation of both Version rows and their bindings.
- Secret-shaped data is rejected on current writes and redacted on legacy
  reads/copies; token plaintext and token hashes are absent from API/audit
  payloads.
- The migration is additive. If route exposure must be rolled back, the Agent
  router can be disabled while preserving already-created schema and history;
  no destructive rollback is required for the reviewed acceptance decision.

## Handoff

The independent Critic gate for AS-01/AS-F002 is approved. The orchestrator may
now perform the authorized Oracle/state update and strict AS-01 completion gate.
Only a successful completion gate may unlock the next phase. AS-09 and the full
demand remain incomplete until their own required evidence is produced.
