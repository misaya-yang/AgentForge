# Phase 03 - Durable Long Task Runtime

> Agentic worker: open `deploy/runbooks/assistant-runtime-optimization/context-profile.json`, `deploy/runbooks/assistant-runtime-optimization/loop-state.json`, and this file only. Execute just this phase, make the smallest requirement-satisfying change, write evidence, then stop at the gates.

- PHASE_ID: ARO-03
- DEPENDS_ON: ARO-02
- UNLOCKS: ARO-04
- FEATURE: ARO-F004

**Goal:** Add lightweight AgentLoop checkpoint/resume and idempotent long-run recovery before considering Temporal or another workflow engine.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ARO-03",
    "number": "03",
    "title": "Durable Long Task Runtime",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-runtime-optimization",
    "phase_file": "deploy/runbooks/assistant-runtime-optimization/phase-03-durable-long-task-runtime.md",
    "depends_on": ["ARO-02"],
    "unlocks": ["ARO-04"]
  },
  "goal": {
    "target": "Persist resumable run checkpoints and prevent duplicate side effects during resume.",
    "prompt": "Complete ARO-03 Durable Long Task Runtime by following deploy/runbooks/assistant-runtime-optimization/phase-03-durable-long-task-runtime.md; work only on ARO-F004; add additive checkpoint/resume behavior with idempotency tests; do not adopt Temporal unless the report proves the local checkpoint contract is insufficient and records a PoC-only boundary.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-plan.md",
    "completion_report": "deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-report.md"
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
      "deploy/runbooks/assistant-runtime-optimization/phase-03-durable-long-task-runtime.md"
    ],
    "primary_context": [
      "database/migrations/",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py",
      "apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "Temporal docs only when writing PoC rationale",
      "web/src only if resume UX scope is explicitly approved",
      "production database state never without user approval",
      "source-packet.md only for checkpoint contract writeback",
      "continuity-ledger.md only for dependency boundary lookup or writeback",
      "feature-oracle.json only for ARO-F004 status update",
      "progress-log.md only for recent blocker or status history"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "database/migrations/",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py",
      "apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py",
      "src/api/v1/_assistant_proxy.py",
      "src/api/v1/assistant.py",
      "tests/services/assistant/",
      "tests/contract/",
      "tests/api/test_assistant_sessions.py",
      "deploy/runbooks/assistant-runtime-optimization/"
    ],
    "do_not_edit": ["external workflow engine integration except PoC notes", "production compose files", "provider dashboards"],
    "external_inputs": [],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["repo search", "apply_patch", "shell validation"],
    "approval_required": ["schema migration execution", "production data access", "deployment", "Temporal/cloud service setup"],
    "dangerous_commands": ["DROP", "TRUNCATE", "docker compose down", "rm -rf"]
  },
  "risk": {
    "tags": ["ai", "agent", "database", "migration", "auth", "release"],
    "data_mutation": false,
    "migration_required": true,
    "browser_required": false,
    "ai_eval_required": true,
    "external_service_required": false,
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-checkpoint",
        "cwd": ".",
        "command": "uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py apps/assistant-service/src/assistant_service/core/assistant_service.py apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/contract/test_find_active_command.py tests/contract/test_migrated_routes_equivalence.py",
        "expected": "Ruff reports no errors in checkpoint/resume code and tests.",
        "required": true
      },
      {
        "id": "checkpoint-runtime-tests",
        "cwd": ".",
        "command": "uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py",
        "expected": "AgentLoop contracts pass with checkpoint behavior.",
        "required": true
      },
      {
        "id": "run-state-contract-tests",
        "cwd": ".",
        "command": "uv run pytest -q --no-cov tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py tests/api/test_assistant_sessions.py",
        "expected": "Run state, command de-dup, and assistant session contracts pass.",
        "required": true
      }
    ],
    "browser_checks": [],
    "regression_scope": ["run persistence", "approval pause/resume", "tool idempotency", "trace terminal events", "migration idempotency"],
    "compliance_gates": ["checkpoint payload excludes raw secrets and unbounded prompts", "tenant/user filters apply to resume", "side-effecting tools require idempotency keys"],
    "acceptance_gates": [
      "checkpoint schema or storage is additive and documented",
      "resume after approval pause restores enough state to continue",
      "resume after simulated interruption does not duplicate completed tool calls",
      "failed resume surfaces a blocked run state and trace evidence",
      "critic verifies rollback or forward-fix path for migration"
    ],
    "rollback_plan": ["disable resume endpoint or feature flag", "leave additive checkpoint table unused if runtime validation fails", "document migration rollback or forward fix"]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-report.md",
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-critic.md"
    ],
    "required_artifacts": ["phase report", "critic artifact", "migration validation notes", "resume/idempotency test evidence", "minimal-change note", "feature oracle evidence", "progress log entry", "continuity ledger update", "source packet update", "handoff update"],
    "waiver_policy": "A missing checkpoint/resume test blocks ARO-04 unless explicitly waived.",
    "next_phase_handoff": "Document checkpoint metrics and cache/context interaction assumptions for ARO-04."
  },
  "stop_conditions": ["checkpoint needs production data to test", "migration cannot be additive", "resume requires a new external workflow engine to be safe"]
}
```

## Requirements

### R1 Checkpoint Contract

Each resumable run must persist phase, iteration, message-state hash, pending tool or approval state, and idempotency metadata without storing secrets.

### R2 Resume Behavior

Resume must continue from the latest safe checkpoint and prevent duplicate side effects for completed tools.

### R3 Failure Semantics

If resume cannot proceed, the run must become blocked or failed with user-visible status and trace evidence.

## Critic Protocol

Reject completion if checkpoint data stores raw secrets, schema changes are destructive, resume can double-execute a tool, tenant/user scoping is missing, or Temporal/cloud setup is introduced as a required dependency without approval.
