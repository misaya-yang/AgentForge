# Phase 00 - Baseline Trace Architecture

> Enter plan-first mode before editing. Execute this phase only, work on ATE-F001 only, and do not advance to ATE-01 until the report and critic artifact pass the gates below.

**Goal:** Freeze repo-specific trace architecture, first-wave scope, and validation boundaries before implementation.

**Architecture:** The Agent Trace Eval module follows the industry pattern found in LangSmith, Langfuse, Phoenix, MLflow, and OpenTelemetry GenAI: a root trace/run contains ordered spans, durable events, tenant-scoped metadata, and score or feedback records. First-wave implementation covers AI Assistant traces only. LangGraph Proxy Trace and RAG Trace stay as documented expansion contracts until ATE-04 hands them off.

**Tech Stack:** FastAPI backend, assistant-service agent loop, PostgreSQL migrations, React/Vite frontend, pytest, ruff, pnpm lint, pnpm type check, and Playwright-backed e2e smoke.

---

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ATE-00",
    "number": "00",
    "title": "Baseline Trace Architecture",
    "status": "ready",
    "type": "baseline",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-trace-eval-prd",
    "phase_file": "deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md",
    "depends_on": [],
    "unlocks": [
      "ATE-01"
    ]
  },
  "goal": {
    "target": "Freeze repo-specific trace architecture, first-wave scope, and validation boundaries before implementation.",
    "prompt": "Complete ATE-00 Baseline Trace Architecture for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md`; work on feature-oracle item ATE-F001; preserve the AI Assistant first-wave boundary; keep LangGraph Proxy Trace and RAG Trace as documented expansion contracts; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-plan.md",
    "completion_report": "deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-report.md"
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
      "deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md"
    ],
    "primary_context": [
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md#Current System Facts",
      "src/api/v1/assistant.py",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "web/src/router.tsx"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/agent-trace-eval-prd/README.md only when harness intent is unclear",
      "deploy/runbooks/agent-trace-eval-prd/phase-manifest.md only when the active phase file is unknown",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md only for targeted current-system lookup or writeback",
      "deploy/runbooks/agent-trace-eval-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json only for ATE-F001 evidence update",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md only for baseline boundary writeback",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md only when preparing a fresh context window"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-report.md",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-critic.md"
    ],
    "do_not_edit": [
      "src/",
      "apps/",
      "packages/",
      "database/",
      "web/",
      "production systems",
      "secret files",
      "deployment configuration"
    ],
    "external_inputs": [
      "public documentation links already recorded in source-packet.md"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation",
      "file patch"
    ],
    "approval_required": [
      "production data mutation",
      "destructive commands",
      "external service changes",
      "deployment"
    ],
    "dangerous_commands": [
      "git reset --hard",
      "rm -rf",
      "production migration"
    ]
  },
  "risk": {
    "tags": [
      "baseline",
      "ai",
      "agent",
      "eval",
      "security"
    ],
    "data_mutation": "none",
    "migration_required": "false",
    "browser_required": "false",
    "ai_eval_required": "design evidence only",
    "external_service_required": "false",
    "release_blocking": "false"
  },
  "validation": {
    "commands": [
      {
        "id": "harness-strict",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score",
        "expected": "Validator exits 0 and reports strict structure readiness for the harness folder.",
        "required": true
      },
      {
        "id": "placeholder-scan",
        "cwd": ".",
        "command": "rg -n '\\b(T[D]O|T[B]D)\\b|\\{\\{[^}]+\\}\\}' deploy/runbooks/agent-trace-eval-prd",
        "expected": "Command exits 1 because no placeholder tokens remain in runtime files or phase contracts.",
        "required": true
      },
      {
        "id": "docs-ignore-proof",
        "cwd": ".",
        "command": "git check-ignore -v docs/agent_trace_eval_prd docs/general_ai_assistant_next_gen/README.md",
        "expected": "Command exits 0 and shows root docs paths are ignored, justifying deploy/runbooks as the durable PRD location.",
        "required": true
      },
      {
        "id": "repo-manifest-proof",
        "cwd": ".",
        "command": "rg --files -g 'pyproject.toml' -g 'Makefile' -g 'package.json' -g 'pnpm-lock.yaml' -g '.github/workflows/*'",
        "expected": "Repository manifests and workflow files are listed so later phases can use concrete backend and frontend validation commands.",
        "required": true
      }
    ],
    "browser_checks": [
      "No browser route is created in ATE-00; any browser-visible edit blocks completion until the route and viewport evidence are recorded in the phase report."
    ],
    "regression_scope": [
      "Baseline phase does not change runtime code.",
      "AI Assistant remains the only first-wave implementation scope.",
      "LangGraph Proxy Trace and RAG Trace stay documented as later expansion contracts."
    ],
    "compliance_gates": [
      "No secrets are read, printed, or committed.",
      "No production data is accessed.",
      "No deployment, migration execution, or external service configuration is performed.",
      "Security review confirms redaction and tenant isolation requirements are present for ATE-01."
    ],
    "acceptance_gates": [
      "ATE-F001 is updated with actor report path and critic artifact path.",
      "source-packet.md records the industry research synthesis and repo-specific trace architecture.",
      "continuity-ledger.md records AI Assistant first-wave scope and LangGraph Proxy plus RAG expansion boundaries.",
      "progress-log.md records validation evidence or a blocker.",
      "agent-handoff.md names ATE-01 as the next action only after ATE-00 passes.",
      "The actor report contains validation evidence, minimal-change scope, and the docs-ignore proof.",
      "A separate independent critic artifact contains Critic Verdict and checks requirement coverage, validation evidence, regression impact, security gates, and minimal-change scope."
    ],
    "rollback_plan": [
      "Revert only ATE-00 runbook file updates if validation fails.",
      "Restore loop-state.json active phase to ATE-00 with status blocked when validation cannot pass.",
      "Do not modify runtime code while repairing baseline planning evidence."
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-report.md"
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
    "next_phase_handoff": "ATE-01 is unlocked only when the trace architecture, API boundary, validation commands, and security gates are recorded with critic approval."
  },
  "stop_conditions": [
    "AI Assistant first-wave scope cannot be separated from LangGraph Proxy or RAG implementation",
    "public documentation evidence is insufficient to justify the trace architecture",
    "docs ignore proof contradicts deploy/runbooks persistence choice",
    "strict harness validation fails after one repair pass"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: ATE-00
- GOAL_TARGET: Freeze repo-specific trace architecture, first-wave scope, and validation boundaries before implementation.
- GOAL_PROMPT: Complete ATE-00 Baseline Trace Architecture for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md`; work on feature-oracle item ATE-F001; preserve the AI Assistant first-wave boundary; keep LangGraph Proxy Trace and RAG Trace as documented expansion contracts; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
- DEPENDS_ON: none
- READ_FIRST: `deploy/runbooks/agent-trace-eval-prd/context-profile.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: `deploy/runbooks/agent-trace-eval-prd/source-packet.md#Current System Facts`, `src/api/v1/assistant.py`, `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`, `web/src/router.tsx`
- LIKELY_EDIT_PATHS: `deploy/runbooks/agent-trace-eval-prd/source-packet.md`, `deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md`, `deploy/runbooks/agent-trace-eval-prd/progress-log.md`, `deploy/runbooks/agent-trace-eval-prd/agent-handoff.md`, `deploy/runbooks/agent-trace-eval-prd/feature-oracle.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, `deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-report.md`, `deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-critic.md`
- DO_NOT_EDIT: `src/`, `apps/`, `packages/`, `database/`, `web/`, production systems, secret files, deployment configuration
- EXECUTION_MODE: plan-first; implement one phase and one feature item; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: `python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score`; `rg -n '\b(T[D]O|T[B]D)\b|\{\{[^}]+\}\}' deploy/runbooks/agent-trace-eval-prd`; `git check-ignore -v docs/agent_trace_eval_prd docs/general_ai_assistant_next_gen/README.md`; `rg --files -g 'pyproject.toml' -g 'Makefile' -g 'package.json' -g 'pnpm-lock.yaml' -g '.github/workflows/*'`
- BROWSER_CHECKS: No browser route is created in ATE-00; any browser-visible edit blocks completion until route and viewport evidence are recorded.
- REGRESSION_SCOPE: Runtime code remains unchanged; AI Assistant remains the only first-wave implementation scope; LangGraph Proxy Trace and RAG Trace stay in expansion contracts.
- COMPLIANCE_GATES: Do not read or write secrets, mutate production data, deploy, run production migrations, or configure external services.
- ROLLBACK_PLAN: Revert ATE-00 runbook file updates and leave runtime code untouched if validation fails.
- ACCEPTANCE_GATES: Actor report exists; validation evidence is recorded; ATE-F001, progress log, handoff, source packet, and continuity ledger are updated; minimal-change scope and independent critic verdict are recorded.
- EVIDENCE_OUTPUT: `deploy/runbooks/agent-trace-eval-prd/reports/ate-00-baseline-trace-architecture-report.md`
- STOP_CONDITIONS: Stop if AI Assistant scope cannot be separated, public documentation evidence is insufficient, docs ignore proof contradicts persistence choice, or strict validation fails after one repair pass.

## Task Spec

Create the durable planning baseline for the Agent Trace Eval module. The first implementation wave is AI Assistant trace and eval only. Future LangGraph Proxy Trace and RAG Trace work must remain visible through contracts, not implementation changes.

## Problem Boundary

In scope: trace architecture, data model plan, API surface plan, UI route plan, validation plan, risk gates, and handoff files under the harness directory.

Out of scope: database migrations, FastAPI routes, assistant-service trace writers, frontend components, deployments, production data access, external observability platform integration.

## Context Policy

Load `READ_FIRST` first. Load each `PRIMARY_CONTEXT` item only to verify current repo facts before writing the actor report. Defer `source-packet.md`, `continuity-ledger.md`, `feature-oracle.json`, and `progress-log.md` until targeted lookup or writeback is needed.

## Requirements

- Record the root trace/run, span, event, and score data model as the shared contract.
- Record the first-wave API endpoints for AI Assistant trace list, detail, and score write.
- Record `/eval` as the future frontend module with Assistant, LangGraph Proxy, and RAG tabs.
- Record LangGraph Proxy and RAG Trace as future slices blocked from ATE-01 through ATE-03 implementation.
- Record security gates for tenant isolation, redaction, and no secret capture.

## Test and Regression Requirements

Run the four validation commands from the Machine Contract. Record exact exit status and key output in the actor report. If the placeholder scan exits 1 because no matches exist, record that exit as the expected pass.

## Compliance and Safety Requirements

Do not read secret files, print environment secrets, access production data, run migrations, deploy, or configure external services.

## Rollback and Recovery

If ATE-00 fails, revert only harness file changes from this phase, keep runtime code untouched, set loop-state status to `blocked`, and write the blocker into the actor report.

## Execution Capture

Write `reports/ate-00-baseline-trace-architecture-report.md` with Status, Validation Evidence, Feature Oracle Updates, Minimal Change, Regression Scope, Compliance Evidence, and Next Phase Handoff. Update source packet, continuity ledger, progress log, feature oracle, handoff, and loop state.

## Critic Protocol

Use a separate critic artifact at `reports/ate-00-baseline-trace-architecture-critic.md`. The critic must state `Critic Verdict`, name the actor report reviewed, and verify scope, validation evidence, redaction and tenant-isolation gates, minimal-change evidence, and ATE-01 readiness.

## Acceptance Criteria

- Strict harness validation passes.
- ATE-F001 has actor and critic evidence paths.
- LangGraph Proxy Trace and RAG Trace are documented but not implemented.
- ATE-01 has exact primary context, edit paths, validation commands, and rollback gates.

## Risks

- If baseline planning mixes all three trace families into the first wave, ATE-01 will be too broad.
- If docs persistence is not proven, this runbook can be lost because root `docs/` is ignored.
- If tenant isolation and redaction gates are vague, later code can store sensitive cross-tenant trace content.
