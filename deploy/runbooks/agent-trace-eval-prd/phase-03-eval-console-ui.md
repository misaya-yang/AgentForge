# Phase 03 - Eval Console UI

> Enter plan-first mode before editing. Execute this phase only, work on ATE-F004 only, and depend on ATE-02 trace capture evidence.

**Goal:** Add the first Eval console tab for AI Assistant trace review and scoring.

**Architecture:** This phase creates the protected `/eval` module in the authenticated app shell. The Assistant tab is functional in the first wave. LangGraph Proxy and RAG tabs are visible only as guarded future sections so users understand the roadmap without receiving incomplete functionality.

**Tech Stack:** React, Vite, TypeScript, React Router, Ant Design, lucide-react icons, existing app layout/navigation, Eval API client, Playwright e2e smoke, pnpm lint, and pnpm type check.

---

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ATE-03",
    "number": "03",
    "title": "Eval Console UI",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-trace-eval-prd",
    "phase_file": "deploy/runbooks/agent-trace-eval-prd/phase-03-eval-console-ui.md",
    "depends_on": [
      "ATE-02"
    ],
    "unlocks": [
      "ATE-04"
    ]
  },
  "goal": {
    "target": "Add the first Eval console tab for AI Assistant trace review and scoring.",
    "prompt": "Complete ATE-03 Eval Console UI for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-03-eval-console-ui.md`; work on feature-oracle item ATE-F004; depend on ATE-02 trace capture evidence; implement only the Assistant trace explorer and score workflow in the Eval module; keep LangGraph Proxy and RAG tabs guarded for later phases; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, browser checks, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-plan.md",
    "completion_report": "deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md"
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
      "deploy/runbooks/agent-trace-eval-prd/phase-03-eval-console-ui.md"
    ],
    "primary_context": [
      "web/src/router.tsx",
      "web/src/layouts/AppLayout.tsx",
      "web/src/pages/dashboard/components/panels/RequestTracePanel.tsx",
      "web/src/api/usage.ts"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/agent-trace-eval-prd/README.md only when harness intent is unclear",
      "deploy/runbooks/agent-trace-eval-prd/phase-manifest.md only when dependency order is unclear",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md only for UI contract lookup or writeback",
      "deploy/runbooks/agent-trace-eval-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json only for ATE-F004 evidence update",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md only for ATE-02 dependency lookup or ATE-03 writeback",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md only when preparing a fresh context window"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "web/src/api/eval.ts",
      "web/src/pages/eval/index.tsx",
      "web/src/pages/eval/components/AssistantTraceList.tsx",
      "web/src/pages/eval/components/AssistantTraceDetail.tsx",
      "web/src/pages/eval/components/TraceScorePanel.tsx",
      "web/src/router.tsx",
      "web/src/layouts/AppLayout.tsx",
      "web/e2e/eval-trace.spec.ts",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-critic.md",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json"
    ],
    "do_not_edit": [
      "database/",
      "apps/assistant-service/",
      "src/api/v1/langgraph.py",
      "src/api/v1/eval.py except ATE-02 handoff reports an API display blocker",
      "production systems",
      "secret files",
      "deployment configuration"
    ],
    "external_inputs": [
      "ATE-02 actor report and critic artifact"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation",
      "file patch",
      "pnpm",
      "browser verification"
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
      "frontend",
      "ui",
      "browser",
      "auth",
      "security",
      "eval"
    ],
    "data_mutation": "score API writes only in seeded local or mocked browser tests",
    "migration_required": "false",
    "browser_required": "true",
    "ai_eval_required": "trace review and score workflow",
    "external_service_required": "false",
    "release_blocking": "true"
  },
  "validation": {
    "commands": [
      {
        "id": "web-lint",
        "cwd": ".",
        "command": "pnpm -C web lint",
        "expected": "Web lint exits 0 after Eval UI changes.",
        "required": true
      },
      {
        "id": "web-type-check",
        "cwd": ".",
        "command": "pnpm -C web type-check",
        "expected": "TypeScript type check exits 0 after Eval API client and page changes.",
        "required": true
      },
      {
        "id": "web-e2e-open-source",
        "cwd": ".",
        "command": "pnpm -C web e2e:opensource",
        "expected": "Open-source e2e smoke exits 0 and includes the Eval route smoke added in this phase.",
        "required": true
      },
      {
        "id": "eval-api-regression",
        "cwd": ".",
        "command": "uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py",
        "expected": "Eval API tests exit 0 so the UI client remains aligned with backend schemas.",
        "required": true
      },
      {
        "id": "harness-strict",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score",
        "expected": "Harness strict validation exits 0 after ATE-03 evidence writeback.",
        "required": true
      }
    ],
    "browser_checks": [
      "Open /eval at 1440x900 as an authorized user and record Assistant tab list, filter, detail, and score states.",
      "Open /eval at 390x844 as an authorized user and record no horizontal overflow, readable timeline, and reachable score controls.",
      "Verify keyboard focus moves through nav, tab switcher, filters, trace rows, detail controls, and score submission.",
      "Verify LangGraph Proxy and RAG tabs are guarded and do not present completed trace data."
    ],
    "regression_scope": [
      "Existing /assistant route and app navigation remain available.",
      "Existing dashboard request trace panel remains unchanged.",
      "Eval API client preserves ATE-01 response contract.",
      "ATE-02 latency guard remains documented in the UI as a metric display, not a blocking frontend behavior."
    ],
    "compliance_gates": [
      "Eval route is protected by the existing authenticated app shell.",
      "Trace payload previews render redaction markers and do not expose secrets.",
      "Score submission uses authenticated user context and never accepts tenant_id from the browser.",
      "No external observability service is called from the browser."
    ],
    "acceptance_gates": [
      "/eval route exists and is reachable through authenticated navigation.",
      "Assistant tab lists traces with filters for status, model, user, session, run id, date range, and score status.",
      "Trace detail renders timeline spans, events, usage, latency, errors, redaction markers, linked session id, linked run id, and request id.",
      "Score panel displays existing scores and submits a bounded score or feedback item through the Eval API.",
      "Empty, loading, error, unauthorized, desktop, and mobile states are covered by tests or browser evidence.",
      "Actor report records validation evidence, browser evidence, minimal-change scope, and security proof.",
      "A separate independent critic artifact contains Critic Verdict and checks UI coverage, browser evidence, auth gating, redaction display, accessibility, regression impact, and minimal-change scope."
    ],
    "rollback_plan": [
      "Remove /eval route registration and app navigation entry.",
      "Remove Eval API client and Eval page files created by this phase.",
      "Revert Eval e2e smoke added by this phase.",
      "Leave ATE-01 API and ATE-02 trace capture intact."
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md"
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
    "next_phase_handoff": "ATE-04 is unlocked only when the Eval UI proves Assistant trace review and score workflow on desktop and mobile without breaking existing routes."
  },
  "stop_conditions": [
    "ATE-02 completion evidence is missing or rejected",
    "Eval API contract is unavailable",
    "Authenticated route protection cannot be preserved",
    "Browser checks show horizontal overflow or inaccessible score controls",
    "UI requires backend schema changes outside the ATE-01 contract"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: ATE-03
- GOAL_TARGET: Add the first Eval console tab for AI Assistant trace review and scoring.
- GOAL_PROMPT: Complete ATE-03 Eval Console UI for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-03-eval-console-ui.md`; work on feature-oracle item ATE-F004; depend on ATE-02 trace capture evidence; implement only the Assistant trace explorer and score workflow in the Eval module; keep LangGraph Proxy and RAG tabs guarded for later phases; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, browser checks, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
- DEPENDS_ON: ATE-02
- READ_FIRST: `deploy/runbooks/agent-trace-eval-prd/context-profile.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: `web/src/router.tsx`, `web/src/layouts/AppLayout.tsx`, `web/src/pages/dashboard/components/panels/RequestTracePanel.tsx`, `web/src/api/usage.ts`
- LIKELY_EDIT_PATHS: `web/src/api/eval.ts`, `web/src/pages/eval/index.tsx`, `web/src/pages/eval/components/AssistantTraceList.tsx`, `web/src/pages/eval/components/AssistantTraceDetail.tsx`, `web/src/pages/eval/components/TraceScorePanel.tsx`, `web/src/router.tsx`, `web/src/layouts/AppLayout.tsx`, `web/e2e/eval-trace.spec.ts`, harness report and runtime writeback files for ATE-03
- DO_NOT_EDIT: `database/`, `apps/assistant-service/`, `src/api/v1/langgraph.py`, production systems, secret files, deployment configuration
- EXECUTION_MODE: plan-first; implement one phase and one feature item; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: `pnpm -C web lint`; `pnpm -C web type-check`; `pnpm -C web e2e:opensource`; `uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py`; `python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score`
- BROWSER_CHECKS: `/eval` desktop 1440x900, `/eval` mobile 390x844, keyboard focus path, guarded LangGraph Proxy tab, guarded RAG tab
- REGRESSION_SCOPE: Existing /assistant route, dashboard request trace panel, Eval API client contract, and ATE-02 latency guard evidence remain valid.
- COMPLIANCE_GATES: Authenticated app shell is preserved; redaction markers display; score submission uses authenticated user context; no browser call goes to external observability services.
- ROLLBACK_PLAN: Remove Eval route, nav entry, Eval API client, Eval page files, and Eval e2e smoke while leaving backend trace capture intact.
- ACCEPTANCE_GATES: Assistant trace list, detail timeline, score workflow, desktop/mobile browser evidence, tests, source-packet writeback, continuity ledger update, minimal-change scope, and independent critic verdict are complete.
- EVIDENCE_OUTPUT: `deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md`
- STOP_CONDITIONS: Stop if ATE-02 is not passed, Eval API contract is unavailable, route protection cannot be preserved, browser checks fail, or UI needs backend schema changes outside ATE-01.

## Task Spec

Implement the first Eval console page for AI Assistant traces. The page must support operational review, comparison, and scoring of traces produced by ATE-02.

## Problem Boundary

In scope: protected route, navigation entry, Eval API client, Assistant trace list, trace detail timeline, score panel, guarded future tabs, browser checks, web tests, and harness evidence.

Out of scope: LangGraph Proxy trace implementation, RAG trace implementation, database schema changes, assistant-service changes, external observability exports.

## Context Policy

Load only the four `PRIMARY_CONTEXT` paths after `READ_FIRST`. Open ATE-02 reports and ledger rows only to confirm the trace API and latency guard handoff. Defer source packet, continuity ledger, feature oracle, and progress log until targeted writeback.

## Requirements

- Add a protected `/eval` route and navigation entry.
- Build an Assistant tab with filters, list table, detail timeline, metadata panels, score panel, and redaction indicators.
- Render LangGraph Proxy and RAG tabs as guarded future sections without fake trace data.
- Preserve operational dashboard density and avoid marketing-style layouts.
- Keep UI elements readable on desktop and mobile viewports.

## Test and Regression Requirements

Run the validation commands from the Machine Contract. Capture browser evidence for desktop and mobile. Verify keyboard focus, no horizontal overflow, unauthorized behavior, loading state, empty state, error state, and score submission state.

## Compliance and Safety Requirements

Do not display raw secrets or unredacted payloads. Do not accept tenant identifiers from browser score submission. Do not call external observability vendors from the frontend.

## Rollback and Recovery

Rollback removes UI-only files, route registration, navigation entry, and e2e smoke from ATE-03 while preserving backend trace storage and runtime capture.

## Execution Capture

Write `reports/ate-03-eval-console-ui-report.md` with Status, Validation Evidence, Browser Evidence, Feature Oracle Updates, Minimal Change, Regression Scope, Compliance Evidence, Rollback Evidence, and Next Phase Handoff. Update source packet, continuity ledger, progress log, feature oracle, handoff, and loop state.

## Critic Protocol

Use `reports/ate-03-eval-console-ui-critic.md`. The critic must state `Critic Verdict`, name the actor report reviewed, and verify UI coverage, browser evidence, auth gating, redaction display, keyboard access, regression impact, and minimal-change scope.

## Acceptance Criteria

- `/eval` works for an authorized user.
- Assistant trace list, detail, and score workflow are usable.
- LangGraph Proxy and RAG tabs are guarded.
- Desktop and mobile checks pass without overflow.
- Existing assistant and dashboard routes still work.

## Risks

- Trace payload display can leak sensitive content if redaction markers are ignored.
- A wide timeline or table can overflow on mobile.
- Adding a route or nav entry can accidentally bypass existing auth boundaries.
