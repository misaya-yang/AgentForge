# Phase 08 - Observability, Admin, Data Governance, and Aggregate Gate

> Agentic worker: implement operations/governance and a versioned aggregate regression command; leave the no-feature terminal release decision to AS-09.

- PHASE_ID: AS-08
- DEPENDS_ON: AS-07
- UNLOCKS: AS-09
- FEATURE: AS-F009

**Goal:** Make Agent Studio operable and governable, prove additive migration/feature-flag rollback, and create a complete versioned aggregate regression command for the terminal Phase.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-08", "number": "08", "title": "Observability Admin Data Governance and Aggregate Gate", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-08-observability-admin-migration-and-release-gate.md", "depends_on": ["AS-07"], "unlocks": ["AS-09"]},
  "goal": {
    "target": "Deliver Agent/Version/Publication/channel observability, audit, quota, retention and deletion controls, prove additive migration and feature-flag rollback, and add a versioned make verify-agent-studio aggregate whose manifest covers every AS-00 through AS-08 required gate.",
    "prompt": "Complete AS-08 by following deploy/runbooks/agent-studio-prd/phase-08-observability-admin-migration-and-release-gate.md after AS-07 passes; implement operational metrics, trace filtering, audits, quotas, retention/deletion and admin controls, verify migration/idempotency/feature-flag rollback and built-in Assistant compatibility, create and contract-test a versioned make verify-agent-studio aggregate covering every earlier required gate, and finish with actor, critic, rollback and AS-09 handoff evidence without issuing the terminal release decision.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-08-observability-admin-migration-and-release-gate.md"],
    "primary_context": ["packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py and metrics/audit services", "src/api/v1/eval.py, management/admin APIs, and rate/quota services", "database/migrations 034 through current Agent Studio migrations and scripts/new migration tooling", "Makefile, scripts/agent_studio_regression.py, and tests/contract/test_agent_studio_regression_manifest.py"],
    "context_budget": "focused",
    "do_not_load_unless": ["product-requirements.md section 6.11 and success metrics for an operations behavior dispute", "prior Phase reports only when deriving the aggregate gate manifest", "live Docker/runtime only after ownership checks and explicit approval", "production monitoring only after explicit approval", "source-packet.md only for operations code-fact audit/writeback", "continuity-ledger.md only for operations/aggregate boundary lookup/writeback", "feature-oracle.json only for AS-F009 evidence writeback", "progress-log.md only for blocker history or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py", "src/services/metrics", "src/services/audit", "src/core/rate_limit", "src/api/v1/agents.py", "src/api/v1/eval.py", "src/api/v1/admin*", "database/migrations/07*_agent_studio_operations*.sql", "scripts/new", "scripts/agent_studio_regression.py", "tests/fixtures/agent-studio/regression_manifest.json", "tests/contract/test_agent_studio_regression_manifest.py", "Makefile", "web/src/pages/agents", "web/src/pages/eval", "tests/api/test_agent_observability.py", "tests/security/test_agent_data_governance.py", "tests/database/test_agent_studio_migrations.py", "web/e2e/agent-analytics.spec.ts", "deploy/runbooks/agent-studio-prd", "reports/agent-studio"],
    "do_not_edit": ["unrelated product routes", "production secrets or data", "Docker images/volumes/caches without approval", "deployment/DNS configuration", "historical immutable Version/audit rows", "earlier Phase behavior to make aggregate tests pass"],
    "external_inputs": ["AS-00 through AS-07 passed reports and critic verdicts", "retention/deletion/legal-hold policy", "per-tenant/public quota and alert thresholds", "monitoring/dashboard ownership"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["rg and report/gate inspection", "apply_patch", "targeted pytest/ruff/Make gates", "pnpm build/Playwright/axe", "migration dry-run tooling"],
    "approval_required": ["Docker stop/up/build", "live migration", "external provider smoke", "production dashboard mutation", "deployment", "commit or push"],
    "dangerous_commands": ["DROP/TRUNCATE", "docker prune", "remove volumes/images/caches", "force push", "git reset --hard", "delete or skip failing earlier gate from aggregate"]
  },
  "risk": {"tags": ["database", "migration", "security", "privacy", "observability", "frontend", "browser", "agent"], "data_mutation": true, "migration_required": true, "browser_required": true, "ai_eval_required": false, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "operations-governance", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/api/test_agents_api.py tests/database/test_agent_studio_migrations.py", "expected": "Trace/metric filters, redaction, audit, quota, archive, token/grant revocation, retention, user/tenant deletion, composite tenant constraints and migration idempotency/compatibility tests pass through the current Gateway governance APIs.", "required": true},
      {"id": "aggregate-manifest", "cwd": ".", "command": "uv run pytest -q --no-cov tests/contract/test_agent_studio_regression_manifest.py", "expected": "The versioned aggregate manifest includes every required command/gate ID from AS-00 through AS-08, including AS-00 wiring/allowlist, AS-02 Envelope forgery, AS-03 credential principal, AS-04 Skill entrypoint/Knowledge provenance, AS-06 atomicity/eval, AS-07 built-header and all existing Assistant gates; removing a gate makes the contract test fail.", "required": true},
      {"id": "analytics-frontend", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-analytics.spec.ts --config playwright.opensource.config.ts", "expected": "Analytics/admin UI passes static checks and desktop/mobile trace, metric, audit, quota, retention-limited and permission states.", "required": true},
      {"id": "compatibility", "cwd": ".", "command": "make test-isolation && make verify-assistant-runtime-dev && make validate-example-config", "expected": "Built-in Assistant isolation/runtime and example configuration gates pass with Agent Studio feature flags on and off.", "required": true}
    ],
    "browser_checks": ["At /agents/:id/analytics on 1440x900 and 390x844 verify Agent/Version/Publication/channel/time filters, empty/retention-limited/error states, redaction, pagination and Trace deep links.", "Verify Owner/Editor/Viewer/Admin visibility for audit, quota and deletion controls with keyboard, focus, axe, console and network evidence.", "With Agent Studio frontend flag off, confirm Agent navigation/public entry disappears while /assistant, /knowledge, /eval and /share remain functional."],
    "regression_scope": ["Agent/Version/channel trace dimensions", "audit and redaction", "quota and abuse controls", "retention/deletion/legal hold", "migration idempotency", "feature-flag rollback", "built-in Assistant", "aggregate manifest completeness"],
    "compliance_gates": ["PII/secret redaction and least-privilege audit access", "tenant/Agent/Version/channel filters on metrics and traces", "quota/abuse alerts and fail-safe limits", "retention/deletion/legal-hold behavior", "API token, OAuth grant and cache revocation", "migration idempotency and non-destructive application rollback", "feature flag preserves built-in Assistant", "aggregate command cannot silently omit a required earlier gate"],
    "acceptance_gates": ["Owners query actionable per-Agent/Version/channel metrics and traces; Admin audit records every sensitive lifecycle/capability/credential/token action.", "Quota, archive, user deletion, tenant deletion, retention and legal-hold tests cover sessions, memory, tokens/grants, caches and derived indexes.", "Feature flag off and application rollback preserve existing Assistant and historical Agent evidence without destructive schema rollback.", "make verify-agent-studio is generated from or checked against a versioned manifest that covers every AS-00 through AS-08 required gate, including the specific gaps named by the planning critic.", "Independent critic approves operations, governance, migration, aggregate completeness, compatibility and minimal-change scope."],
    "rollback_plan": ["Disable Agent Studio creation, publication and public channels through separate server/frontend feature flags; preserve read-only data and built-in Assistant.", "Rollback application code while retaining additive schema, Versions, audits and traces; use a forward migration for repair.", "If the aggregate command is wrong, restore the last critic-approved manifest and keep AS-09 locked."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-08-observability-admin-migration-and-release-gate-report.md", "deploy/runbooks/agent-studio-prd/reports/as-08-critic-verdict.md", "reports/agent-studio/as-08-operations-governance.md", "reports/agent-studio/as-08-aggregate-manifest.json", "reports/agent-studio/as-08-browser-matrix.md"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "operations/governance test evidence", "metrics/trace/audit screenshots", "quota/retention/deletion evidence", "migration idempotency and rollback evidence", "aggregate manifest completeness evidence", "feature-flag/built-in Assistant regression evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "Tenant/security/privacy, audit, revocation, migration safety, built-in Assistant compatibility and aggregate-gate completeness cannot be waived for AS-08; production dashboard wiring may be deferred with local metric contract evidence and a named owner.",
    "next_phase_handoff": "AS-09 receives passed AS-00 through AS-08 reports/critics, the immutable aggregate manifest/version, exact make verify-agent-studio command, browser matrix, migration/rollback plan, external smoke approvals and unresolved risk list."
  },
  "stop_conditions": ["AS-07 or any earlier dependency is not passed", "retention/quota/legal policy is missing", "aggregate manifest omits or weakens an earlier required gate", "migration or feature-flag rollback is destructive", "operations require production data or external credentials without approval"]
}
```

## Requirements

### R1 Operable and Auditable Agents

Metrics, traces and audits are queryable by tenant, Agent, Version, Publication, channel and time with redaction, pagination and role controls.

### R2 Quota and Data Governance

Agent/channel quotas, archive, credential/token revocation, retention, user/tenant deletion, legal hold, cache invalidation and derived-data cleanup have deterministic tests and stable outcomes.

### R3 Safe Migration and Compatibility

Agent Studio schema remains additive/idempotent; application/feature-flag rollback preserves `__builtin_assistant__`, immutable history and audit evidence without destructive down migration.

### R4 Versioned Aggregate Gate

`make verify-agent-studio` is backed by a contract-tested manifest that enumerates every earlier required gate and fails when a Phase-specific test is removed; AS-08 does not itself issue the terminal release verdict.

## Critic Protocol

Reject if metrics rely only on free-form metadata, audits omit sensitive actions, deletion leaves credentials/tokens/memory/caches, quotas fail open, migrations require destructive rollback, feature flag breaks the built-in Assistant, or the aggregate manifest omits AS-00 wiring, AS-02 Envelope, AS-03 credential, AS-04 entrypoint/provenance, AS-06 atomicity/eval, AS-07 built-header or existing Assistant gates.
