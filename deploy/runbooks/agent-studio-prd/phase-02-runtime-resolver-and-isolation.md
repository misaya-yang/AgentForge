# Phase 02 - Runtime Resolver and Isolation

> Agentic worker: connect immutable Agent configuration to the existing Assistant Runtime only after AS-01 contracts pass; do not add MCP/Skill/Studio channel features here.

- PHASE_ID: AS-02
- DEPENDS_ON: AS-01
- UNLOCKS: AS-03, AS-04
- FEATURE: AS-F003

**Goal:** Resolve server-authored Agent Snapshots, enforce layered prompts and non-expanding capabilities, pin sessions to Versions, and make runs/traces explicitly queryable by Agent and channel.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-02", "number": "02", "title": "Runtime Resolver and Isolation", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md", "depends_on": ["AS-01"], "unlocks": ["AS-03", "AS-04"]},
  "goal": {
    "target": "Make Gateway the sole external Agent/Version resolver, sign a canonical-Snapshot/body/session-bound Runtime Envelope, make Assistant recalculate hashes, consume nonces and enforce it, and isolate prompts, capabilities, memory, sessions, idempotency, checkpoints, runs, and traces.",
    "prompt": "Complete AS-02 by following deploy/runbooks/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md after AS-01 passes; add distinct Preview/Published external schemas at Gateway, resolve the authorized Draft/Version there, embed the canonical Snapshot in an identity/session/body/spec/time/nonce-bound signed Runtime Envelope, make Assistant recalculate hashes and atomically consume nonce, reject forged Agent fields on the generic Assistant route, enforce layered prompts and capability intersection, pin sessions/observability, preserve built-in Assistant behavior, and finish with runtime, isolation, forgery/replay, eval, trace, critic, and rollback evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-02-runtime-resolver-and-isolation.md"],
    "primary_context": ["src/api/v1/assistant.py, src/api/v1/_assistant_proxy.py, and new Agent runtime Gateway routes/schemas", "apps/assistant-service/src/assistant_service/api/routes/chat.py, core/gateway, and core/assistant_service.py", "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py, core/tool_invoker.py, and core/prompts", "apps/assistant-service/src/assistant_service/core/trace_writer.py and packages/ai-gateway-core persistence repositories"],
    "context_budget": "focused",
    "do_not_load_unless": ["architecture-contract.md sections 3 and 4 for snapshot/prompt invariants", "AS-01 report for exact repository/schema names", "source-packet.md Trace/Eval section for a disputed reuse boundary or code-fact writeback", "live model/provider only after explicit approval", "continuity-ledger.md only for resolver/session/trace boundary lookup/writeback", "feature-oracle.json only for AS-F003 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["packages/ai-gateway-core/src/ai_gateway_core/agents", "src/api/v1/assistant.py", "src/api/v1/_assistant_proxy.py", "src/api/v1/agent_runtime.py", "src/api/schemas/agent_runtime.py", "apps/assistant-service/src/assistant_service/core/agent", "apps/assistant-service/src/assistant_service/core/assistant_service.py", "apps/assistant-service/src/assistant_service/core/tool_invoker.py", "apps/assistant-service/src/assistant_service/core/trace_writer.py", "apps/assistant-service/src/assistant_service/api/routes/chat.py", "database/migrations/07*_agent_runtime_dimensions*.sql", "tests/api/test_agent_runtime_envelope.py", "tests/services/assistant/test_agent_runtime_resolver.py", "tests/services/assistant/test_agent_runtime_isolation.py", "tests/services/assistant/test_agent_trace_capture.py", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["MCP server registry/auth", "Skill persistence", "Knowledge service retrieval implementation", "web Studio/Hosted/Embed UI", "Publication promotion APIs", "existing client-trusted arbitrary runtime override"],
    "external_inputs": ["AS-01 passed report and exact AgentSpec/repository contracts"],
    "secrets_required": ["GATEWAY_ASSISTANT_SHARED_SECRET for approved live internal smoke only"]
  },
  "tool_policy": {
    "allowed_tools": ["rg and code inspection", "apply_patch", "targeted ruff/pytest", "offline golden and trace fixtures"],
    "approval_required": ["live provider call", "Docker/live runtime", "database migration execution", "deployment", "commit or push"],
    "dangerous_commands": ["client-provided snapshot trust", "bypass tenant policy", "production migration", "git reset --hard", "rm -rf"]
  },
  "risk": {"tags": ["agent", "ai", "llm", "eval", "auth", "security", "database", "migration", "multi-tenant"], "data_mutation": true, "migration_required": true, "browser_required": false, "ai_eval_required": true, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "gateway-envelope", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py", "expected": "Preview/Published schemas reject client model, Prompt, capability, snapshot and X-Agent forgery; signatures bind tenant/agent/version/publication/channel/session/body/canonical Snapshot/spec/time/nonce, Assistant recalculates hashes and atomically consumes nonce, and tests reject snapshot mutation, expiry, replay and body/session substitution.", "required": true},
      {"id": "connector-binding", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agent_connector_capability.py", "expected": "Catalog-model connector bindings (provider + tool_name, no grant) resolve only when the provider has an enabled connector_configs row visible to the tenant and the calling user holds a connected user_connectors row; disabled, ingest-only, unauthenticated, public-channel and disconnected cases are stripped, while grant-based bindings keep the credential-principal authorization path.", "required": true},
      {"id": "resolver-isolation", "cwd": ".", "command": "uv run pytest -q --no-cov tests/services/agent_runtime/test_control_plane.py tests/services/agent_runtime/test_runtime_configuration.py tests/services/agent_runtime/test_capability_leases.py", "expected": "Gateway resolves Agent Runtime control-plane state, runtime configuration, and capability leases with fail-closed tenant/session isolation.", "required": true},
      {"id": "trace-session", "cwd": ".", "command": "make agent-runtime-eval-contract-gate", "expected": "The current Agent Runtime evaluation contract validates native thread/run/session ownership and durable runtime evidence.", "required": true},
      {"id": "runtime-gate", "cwd": ".", "command": "make verify-assistant-runtime-dev && make test-isolation", "expected": "Assistant runtime regression and service isolation gates pass.", "required": true},
      {"id": "lint", "cwd": ".", "command": "uv run ruff check packages/ai-gateway-core/src/ai_gateway_core/agents src/api/v1/assistant.py src/api/v1/agent_runtime.py src/api/schemas/agent_runtime.py src/services/agent_runtime tests/api/test_agent_runtime_envelope.py tests/services/agent_runtime", "expected": "Ruff exits zero for touched Gateway and Agent Runtime control-plane paths.", "required": true}
    ],
    "browser_checks": ["No browser UI is introduced; use API/SSE integration fixtures to verify public event compatibility and absence of internal snapshot, prompt, policy, or secret fields."],
    "regression_scope": ["built-in Assistant chat and session history", "streaming SSE event schema", "model-native search", "KB injection", "tool approval", "memory modes", "idempotency/checkpoint resume", "trace/eval ingestion"],
    "compliance_gates": ["Gateway is the sole external Agent/Version resolver", "external schemas cannot carry trusted model, Prompt, capability or snapshot overrides", "Envelope carries the canonical Snapshot and its signature binds tenant/agent/version/publication/channel/session/body/spec/time/nonce", "Assistant recalculates body/Snapshot hashes, atomically consumes nonce, rejects replay and repeats fail-closed resource checks", "tenant and ACL checks happen before data load", "deny overrides allow", "unknown readiness fails closed", "anonymous/user memory policy remains distinguishable", "external text stays in the lowest prompt trust layer", "traces redact secrets and protected prompt content"],
    "acceptance_gates": ["Two Agents in the same tenant and two tenants cannot share prompts, tools, memory, sessions, idempotency results or traces.", "Existing sessions remain pinned after Publication version change; revoked Versions fail with a stable error rather than silently upgrading.", "Tool selection and invocation cannot exceed the resolved allowlist even when the user/model names an unbound tool.", "Golden questions cover normal response, no-tool Agent, KB/tool trace, prompt injection, permission denial and resource unavailable behavior.", "Independent critic approves isolation, eval/trace evidence, compatibility and minimal-change scope."],
    "rollback_plan": ["Disable Agent runtime resolution behind the feature flag and route built-in Assistant through the legacy None-allowlist path.", "Keep additive trace/session columns nullable and preserve captured evidence during application rollback."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-02-runtime-resolver-and-isolation-report.md", "deploy/runbooks/agent-studio-prd/reports/as-02-critic-verdict.md", "reports/agent-studio/as-02-golden-results.json"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "snapshot schema and hash evidence", "runtime/isolation test evidence", "golden-question table", "trace and tool/source boundary evidence", "prompt-injection/privacy evidence", "migration and rollback evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "Tenant isolation, server-authored snapshots, non-expanding capabilities, session version pinning, secret redaction, and built-in Assistant compatibility cannot be waived for AS-02.",
    "next_phase_handoff": "AS-03 and AS-04 receive the exact Capability binding interface, signed Envelope/resolved snapshot schemas, ToolInvoker adapter contract, policy-reason trace format, session pinning and internal authentication boundary."
  },
  "stop_conditions": ["AS-01 is not passed", "AgentSpec/repository contracts are unstable", "the design requires trusting a client-supplied snapshot", "tenant or capability policy fails open", "existing Assistant compatibility cannot be preserved behind a feature flag", "live credentials or migration are required without approval"]
}
```

## Requirements

### R1 Authorized Deterministic Resolver

Only Gateway-side tenant/ACL-authorized Draft revisions or immutable Versions may become a normalized, hashed Runtime Snapshot and signed, request-bound Envelope; generic Assistant requests cannot inject Agent configuration.

### R2 Layered Prompt and Capability Boundary

Prompt trust layers and the full policy intersection must be assembled before model/tool execution; downstream selectors may only reduce capabilities.

### R3 Session and Evidence Pinning

Sessions, idempotency, checkpoints, runs and traces must bind to Agent, Version, Publication/channel and runtime fingerprints with no silent version switch.

### R4 Legacy Compatibility

When Agent Studio is disabled or a built-in Assistant request has no Agent context, the existing Assistant behavior and external SSE/API shape remain unchanged.

## Critic Protocol

Reject if clients can submit resolved capabilities, any unknown policy is fail-open, selectors/invokers disagree, prompt layers allow external data to override platform/Agent instructions, sessions hot-swap versions, key dimensions live only in free-form metadata, or existing Assistant regression evidence is absent.
