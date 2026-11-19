# Phase 01 - Middleware Harness and Approval Completion

> Agentic worker: open `deploy/runbooks/assistant-runtime-optimization/context-profile.json`, `deploy/runbooks/assistant-runtime-optimization/loop-state.json`, and this file only. Execute just this phase, make the smallest requirement-satisfying change, write evidence, then stop at the gates.

- PHASE_ID: ARO-01
- DEPENDS_ON: ARO-00
- UNLOCKS: ARO-02
- FEATURE: ARO-F002

**Goal:** Complete middleware lifecycle hooks and approval/resume semantics without a broad AgentLoop rewrite.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ARO-01",
    "number": "01",
    "title": "Middleware Harness and Approval Completion",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-runtime-optimization",
    "phase_file": "deploy/runbooks/assistant-runtime-optimization/phase-01-middleware-harness-and-approval-completion.md",
    "depends_on": ["ARO-00"],
    "unlocks": ["ARO-02"]
  },
  "goal": {
    "target": "Wire lifecycle middleware and persist CONFIRM approval/resume with no duplicate tool execution.",
    "prompt": "Complete ARO-01 Middleware Harness and Approval Completion by following deploy/runbooks/assistant-runtime-optimization/phase-01-middleware-harness-and-approval-completion.md; work only on ARO-F002; implement lifecycle middleware hooks and persisted approval resume inside the named paths; write actor and critic evidence; update runtime artifacts; stop before ARO-02.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-plan.md",
    "completion_report": "deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-report.md"
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
      "deploy/runbooks/assistant-runtime-optimization/phase-01-middleware-harness-and-approval-completion.md"
    ],
    "primary_context": [
      "apps/assistant-service/src/assistant_service/core/agent/middleware.py",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py",
      "apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "source-packet.md only for ARO-01 code fact writeback",
      "continuity-ledger.md only for dependency boundary writeback",
      "feature-oracle.json only for ARO-F002 status update",
      "progress-log.md only for recent blocker or status history",
      "web/src only if approval resume requires UI work and the report records scope expansion",
      "database/migrations only if additive approval/checkpoint schema is required and ARO-03 ownership is not violated"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "apps/assistant-service/src/assistant_service/core/agent/middleware.py",
      "apps/assistant-service/src/assistant_service/core/agent/middlewares/",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py",
      "apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py",
      "tests/services/assistant/",
      "tests/contract/test_adr004_no_silent_regression.py",
      "tests/contract/test_find_active_command.py",
      "deploy/runbooks/assistant-runtime-optimization/"
    ],
    "do_not_edit": ["production deployment config", "provider credentials", "unrelated tools", "Eval UI except documented approval-resume scope expansion"],
    "external_inputs": [],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["repo search", "apply_patch", "shell validation"],
    "approval_required": ["schema migration", "deployment", "production data mutation"],
    "dangerous_commands": ["git reset --hard", "rm -rf", "docker compose down"]
  },
  "risk": {
    "tags": ["ai", "agent", "auth", "eval"],
    "data_mutation": false,
    "migration_required": false,
    "browser_required": false,
    "ai_eval_required": true,
    "external_service_required": false,
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-agent-middleware",
        "cwd": ".",
        "command": "uv run ruff check apps/assistant-service/src/assistant_service/core/agent/middleware.py apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/agent/middlewares/harness.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py tests/services/assistant/test_middleware_chain.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_harness_middlewares.py tests/contract/test_find_active_command.py",
        "expected": "Ruff reports no errors in the ARO-01 changed middleware, AgentLoop, gateway, and focused tests. The previous broad tests/services/assistant sweep is intentionally not used because it includes unrelated pre-existing lint failures.",
        "required": true
      },
      {
        "id": "assistant-runtime-contract",
        "cwd": ".",
        "command": "uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_agent_trace_capture.py",
        "expected": "AgentLoop config/event/trace contracts pass.",
        "required": true
      },
      {
        "id": "approval-gateway-contract",
        "cwd": ".",
        "command": "uv run pytest -q --no-cov tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py",
        "expected": "DB-authoritative runs and command de-dup contracts pass.",
        "required": true
      }
    ],
    "browser_checks": [],
    "regression_scope": ["streaming-first event schema", "trace capture", "run/approval DB authority", "tool execution idempotency"],
    "compliance_gates": ["approval events never expose raw tool arguments or secrets", "permission failures remain tenant-scoped", "middleware exceptions cannot bypass explicit deny"],
    "acceptance_gates": [
      "on_stream_event and on_error hooks are wired with tests",
      "loop/call/time/precompletion middleware has focused tests",
      "CONFIRM creates a persisted approval_id or reports a blocker",
      "approved resume executes the intended tool exactly once",
      "critic checks minimal-change scope and no broad AgentLoop rewrite"
    ],
    "rollback_plan": ["disable new middleware via config or remove from default chain", "revert approval-resume code if idempotency tests fail"]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-report.md",
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-critic.md"
    ],
    "required_artifacts": ["phase report", "critic artifact", "test output", "minimal-change note", "feature oracle evidence", "progress log entry", "continuity ledger update", "source packet update", "handoff update"],
    "waiver_policy": "Missing approval resume proof blocks ARO-02 unless the user explicitly waives with residual risk.",
    "next_phase_handoff": "Document middleware event/error payloads and approval resume contract for trace feedback."
  },
  "stop_conditions": ["approval resume needs UI/schema work outside this phase", "tool idempotency cannot be proven", "tests require live provider credentials"]
}
```

## Requirements

### R1 Lifecycle Middleware

The middleware chain must expose stream-event and error hooks that are called from the streaming-first loop, preserve event ordering, and isolate middleware failures.

### R2 Approval Resume

A confirm verdict must create durable approval evidence, expose an `approval_id`, and support approved resume without duplicate tool side effects.

### R3 Regression Safety

The implementation must preserve existing AgentLoop event vocabulary, trace capture, run status persistence, and tenant/user boundaries.

## Critic Protocol

Reject completion if the patch rewrites the whole loop, leaves confirm as deny-only, lacks duplicate-execution tests, leaks raw tool arguments in approval events, or marks ARO-F002 passing without actor and critic evidence.
