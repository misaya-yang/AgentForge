# Phase 01 - AI Assistant Trace Schema and API

> Enter plan-first mode before editing. Execute this phase only, work on ATE-F002 only, and do not implement assistant-service capture or frontend UI in this phase.

**Goal:** Add the AI Assistant trace database contract and tenant-scoped Eval API surface.

**Architecture:** This phase creates the durable storage and API read/write contract for AI Assistant trace evaluation. It must expose trace list, trace detail, and score write semantics without changing assistant-service runtime behavior. LangGraph Proxy Trace and RAG Trace fields may be represented through typed `trace_family` and metadata contracts, but only AI Assistant data is accepted in this phase.

**Tech Stack:** PostgreSQL additive migration, FastAPI router under `src/api/v1`, Pydantic schemas under `src/api/schemas`, ai-gateway-core persistence helpers, pytest API tests, OpenAPI compatibility tests, and ruff.

---

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ATE-01",
    "number": "01",
    "title": "AI Assistant Trace Schema and API",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-trace-eval-prd",
    "phase_file": "deploy/runbooks/agent-trace-eval-prd/phase-01-ai-assistant-trace-schema-and-api.md",
    "depends_on": [
      "ATE-00"
    ],
    "unlocks": [
      "ATE-02"
    ]
  },
  "goal": {
    "target": "Add the AI Assistant trace database contract and tenant-scoped Eval API surface.",
    "prompt": "Complete ATE-01 AI Assistant Trace Schema and API for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-01-ai-assistant-trace-schema-and-api.md`; work on feature-oracle item ATE-F002; depend on ATE-00 evidence; add only the AI Assistant trace schema and tenant-scoped Eval API contract; do not implement assistant-service trace capture or frontend UI; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-plan.md",
    "completion_report": "deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-report.md"
  },
  "runtime": {
    "context_profile": "deploy/runbooks/agent-trace-eval-prd/context-profile.json",
    "feature_oracle": "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
    "loop_contract": "deploy/runbooks/agent-trace-eval-prd/loop-contract.json",
    "loop_state": "deploy/runbooks/agent-trace-eval-prd/loop-state.json",
    "progress_log": "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
    "handoff": "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
    "continuity_ledger": "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
    "next_window_prompt": "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md",
    "session_boot": {
      "read_progress": true,
      "run_baseline_check": true,
      "update_progress_before_exit": true
    },
    "agent_roles": [
      "planner",
      "generator",
      "critic"
    ]
  },
  "context": {
    "read_first": [
      "deploy/runbooks/agent-trace-eval-prd/context-profile.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json",
      "deploy/runbooks/agent-trace-eval-prd/phase-01-ai-assistant-trace-schema-and-api.md"
    ],
    "primary_context": [
      "database/migrations/033_observability_and_quota_governance.sql",
      "database/migrations/034_assistant_gateway_foundation.sql",
      "src/api/router.py",
      "src/api/v1/usage.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/agent-trace-eval-prd/README.md only when harness intent is unclear",
      "deploy/runbooks/agent-trace-eval-prd/phase-manifest.md only when dependency order is unclear",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md only for target data model lookup or writeback",
      "deploy/runbooks/agent-trace-eval-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json only for ATE-F002 evidence update",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md only for ATE-00 dependency lookup or ATE-01 writeback",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md only when preparing a fresh context window"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "database/migrations/060_agent_trace_eval.sql",
      "src/api/v1/eval.py",
      "src/api/schemas/eval.py",
      "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py",
      "src/api/router.py",
      "tests/api/test_eval_traces.py",
      "tests/contract/test_openapi_schema_compat.py",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-report.md",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-critic.md",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json"
    ],
    "do_not_edit": [
      "apps/assistant-service/",
      "web/",
      "src/api/v1/langgraph.py",
      "production systems",
      "secret files",
      "deployment configuration"
    ],
    "external_inputs": [
      "ATE-00 actor report and critic artifact"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation",
      "file patch",
      "pytest",
      "ruff"
    ],
    "approval_required": [
      "production data mutation",
      "destructive commands",
      "external service changes",
      "deployment",
      "production migration execution"
    ],
    "dangerous_commands": [
      "git reset --hard",
      "rm -rf",
      "production migration"
    ]
  },
  "risk": {
    "tags": [
      "database",
      "schema",
      "migration",
      "api",
      "auth",
      "security",
      "eval",
      "agent"
    ],
    "data_mutation": "local development database schema only",
    "migration_required": "additive migration 060_agent_trace_eval.sql",
    "browser_required": "false",
    "ai_eval_required": "score contract only",
    "external_service_required": "false",
    "release_blocking": "true"
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-eval-api",
        "cwd": ".",
        "command": "uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py tests/api/test_eval_traces.py tests/contract/test_openapi_schema_compat.py",
        "expected": "Ruff exits 0 for the Eval API, schemas, repository helper, and touched tests.",
        "required": true
      },
      {
        "id": "eval-api-tests",
        "cwd": ".",
        "command": "uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_gateway_tenant_isolation.py tests/contract/test_openapi_schema_compat.py",
        "expected": "Pytest exits 0 and proves trace list, detail, score write, tenant isolation, and OpenAPI compatibility.",
        "required": true
      },
      {
        "id": "migration-contract-scan",
        "cwd": ".",
        "command": "rg -n 'agent_traces|agent_trace_spans|agent_trace_events|agent_trace_scores|tenant_id|trace_family|assistant' database/migrations/060_agent_trace_eval.sql",
        "expected": "Migration output includes the trace tables, tenant_id, trace_family, and assistant scope tokens.",
        "required": true
      },
      {
        "id": "harness-strict",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score",
        "expected": "Harness strict validation exits 0 after ATE-01 evidence writeback.",
        "required": true
      }
    ],
    "browser_checks": [
      "No browser route is created in ATE-01; any browser-visible edit blocks completion until route and viewport evidence are recorded."
    ],
    "regression_scope": [
      "Existing usage trace API behavior remains unchanged.",
      "Existing assistant session and OpenAPI compatibility tests remain passing.",
      "ATE-00 source packet and continuity ledger facts remain valid."
    ],
    "compliance_gates": [
      "Eval API requires authenticated tenant context before returning trace rows.",
      "Trace list and detail queries filter by tenant_id and never rely on client-supplied tenant identifiers.",
      "Prompt, response, tool input, and error fields store redacted or bounded payloads only.",
      "Score write records evaluator identity from auth context and rejects cross-tenant trace ids.",
      "No production migration is executed in this phase."
    ],
    "acceptance_gates": [
      "Additive migration defines agent_traces, agent_trace_spans, agent_trace_events, and agent_trace_scores with tenant_id and created_at indexes.",
      "API exposes AI Assistant trace list, trace detail, and score write endpoints under an Eval namespace.",
      "API responses include trace status, latency, model/provider metadata, run_id, session_id, span tree, events, scores, and redaction markers.",
      "LangGraph Proxy and RAG trace families are reserved by schema contract but rejected or hidden from AI Assistant endpoints until later phases.",
      "Actor report records validation evidence, rollback SQL or rollback procedure, minimal-change scope, and security proof.",
      "A separate independent critic artifact contains Critic Verdict and checks schema rollback, API tenant isolation, redaction, OpenAPI compatibility, regression impact, and minimal-change scope."
    ],
    "rollback_plan": [
      "If validation fails before migration is applied, revert migration and API files from the working tree.",
      "If a local development migration was applied, run a reviewed rollback migration that drops only agent_trace_scores, agent_trace_events, agent_trace_spans, and agent_traces in dependency order.",
      "Remove Eval router registration from src/api/router.py if the route contract is rejected.",
      "Leave assistant-service and web code untouched during rollback."
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-report.md"
    ],
    "required_artifacts": [
      "phase report with Status line",
      "progress log entry",
      "feature oracle evidence",
      "continuity ledger update",
      "source packet update",
      "handoff update",
      "independent critic evidence with Critic Verdict",
      "minimal-change scope note"
    ],
    "waiver_policy": "Only mark a gate waived when the user explicitly waives it or the actor report records the blocker and remaining evidence.",
    "next_phase_handoff": "ATE-02 is unlocked only when the database and Eval API contract are tenant-scoped, redaction-aware, tested, and approved by the critic."
  },
  "stop_conditions": [
    "ATE-00 completion evidence is missing or rejected",
    "Tenant isolation cannot be enforced from the existing auth context",
    "A destructive migration would be required",
    "API implementation requires assistant-service runtime changes",
    "OpenAPI compatibility fails without an approved contract update"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: ATE-01
- GOAL_TARGET: Add the AI Assistant trace database contract and tenant-scoped Eval API surface.
- GOAL_PROMPT: Complete ATE-01 AI Assistant Trace Schema and API for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-01-ai-assistant-trace-schema-and-api.md`; work on feature-oracle item ATE-F002; depend on ATE-00 evidence; add only the AI Assistant trace schema and tenant-scoped Eval API contract; do not implement assistant-service trace capture or frontend UI; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
- DEPENDS_ON: ATE-00
- READ_FIRST: `deploy/runbooks/agent-trace-eval-prd/context-profile.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: `database/migrations/033_observability_and_quota_governance.sql`, `database/migrations/034_assistant_gateway_foundation.sql`, `src/api/router.py`, `src/api/v1/usage.py`
- LIKELY_EDIT_PATHS: `database/migrations/060_agent_trace_eval.sql`, `src/api/v1/eval.py`, `src/api/schemas/eval.py`, `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py`, `src/api/router.py`, `tests/api/test_eval_traces.py`, `tests/contract/test_openapi_schema_compat.py`, harness report and runtime writeback files for ATE-01
- DO_NOT_EDIT: `apps/assistant-service/`, `web/`, `src/api/v1/langgraph.py`, production systems, secret files, deployment configuration
- EXECUTION_MODE: plan-first; implement one phase and one feature item; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: `uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py tests/api/test_eval_traces.py tests/contract/test_openapi_schema_compat.py`; `uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_gateway_tenant_isolation.py tests/contract/test_openapi_schema_compat.py`; `rg -n 'agent_traces|agent_trace_spans|agent_trace_events|agent_trace_scores|tenant_id|trace_family|assistant' database/migrations/060_agent_trace_eval.sql`; `python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score`
- BROWSER_CHECKS: No browser route is created in ATE-01; any browser-visible edit blocks completion until route and viewport evidence are recorded.
- REGRESSION_SCOPE: Existing usage trace API behavior remains unchanged; assistant session and OpenAPI compatibility tests remain passing; ATE-00 facts remain valid.
- COMPLIANCE_GATES: Authenticated tenant context is required; trace queries filter by server-side tenant_id; redacted and bounded payloads only; no production migration.
- ROLLBACK_PLAN: Revert migration and API files before application, or run a reviewed local rollback migration that drops only ATE-01 trace tables in dependency order.
- ACCEPTANCE_GATES: Migration, Eval API, tests, source-packet writeback, continuity ledger update, minimal-change scope, and independent critic verdict are complete.
- EVIDENCE_OUTPUT: `deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-report.md`
- STOP_CONDITIONS: Stop if ATE-00 is not passed, tenant isolation cannot be enforced, destructive migration is required, assistant-service changes are required, or OpenAPI compatibility fails without an approved update.

## Task Spec

Implement the storage and API foundation for AI Assistant traces. The API must let the Eval console list traces, inspect one trace with spans and events, and attach evaluator scores. It must not yet capture assistant-service runtime events.

## Problem Boundary

In scope: additive schema, repository helper, Pydantic schemas, FastAPI Eval routes, router registration, API tests, OpenAPI contract update when required, and harness evidence.

Out of scope: assistant-service writes, frontend pages, LangGraph Proxy trace capture, RAG retrieval trace capture, production migration execution, external observability exports.

## Context Policy

Load only the four `PRIMARY_CONTEXT` paths after `READ_FIRST`. Open ATE-00 reports and ledger rows only to confirm dependency acceptance. Defer source packet, continuity ledger, feature oracle, and progress log until targeted writeback.

## Requirements

- Add tables for root traces, spans, events, and scores with server-side tenant ownership.
- Model `trace_family` with first-wave value `assistant`; future values must not appear in AI Assistant endpoint results.
- Store run identifiers, session identifiers, request identifiers, timing, status, model/provider metadata, redaction flags, and bounded JSON payload summaries.
- Provide Eval API endpoints for list, detail, and score write.
- Preserve existing usage trace endpoints and assistant session APIs.

## Test and Regression Requirements

Run the validation commands from the Machine Contract. API tests must prove same-tenant access succeeds, cross-tenant access is rejected, score writes use evaluator identity from auth context, and OpenAPI remains compatible.

## Compliance and Safety Requirements

Do not store raw secrets, tokens, provider keys, or connection strings in trace payloads. Do not execute migrations against production. Do not accept tenant_id from the request body or query string as an authorization source.

## Rollback and Recovery

Rollback removes only the ATE-01 Eval API files and additive trace tables. If a local development database has applied the migration, rollback must drop score, event, span, then root trace tables in dependency order.

## Execution Capture

Write `reports/ate-01-ai-assistant-trace-schema-and-api-report.md` with Status, Validation Evidence, Feature Oracle Updates, Minimal Change, Regression Scope, Compliance Evidence, Rollback Evidence, and Next Phase Handoff. Update source packet, continuity ledger, progress log, feature oracle, handoff, and loop state.

## Critic Protocol

Use `reports/ate-01-ai-assistant-trace-schema-and-api-critic.md`. The critic must state `Critic Verdict`, name the actor report reviewed, and verify tenant isolation, redaction, additive migration design, rollback path, OpenAPI compatibility, test sufficiency, and minimal-change scope.

## Acceptance Criteria

- Eval API list, detail, and score endpoints are tenant-scoped.
- Migration introduces only additive AI Assistant trace tables and indexes.
- Tests cover positive, cross-tenant negative, and score write paths.
- ATE-02 receives a stable trace-write contract without requiring schema redesign.

## Risks

- Cross-tenant leakage is the highest risk because traces contain conversation and tool metadata.
- Raw prompt or tool payload storage can create open-source and compliance exposure.
- Schema overreach can prematurely couple LangGraph Proxy and RAG Trace to AI Assistant implementation.
