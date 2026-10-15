# Phase 02 - Trace Eval Feedback Loop

> Agentic worker: open `deploy/runbooks/assistant-runtime-optimization/context-profile.json`, `deploy/runbooks/assistant-runtime-optimization/loop-state.json`, and this file only. Execute just this phase, make the smallest requirement-satisfying change, write evidence, then stop at the gates.

- PHASE_ID: ARO-02
- DEPENDS_ON: ARO-01
- UNLOCKS: ARO-03
- FEATURE: ARO-F003

**Goal:** Turn existing assistant/langgraph/rag trace families into a reviewed trace-to-dataset-to-harness feedback loop.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ARO-02",
    "number": "02",
    "title": "Trace Eval Feedback Loop",
    "status": "ready",
    "type": "eval",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-runtime-optimization",
    "phase_file": "deploy/runbooks/assistant-runtime-optimization/phase-02-trace-eval-feedback-loop.md",
    "depends_on": ["ARO-01"],
    "unlocks": ["ARO-03"]
  },
  "goal": {
    "target": "Promote failed traces into redacted eval datasets and reviewed harness-profile proposals.",
    "prompt": "Complete ARO-02 Trace Eval Feedback Loop by following deploy/runbooks/assistant-runtime-optimization/phase-02-trace-eval-feedback-loop.md; work only on ARO-F003; use existing assistant, langgraph_proxy, and rag trace families; implement failure clustering, trace-to-dataset promotion, evaluator gates, and bounded UI/API evidence where needed; write actor and critic artifacts before unlocking ARO-03.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-runtime-optimization/reports/aro-02-trace-eval-feedback-loop-plan.md",
    "completion_report": "deploy/runbooks/assistant-runtime-optimization/reports/aro-02-trace-eval-feedback-loop-report.md"
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
      "deploy/runbooks/assistant-runtime-optimization/phase-02-trace-eval-feedback-loop.md"
    ],
    "primary_context": [
      "src/api/v1/eval.py",
      "src/services/eval/",
      "packages/ai-gateway-core/src/ai_gateway_core/eval/",
      "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "web/src/pages/eval only when UI failure-pattern panel is in scope",
      "database/migrations only when additive evaluator/dataset schema cannot reuse existing contracts",
      "source-packet.md only for targeted writeback",
      "continuity-ledger.md only for dependency boundary lookup or writeback",
      "feature-oracle.json only for ARO-F003 status update",
      "progress-log.md only for recent blocker or status history",
      "external SaaS docs only for comparison, never as required dependency"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "src/api/v1/eval.py",
      "src/api/schemas/eval.py",
      "src/services/eval/",
      "packages/ai-gateway-core/src/ai_gateway_core/eval/",
      "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py",
      "web/src/api/eval.ts",
      "web/src/pages/eval/",
      "tests/api/test_eval_traces.py",
      "tests/api/test_eval_api_trace_tree.py",
      "tests/services/eval/",
      "deploy/runbooks/assistant-runtime-optimization/"
    ],
    "do_not_edit": ["assistant tool execution internals", "provider credentials", "production retention settings without approval"],
    "external_inputs": ["production traces only if already available through local/dev DB and redacted in evidence"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["repo search", "apply_patch", "shell validation"],
    "approval_required": ["production trace access", "new external observability vendor", "schema migration", "deployment"],
    "dangerous_commands": ["DROP", "TRUNCATE", "rm -rf", "docker compose down"]
  },
  "risk": {
    "tags": ["ai", "agent", "eval", "frontend", "auth", "database"],
    "data_mutation": false,
    "migration_required": "unknown",
    "browser_required": true,
    "ai_eval_required": true,
    "external_service_required": false,
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-eval-loop",
        "cwd": ".",
        "command": "uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py src/services/eval packages/ai-gateway-core/src/ai_gateway_core/eval packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py tests/api/test_eval_traces.py tests/api/test_eval_api_trace_tree.py tests/services/eval",
        "expected": "Ruff reports no errors in eval feedback code and tests.",
        "required": true
      },
      {
        "id": "pytest-eval-feedback",
        "cwd": ".",
        "command": "uv run pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_eval_api_trace_tree.py tests/services/eval/test_evaluator_executor.py tests/services/eval/test_online_sampling.py tests/services/eval/test_golden_regression_gate.py tests/services/eval/test_trace_capture_helpers.py",
        "expected": "Trace API, evaluator executor, online sampling, golden gate, and trace helper tests pass.",
        "required": true
      },
      {
        "id": "web-eval-contract",
        "cwd": ".",
        "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web e2e:opensource",
        "expected": "Eval UI lint, type-check, and open-source e2e pass when web files change; if no web files changed, report records this command as not required.",
        "required": false
      }
    ],
    "browser_checks": ["If web/src/pages/eval changes, capture /eval desktop and mobile evidence or run web e2e:opensource."],
    "regression_scope": ["assistant trace family", "langgraph_proxy trace family", "rag trace family", "dataset/evaluator APIs", "tenant-scoped Eval permissions"],
    "compliance_gates": ["redact trace payloads before dataset promotion", "do not store raw secrets or full tool arguments", "non-operators remain user-scoped"],
    "acceptance_gates": [
      "failed trace can be clustered by failure mode",
      "selected trace can create a redacted eval dataset case",
      "evaluator/replay gate blocks a known bad candidate",
      "harness/profile proposal remains proposed until review/eval/rollback evidence exists",
      "critic verifies no SaaS dependency is required"
    ],
    "rollback_plan": ["disable analyzer entrypoint or feature flag", "leave created dataset cases disabled/proposed if gate fails"]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-02-trace-eval-feedback-loop-report.md",
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-02-trace-eval-feedback-loop-critic.md"
    ],
    "required_artifacts": ["phase report", "critic artifact", "dataset/evaluator evidence", "redaction evidence", "minimal-change note", "feature oracle evidence", "progress log entry", "continuity ledger update", "source packet update", "handoff update"],
    "waiver_policy": "Missing UI browser evidence can be waived only when no web files changed and API evidence covers the phase.",
    "next_phase_handoff": "Document trace-derived failure modes and checkpoint needs for ARO-03."
  },
  "stop_conditions": ["trace promotion needs production data without approval", "redaction cannot be proven", "schema expansion is required but migration approval is absent"]
}
```

## Requirements

### R1 Trace Failure Patterns

The runtime must classify low-score or failed traces into bounded failure modes such as tool_error, context_overflow, loop_detected, rag_miss, approval_blocked, and model_empty_output.

### R2 Dataset Promotion

A selected trace must become a redacted eval dataset case with tenant scope, source trace link, and replay/evaluator configuration.

### R3 Harness Proposal Gate

Any generated harness or runtime-profile change must remain proposed until critic review, eval evidence, and rollback metadata exist.

## Critic Protocol

Reject completion if the actor treats LangGraph/RAG trace capture as absent, stores raw trace payloads in datasets, requires LangSmith/Langfuse/Phoenix as a dependency, or lacks evaluator evidence for candidate harness changes.
