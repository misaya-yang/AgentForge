# Phase 02 - Assistant Trace Capture

> Enter plan-first mode before editing. Execute this phase only, work on ATE-F003 only, and depend on the ATE-01 storage and API contract.

**Goal:** Persist AI Assistant run, span, event, and error trace evidence from chat and stream flows without adding user-visible agent latency.

**Architecture:** This phase connects assistant-service execution to the ATE-01 trace contract. It must capture a root trace for each AI Assistant run, ordered spans for major execution steps, durable stream events, terminal status, errors, latency, and redaction markers. Trace writes must run outside the chat response critical path through a bounded background writer, outbox, or equivalent non-blocking handoff. It must preserve the existing streaming-first behavior and avoid external observability dependencies.

**Tech Stack:** assistant-service FastAPI routes, assistant service core, AgentLoop, ExecutionGateway, ai-gateway-core persistence contract from ATE-01, pytest service tests, integration isolation tests, and ruff.

---

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "ATE-02",
    "number": "02",
    "title": "Assistant Trace Capture",
    "status": "ready",
    "type": "implementation",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-trace-eval-prd",
    "phase_file": "deploy/runbooks/agent-trace-eval-prd/phase-02-assistant-trace-capture.md",
    "depends_on": [
      "ATE-01"
    ],
    "unlocks": [
      "ATE-03"
    ]
  },
  "goal": {
    "target": "Persist AI Assistant run, span, event, and error trace evidence from chat and stream flows without adding user-visible agent latency.",
    "prompt": "Complete ATE-02 Assistant Trace Capture for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-02-assistant-trace-capture.md`; work on feature-oracle item ATE-F003; depend on ATE-01 storage and API evidence; implement only AI Assistant trace capture for chat and stream flows; preserve streaming-first behavior; keep trace persistence outside the chat response critical path; do not build the Eval frontend; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-plan.md",
    "completion_report": "deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-report.md"
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
      "deploy/runbooks/agent-trace-eval-prd/phase-02-assistant-trace-capture.md"
    ],
    "primary_context": [
      "apps/assistant-service/src/assistant_service/api/routes/chat.py",
      "apps/assistant-service/src/assistant_service/core/assistant_service.py",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "deploy/runbooks/agent-trace-eval-prd/README.md only when harness intent is unclear",
      "deploy/runbooks/agent-trace-eval-prd/phase-manifest.md only when dependency order is unclear",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md only for ATE-01 trace contract lookup or writeback",
      "deploy/runbooks/agent-trace-eval-prd/loop-contract.json only when loop semantics are unclear",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json only for ATE-F003 evidence update",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md only for recent blocker or status history",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md only when next action is unclear",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md only for ATE-01 dependency lookup or ATE-02 writeback",
      "deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md only when preparing a fresh context window"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "apps/assistant-service/src/assistant_service/core/trace_writer.py",
      "apps/assistant-service/src/assistant_service/core/assistant_service.py",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py",
      "apps/assistant-service/src/assistant_service/api/routes/chat.py",
      "tests/services/assistant/test_agent_trace_capture.py",
      "tests/services/assistant/test_agentloop_streaming_first_contract.py",
      "tests/integration/test_assistant_isolation_contract.py",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-report.md",
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-critic.md",
      "deploy/runbooks/agent-trace-eval-prd/source-packet.md",
      "deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md",
      "deploy/runbooks/agent-trace-eval-prd/progress-log.md",
      "deploy/runbooks/agent-trace-eval-prd/agent-handoff.md",
      "deploy/runbooks/agent-trace-eval-prd/feature-oracle.json",
      "deploy/runbooks/agent-trace-eval-prd/loop-state.json"
    ],
    "do_not_edit": [
      "web/",
      "src/api/v1/eval.py except ATE-01 contract bugfix with report evidence",
      "database/migrations/060_agent_trace_eval.sql except ATE-01 contract bugfix with report evidence",
      "src/api/v1/langgraph.py",
      "production systems",
      "secret files",
      "deployment configuration"
    ],
    "external_inputs": [
      "ATE-01 actor report and critic artifact"
    ],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": [
      "repo search",
      "shell validation",
      "file patch",
      "pytest",
      "ruff"
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
      "database",
      "ai",
      "agent",
      "eval",
      "security"
    ],
    "data_mutation": "local development trace row writes only",
    "migration_required": "false unless ATE-01 report authorizes a compatibility fix",
    "browser_required": "false",
    "ai_eval_required": "trace capture evidence only",
    "external_service_required": "false",
    "release_blocking": "true"
  },
  "validation": {
    "commands": [
      {
        "id": "ruff-assistant-trace",
        "cwd": ".",
        "command": "uv run ruff check apps/assistant-service/src/assistant_service/core/trace_writer.py apps/assistant-service/src/assistant_service/core/assistant_service.py apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py apps/assistant-service/src/assistant_service/api/routes/chat.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py",
        "expected": "Ruff exits 0 for assistant trace capture code and touched tests.",
        "required": true
      },
      {
        "id": "assistant-trace-tests",
        "cwd": ".",
        "command": "uv run --extra dev --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/integration/test_assistant_isolation_contract.py",
        "expected": "Pytest exits 0 and proves chat capture, stream capture, terminal status, error capture, streaming-first behavior, tenant isolation, and latency guard behavior.",
        "required": true
      },
      {
        "id": "assistant-latency-guard-tests",
        "cwd": ".",
        "command": "uv run --extra dev --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py -k 'latency or non_blocking or does_not_wait'",
        "expected": "Pytest exits 0 and uses a slow or blocked trace writer fake to prove first stream event, final non-stream response, and run status update do not wait for trace persistence.",
        "required": true
      },
      {
        "id": "trace-token-scan",
        "cwd": ".",
        "command": "rg -n 'trace_writer|run_started|run_finished|run_error|span|redact' apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py",
        "expected": "Command output shows trace writer wiring, terminal event names, span handling, and redaction handling in assistant-service code.",
        "required": true
      },
      {
        "id": "harness-strict",
        "cwd": ".",
        "command": "python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score",
        "expected": "Harness strict validation exits 0 after ATE-02 evidence writeback.",
        "required": true
      }
    ],
    "browser_checks": [
      "No browser route is created in ATE-02; any browser-visible edit blocks completion until route and viewport evidence are recorded."
    ],
    "regression_scope": [
      "Streaming first-token contract remains passing.",
      "Non-stream chat response shape remains compatible.",
      "Assistant isolation contract remains passing.",
      "ATE-01 Eval API contract remains compatible with persisted rows."
    ],
    "compliance_gates": [
      "Trace capture writes only redacted or bounded payload summaries.",
      "Trace persistence is best-effort and uses a bounded non-blocking handoff with a deterministic drop or retry policy.",
      "Trace writer uses authenticated tenant and user context from existing request handling.",
      "Trace errors do not include provider secrets, tokens, connection strings, or raw credentials.",
      "Trace writes are failure-tolerant and must not fail the user-facing chat response when persistence is unavailable."
    ],
    "acceptance_gates": [
      "Streaming and non-stream assistant requests create one root trace per run.",
      "run_started, run_finished, and run_error events are persisted with monotonic ordering.",
      "Major execution steps are persisted as spans with start time, end time, status, and bounded metadata.",
      "Errors are redacted before persistence and linked to the trace root.",
      "Trace writer handles duplicate terminal writes idempotently or records deterministic conflict behavior.",
      "A slow or blocked trace writer fake proves trace persistence is not awaited before first stream event, final non-stream response, or assistant run status update.",
      "Actor report records validation evidence, streaming-first regression evidence, latency guard evidence, minimal-change scope, and security proof.",
      "A separate independent critic artifact contains Critic Verdict and checks capture coverage, streaming regression, latency guard behavior, redaction, failure tolerance, and minimal-change scope."
    ],
    "rollback_plan": [
      "Disable assistant trace writer injection while leaving ATE-01 schema and API intact.",
      "Revert assistant-service code and tests touched by ATE-02.",
      "Leave local development trace rows in place unless a reviewed local cleanup script is included in the actor report.",
      "Do not modify production data during rollback."
    ]
  },
  "evidence": {
    "outputs": [
      "deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-report.md"
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
    "next_phase_handoff": "ATE-03 is unlocked only when AI Assistant trace rows can be listed and inspected through the ATE-01 API after chat and stream flows, with evidence that trace persistence does not delay agent responses."
  },
  "stop_conditions": [
    "ATE-01 completion evidence is missing or rejected",
    "Trace writes break streaming-first behavior",
    "Trace writes add user-visible latency or are awaited by the chat response path",
    "Trace persistence failure can fail the user-facing chat response",
    "Redaction cannot be applied before persistence",
    "Assistant-service capture requires frontend changes"
  ]
}
```

## Coding Agent Contract

- PHASE_ID: ATE-02
- GOAL_TARGET: Persist AI Assistant run, span, event, and error trace evidence from chat and stream flows without adding user-visible agent latency.
- GOAL_PROMPT: Complete ATE-02 Assistant Trace Capture for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-02-assistant-trace-capture.md`; work on feature-oracle item ATE-F003; depend on ATE-01 storage and API evidence; implement only AI Assistant trace capture for chat and stream flows; preserve streaming-first behavior; keep trace persistence outside the chat response critical path; do not build the Eval frontend; stay inside named edit boundaries; make the smallest requirement-satisfying change; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
- DEPENDS_ON: ATE-01
- READ_FIRST: `deploy/runbooks/agent-trace-eval-prd/context-profile.json`, `deploy/runbooks/agent-trace-eval-prd/loop-state.json`, this file
- PRIMARY_CONTEXT: `apps/assistant-service/src/assistant_service/api/routes/chat.py`, `apps/assistant-service/src/assistant_service/core/assistant_service.py`, `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`, `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`
- LIKELY_EDIT_PATHS: `apps/assistant-service/src/assistant_service/core/trace_writer.py`, `apps/assistant-service/src/assistant_service/core/assistant_service.py`, `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`, `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`, `apps/assistant-service/src/assistant_service/api/routes/chat.py`, `tests/services/assistant/test_agent_trace_capture.py`, `tests/services/assistant/test_agentloop_streaming_first_contract.py`, `tests/integration/test_assistant_isolation_contract.py`, harness report and runtime writeback files for ATE-02
- DO_NOT_EDIT: `web/`, `src/api/v1/langgraph.py`, production systems, secret files, deployment configuration
- EXECUTION_MODE: plan-first; implement one phase and one feature item; verify before completion; write evidence before handoff
- VALIDATION_COMMANDS: `uv run ruff check apps/assistant-service/src/assistant_service/core/trace_writer.py apps/assistant-service/src/assistant_service/core/assistant_service.py apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py apps/assistant-service/src/assistant_service/api/routes/chat.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py`; `uv run --extra dev --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/integration/test_assistant_isolation_contract.py`; `uv run --extra dev --extra test pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py -k 'latency or non_blocking or does_not_wait'`; `rg -n 'trace_writer|run_started|run_finished|run_error|span|redact' apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py`; `python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score`
- BROWSER_CHECKS: No browser route is created in ATE-02; any browser-visible edit blocks completion until route and viewport evidence are recorded.
- REGRESSION_SCOPE: Streaming first-token contract, non-stream chat response shape, assistant isolation, latency guard behavior, and ATE-01 Eval API compatibility remain passing.
- COMPLIANCE_GATES: Redacted and bounded payload summaries only; tenant and user context comes from existing auth; persistence failure cannot fail or delay chat; no production data mutation.
- ROLLBACK_PLAN: Disable trace writer injection, revert ATE-02 assistant-service changes, and keep ATE-01 schema/API intact.
- ACCEPTANCE_GATES: Chat and stream requests create trace roots, spans, events, terminal status, redacted errors, latency guard tests, source-packet writeback, continuity ledger update, minimal-change scope, and independent critic verdict.
- EVIDENCE_OUTPUT: `deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-report.md`
- STOP_CONDITIONS: Stop if ATE-01 is not passed, streaming-first behavior breaks, trace persistence can fail chat, redaction cannot run before persistence, or frontend changes are required.

