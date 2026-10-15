# Phase 05 - Release Regression and Operating Model

> Agentic worker: open `deploy/runbooks/assistant-runtime-optimization/context-profile.json`, `deploy/runbooks/assistant-runtime-optimization/loop-state.json`, and this file only. Execute just this phase, make the smallest requirement-satisfying change, write evidence, then stop at the gates.

- PHASE_ID: ARO-05
- DEPENDS_ON: ARO-04
- UNLOCKS: none
- FEATURE: ARO-F006

**Goal:** Prove whole-demand runtime quality and publish the operating model for the optimized assistant runtime.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "hybrid",
  "phase": {
    "id": "ARO-05",
    "number": "05",
    "title": "Release Regression and Operating Model",
    "status": "ready",
    "type": "release",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-runtime-optimization",
    "phase_file": "deploy/runbooks/assistant-runtime-optimization/phase-05-release-regression-and-operating-model.md",
    "depends_on": ["ARO-04"],
    "unlocks": []
  },
  "goal": {
    "target": "Run whole-demand regression and publish ADR/runbook handoff for optimized runtime operations.",
    "prompt": "Complete ARO-05 Release Regression and Operating Model by following deploy/runbooks/assistant-runtime-optimization/phase-05-release-regression-and-operating-model.md; work only on ARO-F006; run whole-demand regression across completed oracle items; publish ADR/runbook decisions and SLO no-go thresholds; update final evidence and do not deploy.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-plan.md",
    "completion_report": "deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-report.md"
  },
  "runtime": {
    "context_profile": "deploy/runbooks/assistant-runtime-optimization/context-profile.json",
    "feature_oracle": "deploy/runbooks/assistant-runtime-optimization/feature-oracle.json",
    "loop_contract": "deploy/runbooks/assistant-runtime-optimization/loop-contract.json",
    "loop_state": "deploy/runbooks/assistant-runtime-optimization/loop-state.json",
    "progress_log": "deploy/runbooks/assistant-runtime-optimization/progress-log.md",
    "handoff": "deploy/runbooks/assistant-runtime-optimization/agent-handoff.md",
    "continuity_ledger": "deploy/runbooks/assistant-runtime-optimization/continuity-ledger.md",
    "next_window_prompt": "deploy/runbooks/assistant-runtime-optimization/next-window-prompt.md",
    "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true},
    "agent_roles": ["planner", "generator", "critic"]
  },
  "context": {
    "read_first": [
      "deploy/runbooks/assistant-runtime-optimization/context-profile.json",
      "deploy/runbooks/assistant-runtime-optimization/loop-state.json",
      "deploy/runbooks/assistant-runtime-optimization/phase-05-release-regression-and-operating-model.md"
    ],
    "primary_context": [
      "deploy/runbooks/assistant-runtime-optimization/feature-oracle.json",
      "deploy/runbooks/assistant-runtime-optimization/continuity-ledger.md",
      "Makefile",
      "web/package.json"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "full source-packet.md only for final writeback",
      "continuity-ledger.md only for final dependency boundary writeback",
      "feature-oracle.json only for final ARO-F006 and whole-demand status update",
      "progress-log.md only for recent blocker or status history",
      "prior reports only for completed feature evidence",
      "deployment manifests only for rollback documentation, not execution",
      "production logs only after user approval"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "deploy/runbooks/assistant-runtime-optimization/",
      "deploy/runbooks/assistant-runtime-optimization/reports/",
      "deploy/runbooks/assistant-runtime-optimization/feature-oracle.json",
      "deploy/runbooks/assistant-runtime-optimization/progress-log.md",
      "deploy/runbooks/assistant-runtime-optimization/agent-handoff.md",
      "deploy/runbooks/assistant-runtime-optimization/continuity-ledger.md",
      "deploy/runbooks/assistant-runtime-optimization/next-window-prompt.md",
      "deploy/runbooks/assistant-runtime-optimization/source-packet.md"
    ],
    "do_not_edit": ["application source files unless fixing release-blocking regression approved by the report", "production deploy config", "secret files"],
    "external_inputs": [],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["repo search", "shell validation", "apply_patch for runbooks"],
    "approval_required": ["deployment", "production smoke", "schema migration execution", "external provider load test"],
    "dangerous_commands": ["git reset --hard", "rm -rf", "docker compose down", "production migration"]
  },
  "risk": {
    "tags": ["ai", "agent", "eval", "frontend", "release", "auth", "database"],
    "data_mutation": false,
    "migration_required": false,
    "browser_required": true,
    "ai_eval_required": true,
    "external_service_required": false,
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {
        "id": "eval-dev-bundle",
        "cwd": ".",
        "command": "make verify-eval-dev",
        "expected": "Agent trace/eval dev verification bundle passes, including backend, eval, assistant trace, and web gates.",
        "required": true
      },
      {
        "id": "open-source-config",
        "cwd": ".",
        "command": "make validate-example-config",
        "expected": "Open-source example configuration validation passes.",
        "required": true
      },
      {
        "id": "harness-full-completion",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --quality-score",
        "expected": "Strict full-demand completion gate passes after reports and critic artifacts exist.",
        "required": true
      },
      {
        "id": "web-release-smoke",
        "cwd": ".",
        "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web e2e:opensource",
        "expected": "Web lint, type-check, and open-source e2e pass when frontend surfaces changed.",
        "required": false
      }
    ],
    "browser_checks": ["If frontend files changed in any ARO phase, capture Eval/assistant desktop and mobile evidence or cite web e2e output."],
    "regression_scope": ["ARO-F001", "ARO-F002", "ARO-F003", "ARO-F004", "ARO-F005", "assistant stream contract", "approval resume", "trace feedback", "checkpoint resume", "cache/context metrics"],
    "compliance_gates": ["no production deployment", "no secret output", "auth/tenant checks still covered", "rollback/no-go runbook exists"],
    "acceptance_gates": [
      "all completed feature-oracle items cite actor report and critic artifact",
      "whole-demand regression passes or blockers are documented",
      "ADR/runbook decisions exist for middleware, approval, checkpoint, eval feedback, and cache policy",
      "SLO/no-go thresholds are documented",
      "next-window prompt names final state and remaining blockers"
    ],
    "rollback_plan": ["do not deploy in this phase", "document feature flags or revert boundaries for each implemented runtime slice", "leave blocked features failing rather than deleting oracle items"]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-report.md",
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-critic.md"
    ],
    "required_artifacts": ["phase report", "critic artifact", "whole-demand regression output", "ADR/runbook links", "feature oracle final evidence", "progress log entry", "continuity ledger update", "source packet update", "handoff update", "minimal-change summary"],
    "waiver_policy": "Any skipped release gate requires explicit waiver, reason, residual risk, and dependent-phase impact.",
    "next_phase_handoff": "No dependent phase. State remaining backlog or final release blocker."
  },
  "stop_conditions": ["full regression cannot run and no waiver exists", "production deploy is requested without explicit approval", "feature oracle evidence is inconsistent"]
}
```

## Requirements

### R1 Whole-Demand Regression

The terminal report must verify every completed oracle item and state the status of every blocked or waived item.

### R2 Operating Model

The release handoff must document ADR/runbook decisions, SLO/no-go thresholds, rollback boundaries, and the commands operators should trust.

### R3 No Deployment

This phase must not deploy or mutate production; it prepares release evidence and operating docs only.

## Critic Protocol

Reject completion if any completed oracle item lacks actor and critic evidence, regression commands are skipped without waiver, release docs omit rollback/no-go thresholds, or the report implies deployment happened.
