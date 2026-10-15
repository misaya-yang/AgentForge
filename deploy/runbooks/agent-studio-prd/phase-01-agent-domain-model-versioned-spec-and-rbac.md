# Phase 01 - Agent Domain Model, Versioned Spec, and RBAC

> Agentic worker: execute only the tenant-safe Agent domain and additive persistence/API slice after AS-00 passes.

- PHASE_ID: AS-01
- DEPENDS_ON: AS-00
- UNLOCKS: AS-02
- FEATURE: AS-F002

**Goal:** Deliver stable Agent identities, optimistic Drafts, immutable Versions, ACLs, Publication records, and audited CRUD without changing runtime execution.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "AS-01", "number": "01", "title": "Agent Domain Model Versioned Spec and RBAC", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-01-agent-domain-model-versioned-spec-and-rbac.md", "depends_on": ["AS-00"], "unlocks": ["AS-02"]
  },
  "goal": {
    "target": "Add tenant-scoped Agent, Draft, normalized binding, immutable Version, member ACL, Publication, publish-event, and API-token primitives with explicit tenant child keys/composite foreign keys plus CRUD, copy, archive, validation, and optimistic concurrency APIs.",
    "prompt": "Complete AS-01 by following deploy/runbooks/agent-studio-prd/phase-01-agent-domain-model-versioned-spec-and-rbac.md after AS-00 passes; create additive idempotent schema, typed repositories/schemas/routes, tenant and role authorization, immutable version and optimistic draft tests, report evidence, and an independent critic verdict without wiring Agent execution or UI.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-01-agent-domain-model-versioned-spec-and-rbac-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-01-agent-domain-model-versioned-spec-and-rbac-report.md"
  },
  "runtime": {
    "context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]
  },
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-01-agent-domain-model-versioned-spec-and-rbac.md"],
    "primary_context": ["database/migrations/034_assistant_gateway_foundation.sql and migration runner conventions", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories", "src/api/v1 and src/api/schemas tenant-scoped CRUD patterns", "src/core/auth and tests/security authorization patterns"],
    "context_budget": "focused",
    "do_not_load_unless": ["architecture-contract.md data model and API sections for a disputed invariant", "product-requirements.md sections 4, 5, and 6.1 for role/lifecycle behavior", "AS-00 report only for inherited capability boundary", "live database only after explicit migration approval", "source-packet.md only for a targeted schema or auth fact lookup/writeback", "continuity-ledger.md only for Agent identity and persistence boundary lookup/writeback", "feature-oracle.json only for AS-F002 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["database/migrations/07*_agent_studio*.sql", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py", "src/api/v1/agents.py", "src/api/schemas/agents.py", "src/api/router.py", "tests/database/test_agent_studio_migrations.py", "tests/api/test_agents_api.py", "tests/security/test_agent_studio_rbac.py", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["apps/assistant-service AgentLoop/runtime", "web UI", "MCP/Skills/Knowledge implementations", "existing session service_id semantics", "production data"],
    "external_inputs": ["AS-00 passed report and confirmed next free migration number"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["rg and migration inspection", "apply_patch", "targeted ruff and pytest", "OpenAPI schema inspection"],
    "approval_required": ["apply migration to a live database", "destructive schema operation", "Docker", "deployment", "commit or push"],
    "dangerous_commands": ["DROP TABLE", "TRUNCATE", "migration against production", "git reset --hard", "rm -rf"]
  },
  "risk": {"tags": ["database", "schema", "migration", "auth", "security", "multi-tenant"], "data_mutation": true, "migration_required": true, "browser_required": false, "ai_eval_required": false, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "migration-contract", "cwd": ".", "command": "uv run pytest -q --no-cov tests/database/test_agent_studio_migrations.py", "expected": "Migration SQL is additive/idempotent; every tenant-owned child stores tenant_id, composite foreign keys reject Tenant A child to Tenant B parent insertion, and indexes, immutable-version guards, soft-delete and rollback-safe compatibility assertions pass.", "required": true},
      {"id": "agent-api", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py", "expected": "CRUD, pagination, copy, archive, ACL, last-owner, If-Match conflict, immutable Version, redaction, and cross-tenant cases pass.", "required": true},
      {"id": "lint", "cwd": ".", "command": "uv run ruff check src/api/v1/agents.py src/api/schemas/agents.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py tests/database/test_agent_studio_migrations.py tests/api/test_agents_api.py tests/security/test_agent_studio_rbac.py", "expected": "Ruff exits zero for touched Python code.", "required": true},
      {"id": "gateway-regression", "cwd": ".", "command": "uv run pytest -q --no-cov tests/integration/test_gateway_boot.py tests/security/test_management_api_authorization.py", "expected": "Gateway boot and existing management authorization contracts pass.", "required": true}
    ],
    "browser_checks": ["No frontend route is added; inspect generated OpenAPI to confirm Agent endpoints and error schemas are present without exposing secret_ref internals or token hashes."],
    "regression_scope": ["gateway boot", "existing tenant authorization", "migration ordering/idempotency", "service/session APIs", "serialization without secrets"],
    "compliance_gates": ["every tenant-owned parent and child table stores tenant_id", "child relationships use composite tenant_id plus object_id foreign keys", "every repository query includes tenant_id", "object UUID alone never authorizes access", "last Owner cannot be removed", "token tables store hashes only", "soft delete and audit retention are explicit", "no destructive migration"],
    "acceptance_gates": ["Stale Draft revision returns 409 AGENT_DRAFT_CONFLICT and does not lose either edit.", "Created Version rows and resolved specs cannot be updated through repository or API paths.", "Copy excludes sessions, memory, ACL, publications, API tokens, secrets, and inaccessible resource bindings.", "Database tests reject direct cross-tenant parent/child foreign-key insertion for members, drafts, versions, bindings, publications, events and tokens.", "Cross-tenant API read/write/list/version tests reveal no object existence or data.", "Independent critic approves migration safety, RBAC, API contract, tests, and minimal-change scope."],
    "rollback_plan": ["Disable Agent routes and leave additive tables/nullable structures intact if application rollback is needed.", "Do not drop Version, audit, or token history during rollback; document a forward cleanup migration if schema repair is required."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-01-agent-domain-model-versioned-spec-and-rbac-report.md", "deploy/runbooks/agent-studio-prd/reports/as-01-critic-verdict.md"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "migration test evidence", "API/RBAC test evidence", "OpenAPI excerpt", "schema and error contract summary", "rollback evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "Tenant isolation, immutable Version, optimistic concurrency, token hashing, and additive migration gates cannot be waived for downstream execution; a user waiver may only defer non-security metadata fields.",
    "next_phase_handoff": "AS-02 receives stable IDs, repository methods, AgentSpec schema version, ACL decisions, Draft revision semantics, Version immutability, and additive trace/session column plan."
  },
  "stop_conditions": ["AS-00 is not passed", "the next migration number conflicts with the target branch", "a required schema change is destructive", "tenant authorization cannot be expressed with existing auth context", "live migration is required without approval"]
}
```

## Requirements

### R1 Stable Tenant-Scoped Domain

Agent identity, ACL, Draft, normalized bindings, Version, Publication, audit and API-token entities must all store explicit tenant ownership and use composite tenant/object constraints so the database rejects cross-tenant child references.

### R2 Optimistic Drafts and Immutable Versions

Every Draft mutation requires an expected revision; every Version is write-once and carries a deterministic schema version/spec hash.

### R3 Role-Correct APIs

Owner, Editor and Viewer permissions, last-owner protection, cross-tenant denial, copy, archive and soft-delete behavior must be server-enforced and observable through stable errors.

### R4 Additive Migration

The migration may add tables, constraints, indexes and nullable compatibility columns; it must not repurpose `service_id`, rewrite history, or remove existing schema.

## Critic Protocol

Reject if any repository method omits tenant scope, ACL relies on UI, Draft updates lack revision checks, Version content is mutable, copy includes sensitive/runtime state, API tokens are reversible, migration is destructive/non-idempotent, or runtime/UI scope leaks into this Phase.