## Task Spec

Implement runtime capture for AI Assistant traces. Each assistant run must produce a trace root and ordered execution evidence that the ATE-03 Eval console can read through the ATE-01 API. Trace persistence must use a non-blocking handoff so the agent response path never waits for database writes.

## Problem Boundary

In scope: assistant-service trace writer, AgentLoop instrumentation, chat route wiring, execution gateway updates, service tests, integration regression, and harness evidence.

Out of scope: frontend UI, LangGraph Proxy trace capture, RAG retrieval trace capture, production migration execution, provider observability exports.

## Context Policy

Load only the four `PRIMARY_CONTEXT` paths after `READ_FIRST`. Open ATE-01 reports and ledger rows only to confirm storage and API contracts. Defer source packet, continuity ledger, feature oracle, and progress log until targeted writeback.

## Requirements

- Create one trace root per assistant run for stream and non-stream paths.
- Persist ordered events for run start, terminal success, terminal error, and cancellation when the runtime exposes cancellation.
- Persist spans for major execution steps: context loading, model invocation, tool execution, memory update, and response finalization when those steps exist in the current code path.
- Use redaction before persistence for prompts, tool inputs, tool outputs, and errors.
- Treat trace persistence as non-blocking for user-facing chat success.
- Use a bounded queue, background task, outbox writer, or equivalent non-blocking handoff with short timeouts and deterministic drop or retry behavior.

