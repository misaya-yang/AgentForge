# Phase 00 - Comparative Baseline Evidence

> For agentic workers: enter plan-first mode before editing. Execute this phase only, make the smallest requirement-satisfying change, write the required evidence, and do not advance to the next phase until acceptance gates pass or blockers are documented.

**Goal:** Freeze the AI--Platfform vs Hermes Agent plus OpenClaw source-backed comparison and make later implementation phases safe to execute.

**Architecture:** This phase is docs/runbook-only. It must re-check the AI--Platfform and OpenClaw paths named in `product-prd.md`, `source-packet.md`, and `openclaw-synthesis.md`, re-open the local Hermes technical report, correct stale facts, record terminology invariants, and record exact implementation boundaries before code-changing phases begin.

**Tech Stack:** AI--Platfform Assistant service, Eval services/API/UI, Makefile gates, Python/uv test commands, web pnpm commands, local Hermes technical report, and OpenClaw TypeScript/runtime docs used only as comparison evidence.

---

## Machine Contract

The JSON block below is the authoritative machine-readable contract for goal-mode agents and validators. Keep it synchronized with the human-readable sections.

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "AHR-00",
    "number": "00",
    "title": "Comparative Baseline Evidence",
    "status": "ready",
    "type": "baseline",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-hermes-runtime-prd",
    "phase_file": "deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md",
    "depends_on": [],
    "unlocks": [
      "AHR-01"
    ]
  },
  "goal": {
    "target": "Establish baseline code, validation, terminology, and boundary evidence for Assistant Hermes OpenClaw Runtime PRD.",
    "prompt": "Complete AHR-00 Comparative Baseline Evidence for `.` by following `deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md`; work on the matching feature-oracle item, preserve continuity with adjacent phases, write code facts, source matrix, and terminology invariants back to the source packet and continuity ledger, stay inside the named edit boundaries, make the smallest requirement-satisfying change, and finish only after validation, regression, review, compliance, rollback, evidence, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-plan.md",
    "completion_report": "deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-report.md"
  },
  "runtime": {
    "context_profile": "deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json",
    "feature_oracle": "deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json",
    "loop_contract": "deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json",
    "loop_state": "deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json",
    "progress_log": "deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md",
    "handoff": "deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md",
    "continuity_ledger": "deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md",
    "next_window_prompt": "deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md",
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
      "deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json",
      "deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json",
      "deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md"
    ],
    "primary_context": [
      "deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json#roles.actor.primary_context",
      "target phase file",
      "repo paths named by the current phase contract"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/assistant-hermes-runtime-prd/README.md only when the harness intent is unclear",
      "deploy/runbooks/assistant-hermes-runtime-prd/phase-manifest.md only when the target phase file is unknown",
      "deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md only for targeted code fact lookup or writeback",
      "deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json only for the selected feature item",
      "deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md only for dependency boundary lookup or writeback",
      "deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md only when preparing a new window",
      "external dashboards only after approval",
      "production environments only after approval"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md",
      "deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md",
      "deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md",
      "deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md",
      "repo paths explicitly recorded in the source packet"
    ],
    "do_not_edit": [
      "production systems",
      "secret files",
      "deployment configuration",
      "unrelated roadmap or product scopes"
    ],
    "external_inputs": [
      "no external input approved by this PRD; record any required external input before use"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation"
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
      "baseline"
    ],
    "data_mutation": "unknown",
    "migration_required": "unknown",
    "browser_required": "unknown",
    "ai_eval_required": "unknown",
    "external_service_required": "unknown",
    "release_blocking": "unknown"
  },
  "validation": {
    "commands": [
      {
        "id": "prd-source-map-check",
        "cwd": ".",
        "command": "rg -n \"AHR-F001|Hermes|OpenClaw|ExecutionGateway|MemoryProvider|Context Compiler|memory_search|memory_get|transcript|Doctor|Eval|Hermes_Agent_技术分析\" deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json",
        "expected": "The PRD, source packet, OpenClaw synthesis, and oracle contain the AI--Platfform, Hermes-report, and OpenClaw baseline anchors needed for AHR-00.",
        "required": true
      }
    ],
    "browser_checks": [
      "no browser check required unless this phase changes UI; add route evidence before UI completion"
    ],
    "regression_scope": [
      "prior phase report evidence and feature-oracle status remain valid"
    ],
    "compliance_gates": [
      "do not read or write secrets",
      "do not mutate production data",
      "document approval before external service or deployment changes"
    ],
    "acceptance_gates": [
      "phase report exists with validation or blocker evidence",
      "feature-oracle item is updated with evidence or blocker notes",
      "source-packet and continuity-ledger code summaries are updated",
      "progress-log and agent-handoff name the next concrete action",
      "changed files stay inside the smallest phase edit boundary; any expansion is justified in the phase report",
      "independent critic/subagent review checks actor output against requirement coverage, test evidence, regression impact, and minimal-change scope",
      "runtime files contain enough current facts, blockers, decisions, and next actions for a fresh window after context compaction"
    ],
    "rollback_plan": [
      "revert phase-scoped changes and restore runtime docs from git if validation fails"
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-report.md"
    ],
    "required_artifacts": [
      "phase report",
      "progress-log entry",
      "feature-oracle evidence",
      "continuity-ledger update",
      "source-packet code summary",
      "OpenClaw synthesis or source matrix update",
      "terminology invariants",
      "handoff update",
      "independent critic evidence",
      "minimal-change scope note"
    ],
    "waiver_policy": "Only mark a gate waived when the user explicitly waives it or the report documents a blocker and remaining evidence.",
    "next_phase_handoff": "State whether dependent phases are unlocked and what the next agent must know."
  },
  "stop_conditions": [
    "exact code paths are still unknown after baseline inspection",
    "credentials or approvals are required but not documented",
    "destructive commands, production data access, or out-of-scope edits are required"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: AHR-00
- GOAL_TARGET: Establish baseline code, validation, terminology, and boundary evidence for Assistant Hermes OpenClaw Runtime PRD.
- GOAL_PROMPT: Complete AHR-00 Comparative Baseline Evidence for `.` by following `deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md`; work on feature-oracle item AHR-F001; preserve dependency continuity with none; write code facts, source matrix, terminology invariants, and boundary decisions back before handoff; stay inside the named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, review, compliance, rollback, evidence, acceptance gates, and `--completion-gate --phase AHR-00` pass or blockers are documented.
- DEPENDS_ON: none
- READ_FIRST: `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json`, `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json#roles.actor.primary_context, target phase file, repo paths named by the current phase contract
- LIKELY_EDIT_PATHS: deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md, deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md, deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md, deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md, repo paths explicitly recorded in the source packet
- DO_NOT_EDIT: production systems, secret files, deployment configuration, unrelated roadmap or product scopes
- EXECUTION_MODE: plan-first; implement stepwise; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: rg -n "AHR-F001|Hermes|OpenClaw|ExecutionGateway|MemoryProvider|Context Compiler|memory_search|memory_get|transcript|Doctor|Eval|Hermes_Agent_技术分析" deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json
- BROWSER_CHECKS: No browser check required unless this phase changes UI; add route, viewport, and screenshot evidence before UI completion.
- REGRESSION_SCOPE: Prior phase report evidence, feature-oracle status, and continuity-ledger boundaries remain valid.
- COMPLIANCE_GATES: Do not read/write secrets, mutate production data, deploy, or change external services without documented approval.
- ROLLBACK_PLAN: Revert phase-scoped changes and restore runtime docs from git if validation fails.
- ACCEPTANCE_GATES: Phase report exists; validation or blocker evidence is recorded; oracle item, progress log, handoff, source packet, OpenClaw synthesis/source matrix, terminology invariants, and continuity ledger are updated; minimal-change scope and independent critic verdict are recorded.
- EVIDENCE_OUTPUT: `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-report.md`
- STOP_CONDITIONS: Stop if exact code paths, credentials, approvals, destructive commands, production data access, or out-of-scope edits are required but undocumented.

## Harness Runtime

- FEATURE_ORACLE: `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json`
- CONTEXT_PROFILE: `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json`
- LOOP_CONTRACT: `deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json`
- LOOP_STATE: `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json`
- PROGRESS_LOG: `deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md`
- AGENT_HANDOFF: `deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md`
- NEXT_WINDOW_PROMPT: `deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md`
- CONTINUITY_LEDGER: `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md`

Session boot:

1. Read `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json`, `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json`, and this phase file only.
2. Load deferred runtime artifacts only when the context profile trigger applies.
3. Follow the loop contract: observe, select, execute, verify, record, decide.
4. Run the target phase's baseline or smoke validation before implementation when available.
5. Select one matching feature-oracle item and keep work scoped to that item and this phase.
6. Summarize inspected code facts and interface decisions back into targeted source-packet and continuity-ledger sections.
7. Record minimal-change scope and test evidence.
8. Update loop state, progress, continuity, and handoff files before exiting.
9. Hand off to an independent critic/subagent for completion review.
10. Run `--strict --completion-gate --phase AHR-00` before claiming this phase is passed or unlocked.

## Feature Oracle Policy

The feature oracle is the durable test list for long-running agents. Do not delete oracle cases to make completion easier. Update only `status`, `evidence`, and `notes` unless the user explicitly changes scope.

Status rules:

- `failing`: not implemented or not verified.
- `passing`: end-to-end actor evidence exists, cites a phase report with `Status: passed`, and cites an independent critic artifact with `Critic Verdict: approved`.
- `blocked`: a named dependency, credential, environment, or scope issue prevents completion.
- `waived`: the user explicitly waived the case and remaining risk is documented.

## Task Spec

Execute AHR-00 by using the phase contract, updating AHR-F001, and preserving the dependency chain `AHR-00 Comparative Baseline Evidence
  -> AHR-01 Entry Session And Turn Contract
  -> AHR-02 Memory Context And Compaction Lineage
  -> AHR-03 Tool Permission And Runtime Safety
  -> AHR-04 Observability Eval And Regression Cockpit
  -> AHR-05 Operating Model And Release Gate`.

## Cross-Phase Continuity

- Depends on: none
- Unlocks: AHR-01
- Feature-oracle item: AHR-F001
- Continuity ledger: `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md`
- Prior-phase evidence to inherit: no prior phase; establish baseline evidence for dependent phases
- Boundary this phase must preserve for later phases: code/interface facts written by prior phases and any downstream contract named in the continuity ledger
- Handoff this phase must produce: phase report, progress-log entry, oracle evidence, source-packet code summary, continuity-ledger update, and agent-handoff next action

## Code Summary Writeback

Before claiming completion, inspect the code paths allowed by this phase and write back:

- `source-packet.md`: summarize discovered files, services, routes, schemas, tests, commands, and runtime constraints.
- `continuity-ledger.md`: record interface boundaries, dependency assumptions, changed contracts, and any downstream phase impact.
- `agent-handoff.md`: state the next concrete action, active feature-oracle item, validation evidence, and blocker status.
- Phase report: link validation output, independent critic evidence, minimal-change scope notes, the exact code-summary update, and completion-gate result.

## Problem Boundary

In scope:

- Work needed to satisfy AHR-F001 for Comparative Baseline Evidence.
- Code inspection and summary writeback required to keep later phases aligned.

Out of scope:

- Production deployment.
- Production data mutation.
- Unrelated feature work outside this phase chain.

## Context Policy

Before editing, inspect:

- Start with `context-profile.json`, `loop-state.json`, and this phase file.
- Inspect the repository and re-open the local Hermes technical report to establish baseline facts before unlocking implementation phases.
- Open `source-packet.md` and `continuity-ledger.md` only for targeted writeback after inspection.

Do not load unrelated files unless a blocker requires expanding context.
Do not load full runtime artifacts when `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json` provides a narrower trigger or slice.

## Requirements

### R1 Source-Backed Comparison

AHR-00 must prove or correct the comparison claims in `product-prd.md` with local file evidence or report citation. Any assertion about AI--Platfform gaps must cite an AI path or be marked `verify-needed` rather than stated as fact. Any Hermes line-level claim must be marked unavailable unless a Hermes source checkout is located and inspected.

### R2 Implementation Boundary Freeze

AHR-00 must name the concrete AI--Platfform surfaces each later phase may touch, plus protected non-goals. Do not unlock AHR-01 until this boundary is written into `source-packet.md`, `continuity-ledger.md`, and the AHR-00 report.

### R3 Validation Command Map

AHR-00 must record the exact docs, backend, eval, assistant-service, and web commands that later phases should use. If a command cannot run locally, record the command and what a pass should look like.

## Test and Regression Requirements

Run the docs validation command for this PRD and record the exact output:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --quality-score
```

Do not claim backend or frontend tests passed unless they are actually run during AHR-00.

## Compliance and Safety Requirements

Do not expose secrets, mutate production data, deploy, or use external services unless the required approval and source packet entry exist.

## Rollback and Recovery

Revert phase-scoped code changes, restore runtime docs from git, and mark the oracle item blocked if validation cannot be recovered.

## Execution Capture

Write the phase report, append progress-log evidence, update oracle evidence, update continuity-ledger boundaries, and refresh agent-handoff next action.

Use ``deploy/runbooks/assistant-hermes-runtime-prd/reports/phase-report-template.md`` when writing the phase report.

## Critic Protocol

Reject completion if evidence is missing, tests or critic review are absent, code facts were not written back, continuity boundaries are stale, scope expansion lacks justification, or the phase tries to unlock dependent work without a report.

## Acceptance Criteria

- AHR-F001 has evidence or a documented blocker.
- `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-report.md` exists or is named as blocked evidence.
- `source-packet.md`, `continuity-ledger.md`, `progress-log.md`, and `agent-handoff.md` reflect the latest code facts and next action.
- Minimal-change scope, test evidence, and independent critic verdict are recorded.

## Risks

- Phase isolation can break if downstream boundary changes are not recorded.
- Implementation can drift if code summaries stay stale.
- Long-running agents can repeat work if handoff evidence is incomplete.
