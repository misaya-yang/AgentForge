# Phase 09 - Terminal Whole-Demand Release Gate

> Agentic worker: perform no feature implementation. Run one compatible-build regression, aggregate durable evidence, obtain an independent release critique, then let the orchestrator run the completion gate.

- PHASE_ID: AS-09
- DEPENDS_ON: AS-08
- UNLOCKS: none
- FEATURE: AS-F010

**Goal:** Produce an evidence-backed released, ready-but-not-deployed, or blocked decision after rerunning every Agent Studio and built-in Assistant gate in one compatible build.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "evaluation",
  "phase": {"id": "AS-09", "number": "09", "title": "Terminal Whole-Demand Release Gate", "status": "ready", "type": "release", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-09-terminal-whole-demand-release-gate.md", "depends_on": ["AS-08"], "unlocks": []},
  "goal": {
    "target": "Run the versioned aggregate regression and terminal browser/migration/security/rollback review without feature edits, then issue a critic-approved release decision and post-critic completion-gate handoff.",
    "prompt": "Complete AS-09 by following deploy/runbooks/agent-studio-prd/phase-09-terminal-whole-demand-release-gate.md after AS-08 passes; do not implement or repair features in this phase, run make verify-agent-studio and the supported structure validator in one compatible build, execute approved runtime/header/migration smokes with ownership checks, aggregate all AS-F001 through AS-F010 evidence, write the terminal actor report, obtain an independent release critic, and hand off to the orchestrator for the claim-check command only after actor and critic artifacts exist.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-09-terminal-whole-demand-release-gate.md"],
    "primary_context": ["Makefile and tests/fixtures/agent-studio/regression_manifest.json", "deploy/runbooks/agent-studio-prd/phase-manifest.md report index and validation matrix", "reports/agent-studio artifacts produced by AS-00 through AS-08", "repository AGENTS.md Docker ownership, provider and deployment constraints"],
    "context_budget": "focused",
    "do_not_load_unless": ["prior actor/critic reports only when the aggregate result or manifest points to their evidence", "production dashboards only after explicit release approval", "Docker/live runtime only after compose ownership checks and explicit approval", "deployment target only after all local gates pass and explicit approval", "source-packet.md only for terminal code-fact reconciliation/writeback", "continuity-ledger.md only for terminal invariant audit/writeback", "feature-oracle.json only for AS-F010 and final evidence update", "progress-log.md only for blocker history or terminal exit append"]
  },
  "boundaries": {
    "likely_edit_paths": ["deploy/runbooks/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-plan.md", "deploy/runbooks/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-report.md", "deploy/runbooks/agent-studio-prd/reports/as-09-critic-verdict.md", "deploy/runbooks/agent-studio-prd/feature-oracle.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/progress-log.md", "deploy/runbooks/agent-studio-prd/agent-handoff.md", "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "deploy/runbooks/agent-studio-prd/source-packet.md", "reports/agent-studio/as-09-*"],
    "do_not_edit": ["application source", "database migrations", "tests or aggregate manifest", "frontend source/config", "deployment configuration", "production data", "earlier actor/critic evidence"],
    "external_inputs": ["AS-00 through AS-08 passed actor reports and independent critic verdicts", "critic-approved make verify-agent-studio manifest/version", "explicit Docker/live migration/provider/deployment approval plus a separately reviewed command plan because no repository-owned Agent Studio live-smoke script exists", "release owner, monitoring window and rollback trigger"],
    "secrets_required": ["DASHSCOPE_CHAT_API_KEY for approved live smoke", "GATEWAY_ASSISTANT_SHARED_SECRET for approved internal smoke", "monitoring credentials for approved release verification"]
  },
  "tool_policy": {
    "allowed_tools": ["read-only repo/report inspection", "make verify-agent-studio", "supported harness validation", "Playwright/axe through the aggregate", "approved read-only Docker ownership and health checks"],
    "approval_required": ["Docker stop/up/build", "live migration", "external provider smoke", "production dashboard mutation", "deployment", "commit or push"],
    "dangerous_commands": ["edit feature code to hide a failure", "delete or weaken a gate", "DROP/TRUNCATE", "docker prune", "force push", "git reset --hard", "deploy without explicit approval"]
  },
  "risk": {"tags": ["release", "security", "privacy", "migration", "browser", "agent", "ai", "eval", "external-service"], "data_mutation": false, "migration_required": true, "browser_required": true, "ai_eval_required": true, "external_service_required": true, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "whole-demand-aggregate", "cwd": ".", "command": "make verify-agent-studio", "expected": "The critic-approved manifest runs every AS-00 through AS-08 required backend, Assistant, migration, security, Eval, frontend, browser, built-header and built-in Assistant gate in one compatible build and writes a versioned result summary.", "required": true},
      {"id": "harness-structure", "cwd": ".", "command": "python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py deploy/runbooks/agent-studio-prd --quality-score", "expected": "Harness structure and quality pass without claiming implementation completion before the terminal actor/critic artifacts exist.", "required": true}
    ],
    "browser_checks": ["Rerun the complete AS-05/06/07/08 route-state-viewport matrix at 1440x900, 1024x768 and 390x844 through make verify-agent-studio.", "Inspect actual built responses for Hosted anti-framing and dedicated Embed frame-ancestors/XFO behavior, plus allowed/rejected Origin fixtures.", "Verify Agent Studio flag off preserves /assistant, /knowledge, /eval and /share with no console/network regression.", "Confirm terminal axe/keyboard/focus/reduced-motion, token/secret redaction and screenshot evidence belongs to the same build hash."],
    "regression_scope": ["whole-demand regression across AS-F001 through AS-F010", "all phase-specific required command IDs", "built-in Assistant chat/sessions/tools/KB/search/memory", "MCP/Connector credential modes", "Skill entrypoint and Knowledge provenance", "publish atomicity/eval", "Hosted/Embed/API headers/auth", "operations/governance", "migration and feature-flag rollback"],
    "compliance_gates": ["no feature or test edits occur in terminal evaluation", "all prior dependencies have passed actor and critic evidence", "aggregate manifest hash and build hash are recorded", "PII/Secret/tenant/security/accessibility/migration gates pass", "Docker ownership is verified before approved runtime actions", "deployment remains separate explicit authorization", "completion-gate command runs only after terminal actor report, critic verdict and Oracle evidence exist"],
    "acceptance_gates": ["One aggregate result proves every earlier required gate ran in one compatible build; no Phase-specific test is merely referenced from an older report.", "Terminal report states released, ready-but-not-deployed, or blocked and lists residual risk, owner, monitoring and rollback trigger.", "Any aggregate failure stops AS-09 and is routed back to the owning Phase; AS-09 does not patch source/tests to obtain green.", "Independent release critic approves whole-demand, security, migration, browser/header, rollback and evidence integrity.", "After critic approval, the orchestrator updates AS-F010 evidence and runs --claim-check --phase AS-09 --quality-score; that post-critic result is appended rather than declared by the actor in advance."],
    "rollback_plan": ["If no deployment occurred, leave features disabled or at the last approved rollout percentage and retain evidence.", "If an approved deployment occurred, disable public channels, revoke tokens/grants, repoint Publications to last healthy Versions and roll back application code while retaining additive schema/audits.", "Route any test or product failure to its owner Phase and rerun the complete aggregate after a new actor/critic cycle."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-09-terminal-whole-demand-release-gate-report.md", "deploy/runbooks/agent-studio-prd/reports/as-09-critic-verdict.md", "reports/agent-studio/as-09-whole-demand-matrix.md", "reports/agent-studio/as-09-build-and-manifest.json", "reports/agent-studio/as-09-release-decision.md"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet reconciliation", "handoff update", "whole-demand regression aggregate log", "build and manifest hashes", "terminal browser/header/accessibility evidence", "migration and feature-flag rollback evidence", "built-in Assistant regression evidence", "deployment blocker or approved live smoke", "independent release critic evidence", "minimal-change scope note", "post-critic completion-gate result"],
    "waiver_policy": "Only deployment and an approved live-provider smoke may remain deferred for a ready-but-not-deployed result; whole-demand local aggregate, tenant/security/secret/privacy, migration safety, built-in Assistant compatibility, header security, rollback readiness, critical accessibility and independent release criticism cannot be waived for implementation completion.",
    "next_phase_handoff": "No implementation phase unlocks. After the actor report and independent critic approve, the orchestrator updates AS-F010 plus runtime artifacts and runs --claim-check --phase AS-09. The final status must name release/deploy separation and every residual risk."
  },
  "stop_conditions": ["AS-08 or any earlier dependency is not passed", "any Oracle lacks actor or critic evidence", "aggregate manifest/hash differs from the critic-approved AS-08 artifact", "whole-demand suites fail", "source/test edits are required", "migration or feature-flag rollback is destructive", "Docker ownership is wrong", "live migration/deployment is required without explicit approval"]
}
```

## Requirements

### R1 No-Feature Terminal Evaluation

AS-09 may write only plans, reports, Oracle/runtime evidence and release artifacts; failures return to the owning Phase and trigger a fresh full aggregate.

### R2 One Compatible-Build Aggregate

The versioned aggregate manifest must execute every required AS-00 through AS-08 gate, existing Assistant regression and complete browser/header matrix in one build with recorded hashes.

### R3 Release/Deployment Separation

The report distinguishes implementation completion from deployment authorization and states released, ready-but-not-deployed, or blocked with residual risks, owners, monitoring and rollback triggers.

### R4 Post-Critic Completion Gate

The actor does not run or claim the full completion gate. Only after the terminal actor report and independent critic verdict exist may the orchestrator mark AS-F010 and run `--claim-check --phase AS-09`.

## Critic Protocol

Reject if AS-09 edits source/tests, prior reports substitute for terminal reruns, the aggregate omits a Phase-specific gate, build/manifest hashes are absent, browser validation uses Vite instead of built header behavior, built-in Assistant/rollback is incomplete, deployment is implied without approval, or the actor pre-claims the completion gate before independent criticism.