## Test and Regression Requirements

Run the validation commands from the Machine Contract. Tests must prove stream capture, non-stream capture, terminal error capture, duplicate terminal behavior, persistence failure tolerance, non-blocking latency behavior, and streaming-first regression.

## Compliance and Safety Requirements

Do not persist secrets, tokens, provider keys, connection strings, or unbounded raw payloads. Do not write production data. Do not send trace data to external vendors.

## Rollback and Recovery

Rollback removes assistant-service trace writer wiring and leaves ATE-01 schema/API available for later retry. Local development trace rows may remain unless the actor report includes a reviewed cleanup command for local data only.

## Execution Capture

Write `reports/ate-02-assistant-trace-capture-report.md` with Status, Validation Evidence, Feature Oracle Updates, Minimal Change, Regression Scope, Compliance Evidence, Rollback Evidence, and Next Phase Handoff. Update source packet, continuity ledger, progress log, feature oracle, handoff, and loop state.

## Critic Protocol

Use `reports/ate-02-assistant-trace-capture-critic.md`. The critic must state `Critic Verdict`, name the actor report reviewed, and verify trace coverage, streaming-first behavior, non-blocking latency behavior, redaction, persistence failure tolerance, tests, and minimal-change scope.

## Acceptance Criteria

- AI Assistant chat and stream flows create trace data visible through the ATE-01 API.
- Streaming-first contract remains passing.
- Slow or failing trace persistence does not delay first stream event, final non-stream response, or assistant run status updates.
- Redacted error and payload handling is covered by tests.
- ATE-03 receives stable API data for the Eval console.

## Risks

- Trace writes can accidentally add latency or break streaming-first behavior.
- Error persistence can leak sensitive provider details if redaction is bypassed.
- Instrumentation can duplicate terminal events when stream generators exit through multiple paths.
