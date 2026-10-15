# Phase 04 - Performance Cost and Context Optimization

> Agentic worker: open `deploy/runbooks/assistant-runtime-optimization/context-profile.json`, `deploy/runbooks/assistant-runtime-optimization/loop-state.json`, and this file only. Execute just this phase, make the smallest requirement-satisfying change, write evidence, then stop at the gates.

- PHASE_ID: ARO-04
- DEPENDS_ON: ARO-03
- UNLOCKS: ARO-05
- FEATURE: ARO-F005

**Goal:** Make context, prompt-cache, reasoning, and tool-selection optimizations measurable, configurable, and eval-gated.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ARO-04",
    "number": "04",
    "title": "Performance Cost and Context Optimization",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/assistant-runtime-optimization",
    "phase_file": "deploy/runbooks/assistant-runtime-optimization/phase-04-performance-cost-and-context-optimization.md",
    "depends_on": ["ARO-03"],
    "unlocks": ["ARO-05"]
  },
  "goal": {
    "target": "Operationalize cache/context/reasoning/tool-selection metrics without quality regression.",
    "prompt": "Complete ARO-04 Performance Cost and Context Optimization by following deploy/runbooks/assistant-runtime-optimization/phase-04-performance-cost-and-context-optimization.md; work only on ARO-F005; add measurable cache/context/reasoning/tool-selection policies behind configuration and eval gates; update actor and critic evidence before ARO-05.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-plan.md",
    "completion_report": "deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-report.md"
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
      "deploy/runbooks/assistant-runtime-optimization/phase-04-performance-cost-and-context-optimization.md"
    ],
    "primary_context": [
      "apps/assistant-service/src/assistant_service/core/rag/context_engine.py",
      "apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py",
      "apps/assistant-service/src/assistant_service/core/models/model_registry.py",
      "apps/assistant-service/src/assistant_service/core/tools/tool_selector.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "web/src/pages/eval only if metrics display changes",
      "provider SDK docs only for cache metric names",
      "database/migrations only if existing trace metrics cannot hold new fields",
      "source-packet.md only for metric contract writeback",
      "continuity-ledger.md only for dependency boundary lookup or writeback",
      "feature-oracle.json only for ARO-F005 status update",
      "progress-log.md only for recent blocker or status history"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "apps/assistant-service/src/assistant_service/core/rag/context_engine.py",
      "apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py",
      "apps/assistant-service/src/assistant_service/core/models/model_registry.py",
      "apps/assistant-service/src/assistant_service/core/tools/tool_selector.py",
      "apps/assistant-service/src/assistant_service/core/trace_writer.py",
      "apps/assistant-service/src/assistant_service/core/trace_payloads.py",
      "tests/services/assistant/",
      "tests/services/eval/test_golden_regression_gate.py",
      "deploy/runbooks/assistant-runtime-optimization/"
    ],
    "do_not_edit": ["provider credentials", "pricing secrets", "production routing config", "unrelated UI pages"],
    "external_inputs": ["provider usage metadata from test fixtures only unless user supplies live credentials"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["repo search", "apply_patch", "shell validation"],
    "approval_required": ["live provider load test", "schema migration", "deployment"],
    "dangerous_commands": ["rm -rf", "docker compose down", "production migration"]
  },
  "risk": {
    "tags": ["ai", "agent", "eval", "external-service"],
    "data_mutation": false,
    "migration_required": "unknown",
    "browser_required": false,
    "ai_eval_required": true,
    "external_service_required": "unknown",
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-context-cost",
        "cwd": ".",
        "command": "uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py apps/assistant-service/src/assistant_service/core/models/model_registry.py apps/assistant-service/src/assistant_service/core/tools/tool_selector.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_model_registry.py tests/services/assistant/test_tool_selector.py tests/services/eval/test_golden_regression_gate.py",
        "expected": "Ruff reports no errors in ARO-04 changed context/cache/model/tool paths and focused tests.",
        "required": true
      },
      {
        "id": "assistant-context-tests",
        "cwd": ".",
        "command": "uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/tools/test_context_tools.py",
        "expected": "Assistant golden, trace, and context-tool tests pass.",
        "required": true
      },
      {
        "id": "eval-quality-gate",
        "cwd": ".",
        "command": "uv run pytest -q --no-cov tests/services/eval/test_golden_regression_gate.py tests/services/eval/test_evaluator_executor.py",
        "expected": "Golden regression and evaluator executor tests pass for optimization changes.",
        "required": true
      }
    ],
    "browser_checks": [],
    "regression_scope": ["prompt-prefix stability", "cache metrics extraction", "context budget telemetry", "tool selection quality", "golden eval quality"],
    "compliance_gates": ["no raw prompt or secret in cache metrics", "routing changes are config-gated", "provider metadata parsing is bounded"],
    "acceptance_gates": [
      "prompt-prefix hash and cache token metrics are available in trace or eval evidence",
      "tool schema ordering is deterministic",
      "adaptive reasoning/model routing is disabled by config or backed by eval evidence",
      "golden eval does not regress against baseline",
      "critic verifies cost improvements are not claimed without measured evidence"
    ],
    "rollback_plan": ["disable adaptive routing config", "fall back to existing tool selector", "remove cache metric enrichment if provider parsing fails"]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-report.md",
      "deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-critic.md"
    ],
    "required_artifacts": ["phase report", "critic artifact", "cache/context metric evidence", "golden eval output", "minimal-change note", "feature oracle evidence", "progress log entry", "continuity ledger update", "source packet update", "handoff update"],
    "waiver_policy": "Live provider cache-hit evidence may be skipped only if fixture-based parsing and trace output are proven.",
    "next_phase_handoff": "Document SLI names, thresholds, and no-go conditions for ARO-05 release regression."
  },
  "stop_conditions": ["provider cache metrics require credentials not available", "optimization lowers golden quality", "routing cannot be disabled"]
}
```

## Requirements

### R1 Measured Cache and Context

Trace/eval evidence must include stable prompt-prefix identity, provider cache token metrics when available, TTFT/context utilization where locally observable, and bounded payloads.

### R2 Config-Gated Optimization

Adaptive reasoning, model routing, or embedding-based tool selection must be controlled by configuration and must not replace the existing safe path without eval proof.

### R3 Quality Protection

Any latency or cost improvement claim must include quality regression evidence from golden/evaluator tests.

## Critic Protocol

Reject completion if the actor claims cost savings without measured telemetry, hardcodes provider behavior, removes the existing selector fallback, stores raw prompt content in metrics, or skips golden eval evidence.
