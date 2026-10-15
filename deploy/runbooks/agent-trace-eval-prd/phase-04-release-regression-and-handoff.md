# Phase 04 - Release Regression and Handoff

> Enter plan-first mode before editing. Execute this terminal phase only, work on ATE-F005 only, and do not claim completion without whole-demand regression evidence.

**Goal:** Run release regression and hand off the LangGraph Proxy Trace and RAG Trace expansion contracts.

**Architecture:** This phase proves the first-wave AI Assistant trace Eval feature end to end: storage, API, runtime capture, non-blocking latency guard, UI review, scoring, security, and operational regression. It also writes the next-wave contracts for LangGraph Proxy Trace and RAG Trace without implementing them.

**Tech Stack:** pytest, ruff, pnpm lint, pnpm type check, Playwright e2e smoke, Makefile config validation, harness strict validator, and runbook evidence files.

---

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ATE-04",
    "number": "04",
    "title": "Release Regression and Handoff",
    "status": "ready",
    "type": "release",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-trace-eval-prd",
    "phase_file": "deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md",
    "depends_on": [
      "ATE-03"
    ],
    "unlocks": []
  },
  "goal": {
    "target": "Run release regression and hand off the LangGraph Proxy Trace and RAG Trace expansion contracts.",
    "prompt": "Complete ATE-04 Release Regression and Handoff for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md`; work on feature-oracle item ATE-F005; depend on ATE-03 evidence; run whole-demand regression for the completed AI Assistant trace Eval wave; prove trace persistence does not add user-visible agent latency; write LangGraph Proxy Trace and RAG Trace handoff contracts without implementing them; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, browser checks, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-plan.md",
    "completion_report": "deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md"
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
      "deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md"
    ],
    "primary_context": [
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "web/package.json",
      "pyproject.toml"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/agent-trace-eval-prd/README.md only when release intent is unclear",
      "deploy/runbooks/agent-trace-eval-prd/phase-manifest.md only when dependency order is unclear",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md only for terminal handoff lookup or writeback",
      "deploy/runbooks/agent-trace-eval-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json only for ATE-F005 evidence update or whole-demand status review",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md only for terminal dependency review or ATE-04 writeback",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md only when preparing a fresh context window"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-critic.md",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md"
    ],
    "do_not_edit": [
      "database/migrations/",
      "src/api/v1/",
      "apps/assistant-service/",
      "web/src/",
      "production systems",
      "secret files",
      "deployment configuration"
    ],
    "external_inputs": [
      "ATE-00 through ATE-03 actor reports and critic artifacts"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation",
      "browser verification",
      "file patch",
      "pytest",
      "ruff",
      "pnpm",
      "make"
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
      "release",
      "frontend",
      "browser",
      "api",
      "database",
      "security",
      "ai",
      "agent",
      "eval"
    ],
    "data_mutation": "local seeded trace and score rows only when tests require them",
    "migration_required": "false",
    "browser_required": "true",
    "ai_eval_required": "whole-demand regression over AI Assistant trace Eval",
    "external_service_required": "false",
    "release_blocking": "true"
  },
  "validation": {
    "commands": [
      {
        "id": "backend-whole-demand-regression",
        "cwd": ".",
        "command": "uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/integration/test_assistant_isolation_contract.py tests/contract/test_openapi_schema_compat.py tests/api/test_usage_api.py",
        "expected": "Pytest exits 0 and proves Eval API, assistant trace capture, latency guard behavior, streaming contract, isolation, OpenAPI compatibility, and existing usage traces.",
        "required": true
      },
      {
        "id": "backend-lint-regression",
        "cwd": ".",
        "command": "uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py apps/assistant-service/src/assistant_service tests/api/test_eval_traces.py tests/services/assistant/test_agent_trace_capture.py",
        "expected": "Ruff exits 0 for trace API, trace repository, assistant-service, and touched tests.",
        "required": true
      },
      {
        "id": "frontend-lint",
        "cwd": ".",
        "command": "pnpm -C web lint",
        "expected": "Web lint exits 0.",
        "required": true
      },
      {
        "id": "frontend-type-check",
        "cwd": ".",
        "command": "pnpm -C web type-check",
        "expected": "Web type check exits 0.",
        "required": true
      },
      {
        "id": "frontend-e2e",
        "cwd": ".",
        "command": "pnpm -C web e2e:opensource",
        "expected": "Open-source e2e smoke exits 0 and covers /eval route behavior.",
        "required": true
      },
      {
        "id": "open-source-env-gate",
        "cwd": ".",
        "command": "make validate-example-config",
        "expected": "Open-source example configuration validation exits 0.",
        "required": true
      },
      {
        "id": "harness-strict",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score",
        "expected": "Harness strict validation exits 0 after terminal evidence writeback.",
        "required": true
      }
    ],
    "browser_checks": [
      "Open /eval at 1440x900 and record Assistant trace list, detail, score, loading, empty, and error states.",
      "Open /eval at 390x844 and record no horizontal overflow plus reachable score controls.",
      "Open /assistant and record that existing chat navigation still loads.",
      "Open dashboard request trace surface and record that existing request trace behavior remains available."
    ],
    "regression_scope": [
      "whole-demand regression covers ATE-F001 through ATE-F004 evidence and ATE-F005 terminal handoff.",
      "AI Assistant trace storage, API, runtime capture, latency guard, UI review, and scoring are proven together.",
      "Existing assistant sessions, assistant runs, request traces, usage traces, and dashboard request trace UI remain passing.",
      "LangGraph Proxy Trace and RAG Trace remain documented as next-wave contracts without first-wave implementation."
    ],
    "compliance_gates": [
      "No production data is read or mutated.",
      "No production migration is executed.",
      "No external observability service is configured.",
      "Trace payload redaction, tenant isolation, permission gating, and no browser-side tenant override are verified in prior phase evidence.",
      "Whole-demand regression evidence includes non-blocking trace persistence proof so agent latency is not degraded."
    ],
    "acceptance_gates": [
      "ATE-F001 through ATE-F004 are passing or explicitly waived before ATE-F005 is marked passing.",
      "Actor report includes whole-demand regression evidence across backend, assistant runtime, frontend, browser, open-source environment, and harness validation.",
      "Actor report includes evidence that slow or failing trace persistence does not delay agent first token, final response, or run status updates.",
      "source-packet.md and continuity-ledger.md record LangGraph Proxy Trace and RAG Trace next-wave boundaries.",
      "agent-handoff.md and next-window-prompt.md name the next phase only as a future user-approved expansion.",
      "No runtime code change is made in ATE-04 unless a blocker requires a documented surgical fix inside the actor report.",
      "A separate independent critic artifact contains Critic Verdict and checks whole-demand regression, latency guard evidence, security gates, browser evidence, next-wave handoff, and minimal-change scope."
    ],
    "rollback_plan": [
      "If ATE-04 only updates evidence files, revert ATE-04 runbook edits.",
      "If a surgical fix is required, use the rollback plan from the owning phase and record the exact files in the actor report.",
      "Do not roll back passed ATE-01 through ATE-03 work from ATE-04 without explicit user approval.",
      "Set loop-state status to blocked when whole-demand regression cannot pass."
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md"
    ],
    "required_artifacts": [
      "phase report with Status line",
      "progress log entry",
      "feature oracle evidence",
      "continuity ledger update",
      "source packet update",
      "handoff update",
      "independent critic evidence with Critic Verdict",
      "minimal-change scope note",
      "whole-demand regression evidence"
    ],
    "waiver_policy": "Only mark a gate waived when the user explicitly waives it or the actor report records the blocker and remaining evidence.",
    "next_phase_handoff": "The first-wave AI Assistant trace Eval demand is complete only after whole-demand regression and critic approval; LangGraph Proxy Trace and RAG Trace remain future expansion contracts."
  },
  "stop_conditions": [
    "ATE-03 completion evidence is missing or rejected",
    "Any ATE-F001 through ATE-F004 item is failing without explicit waiver",
    "Whole-demand regression fails",
    "Latency guard evidence is missing or shows trace persistence delays the agent",
    "Browser checks cannot be completed",
    "Release completion requires deployment or production data access"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: ATE-04
- GOAL_TARGET: Run release regression and hand off the LangGraph Proxy Trace and RAG Trace expansion contracts.
- GOAL_PROMPT: Complete ATE-04 Release Regression and Handoff for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md`; work on feature-oracle item ATE-F005; depend on ATE-03 evidence; run whole-demand regression for the completed AI Assistant trace Eval wave; prove trace persistence does not add user-visible agent latency; write LangGraph Proxy Trace and RAG Trace handoff contracts without implementing them; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, browser checks, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
- DEPENDS_ON: ATE-03
- READ_FIRST: `deploy/runbooks/agent-trace-eval-prd/context-profile.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: `deploy/runbooks/agent-trace-eval-prd/feature-oracle.json`, `deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md`, `web/package.json`, `pyproject.toml`
- LIKELY_EDIT_PATHS: `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md`, `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-critic.md`, `deploy/runbooks/agent-trace-eval-prd/source-packet.md`, `deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md`, `deploy/runbooks/agent-trace-eval-prd/progress-log.md`, `deploy/runbooks/agent-trace-eval-prd/agent-handoff.md`, `deploy/runbooks/agent-trace-eval-prd/feature-oracle.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, `deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md`
- DO_NOT_EDIT: `database/migrations/`, `src/api/v1/`, `apps/assistant-service/`, `web/src/`, production systems, secret files, deployment configuration
- EXECUTION_MODE: plan-first; implement one phase and one feature item; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: `uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/integration/test_assistant_isolation_contract.py tests/contract/test_openapi_schema_compat.py tests/api/test_usage_api.py`; `uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py apps/assistant-service/src/assistant_service tests/api/test_eval_traces.py tests/services/assistant/test_agent_trace_capture.py`; `pnpm -C web lint`; `pnpm -C web type-check`; `pnpm -C web e2e:opensource`; `make validate-example-config`; `python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score`
- BROWSER_CHECKS: `/eval` desktop 1440x900, `/eval` mobile 390x844, `/assistant` route load, dashboard request trace route load
- REGRESSION_SCOPE: whole-demand regression covers ATE-F001 through ATE-F005, including AI Assistant trace storage, API, runtime capture, latency guard, UI review, scoring, security, open-source configuration, and next-wave handoff.
- COMPLIANCE_GATES: No production data, no production migration, no deployment, no external observability configuration, tenant isolation verified, redaction verified, latency guard verified.
- ROLLBACK_PLAN: Revert ATE-04 runbook edits when only evidence changes; use the owning phase rollback plan for any surgical fix; never roll back passed implementation phases from ATE-04 without explicit user approval.
- ACCEPTANCE_GATES: Whole-demand regression evidence, browser evidence, LangGraph/RAG handoff, source-packet writeback, continuity ledger update, minimal-change scope, and independent critic verdict are complete.
- EVIDENCE_OUTPUT: `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md`
- STOP_CONDITIONS: Stop if ATE-03 is not passed, any prior oracle item is failing without waiver, whole-demand regression fails, latency guard evidence is missing, browser checks fail, or completion requires deployment.

## Task Spec

Prove the first-wave AI Assistant trace Eval feature as one working demand. Write the future expansion handoff for LangGraph Proxy Trace and RAG Trace after regression passes.

## Problem Boundary

In scope: release validation, browser evidence, terminal actor report, terminal critic artifact, feature-oracle status updates, source packet writeback, continuity ledger writeback, handoff updates, and next-window prompt updates.

Out of scope: new schema, new API, assistant-service changes, frontend feature changes, production deployment, production migration execution, external observability configuration.

## Context Policy

Load only the four `PRIMARY_CONTEXT` items after `READ_FIRST`. Open prior phase reports and critic artifacts only to verify evidence paths and blockers. Defer source packet, continuity ledger, feature oracle, and progress log until targeted review or writeback.

## Requirements

- Run whole-demand regression across ATE-F001 through ATE-F004 before marking ATE-F005 passing.
- Prove the AI Assistant trace path from chat execution to persisted trace to Eval UI score workflow.
- Prove trace persistence does not add user-visible agent latency.
- Prove privacy, tenant isolation, permission, and redaction gates remain satisfied.
- Write precise handoff contracts for LangGraph Proxy Trace and RAG Trace.

## Test and Regression Requirements

Run every validation command from the Machine Contract. Record exact pass/fail evidence. Browser checks must cover `/eval`, `/assistant`, and existing dashboard request trace behavior.

## Compliance and Safety Requirements

Do not deploy, run production migrations, access production data, print secrets, configure external observability services, or broaden trace retention.

## Rollback and Recovery

If ATE-04 fails with only evidence edits, revert those evidence edits. If a regression fix is required, route the fix to the owning phase boundary and record the exact rollback path before applying it.

## Execution Capture

Write `reports/ate-04-release-regression-and-handoff-report.md` with Status, Validation Evidence, Browser Evidence, Whole-Demand Regression, Feature Oracle Updates, Minimal Change, Regression Scope, Compliance Evidence, Rollback Evidence, and Next-Wave Handoff. Update source packet, continuity ledger, progress log, feature oracle, handoff, loop state, and next-window prompt.

## Critic Protocol

Use `reports/ate-04-release-regression-and-handoff-critic.md`. The critic must state `Critic Verdict`, name the actor report reviewed, and verify whole-demand regression, latency guard evidence, browser evidence, security gates, next-wave handoff, and minimal-change scope.

## Acceptance Criteria

- Whole-demand regression passes across backend, assistant runtime, frontend, browser, open-source environment, and harness validation.
- Non-blocking trace persistence is proven.
- ATE-F001 through ATE-F005 have actor and critic evidence.
- LangGraph Proxy Trace and RAG Trace handoffs are clear enough for a new phase harness or next phase.
- No production or external service change occurs.

## Risks

- A terminal pass can be claimed without proving the end-to-end path.
- Browser evidence can miss mobile overflow or auth regressions.
- Latency guard evidence can be overlooked even when trace persistence technically works.
- Next-wave LangGraph and RAG scope can become ambiguous if handoff fields are not explicit.
