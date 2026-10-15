# Phase 00 - Baseline Runtime and Capability Contract

> Agentic worker: open context-profile.json, loop-state.json, and this file only. Revalidate the target branch, establish the smallest safe capability seam and durable evidence, then stop.

- PHASE_ID: AS-00
- DEPENDS_ON: none
- UNLOCKS: AS-01
- FEATURE: AS-F001

**Goal:** Prove the target branch's real Assistant capability architecture and establish a tested fail-closed allowlist seam without changing user-visible Agent behavior.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {
    "id": "AS-00",
    "number": "00",
    "title": "Baseline Runtime and Capability Contract",
    "status": "ready",
    "type": "baseline",
    "repo_path": ".",
    "docs_path": "deploy/runbooks/agent-studio-prd",
    "phase_file": "deploy/runbooks/agent-studio-prd/phase-00-baseline-runtime-and-capability-contract.md",
    "depends_on": [],
    "unlocks": ["AS-01"]
  },
  "goal": {
    "target": "Revalidate the implementation branch, classify every reachable Assistant capability source, and prove a reusable allowlist filter can only reduce the runtime capability set while legacy built-in Assistant behavior remains unchanged.",
    "prompt": "Complete AS-00 by following deploy/runbooks/agent-studio-prd/phase-00-baseline-runtime-and-capability-contract.md; compare the approved target branch with origin/main without changing branches unless authorized, trace production composition roots and routes, establish the smallest tested capability allowlist seam, update code facts and reports, and stop after AS-F001 evidence and an independent critic verdict exist.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-report.md"
  },
  "runtime": {
    "context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json",
    "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json",
    "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json",
    "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json",
    "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md",
    "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md",
    "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md",
    "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md",
    "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true},
    "agent_roles": ["planner", "generator", "critic"]
  },
  "context": {
    "read_first": [
      "deploy/runbooks/agent-studio-prd/context-profile.json",
      "deploy/runbooks/agent-studio-prd/loop-state.json",
      "deploy/runbooks/agent-studio-prd/phase-00-baseline-runtime-and-capability-contract.md"
    ],
    "primary_context": [
      "apps/assistant-service/src/assistant_service/main.py",
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py and core/tool_invoker.py",
      "apps/assistant-service/src/assistant_service/core/tools and core/mcp",
      "src/main.py, src/api/v1/mcp.py, src/api/v1/skills.py, and packages/ai-gateway-core/src/ai_gateway_core/skills"
    ],
    "context_budget": "focused",
    "do_not_load_unless": [
      "source-packet.md Current System Facts when a code claim must be confirmed or corrected",
      "architecture-contract.md current architecture table when classification semantics are disputed",
      "origin/main file content only after the branch diff identifies a relevant changed path",
      "Docker or live runtime only after explicit approval and compose ownership verification",
      "continuity-ledger.md only when reading or writing a cross-phase capability boundary",
      "feature-oracle.json only for AS-F001 evidence writeback",
      "progress-log.md only when reading the latest blocker or appending the exit state"
    ]
  },
  "boundaries": {
    "likely_edit_paths": [
      "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py",
      "apps/assistant-service/src/assistant_service/core/tool_invoker.py",
      "tests/services/assistant/test_agent_capability_allowlist.py",
      "tests/integration/test_assistant_capability_wiring.py",
      "deploy/runbooks/agent-studio-prd"
    ],
    "do_not_edit": [
      "database schema or Agent CRUD",
      "web UI",
      "production MCP credentials or endpoints",
      "deployment and Docker configuration",
      "existing public Assistant API shapes"
    ],
    "external_inputs": ["user-approved implementation branch or explicit acceptance of the current checkout baseline"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["git status/log/diff read-only inspection", "rg and file inspection", "apply_patch", "targeted ruff and pytest"],
    "approval_required": ["git pull or branch switch", "Docker/live runtime", "migration", "deployment", "commit or push"],
    "dangerous_commands": ["git reset --hard", "git checkout --", "rm -rf", "docker prune", "production migration"]
  },
  "risk": {
    "tags": ["baseline", "agent", "security", "multi-tenant"],
    "data_mutation": false,
    "migration_required": false,
    "browser_required": false,
    "ai_eval_required": true,
    "external_service_required": false,
    "release_blocking": true
  },
  "validation": {
    "commands": [
      {"id": "branch-baseline", "cwd": ".", "command": "git status --short --branch && git log --oneline --decorate HEAD..origin/main --max-count=30 && git diff --name-only HEAD..origin/main", "expected": "The report records the exact branch relation and inspects every changed Assistant, tool, MCP, Skill, Knowledge, session, eval, or Web path before freezing facts.", "required": true},
      {"id": "capability-tests", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_internal_agent_capabilities.py tests/services/agent_runtime/test_control_plane.py", "expected": "Gateway capability reads and Agent Runtime control-plane resolution are covered by the current Gateway/Rust cutover tests; tenant-scoped artifacts and runtime capability bindings remain fail-closed.", "required": true},
      {"id": "assistant-isolation", "cwd": ".", "command": "make test-isolation", "expected": "Existing Assistant service isolation and OpenAPI contracts pass.", "required": true},
      {"id": "lint", "cwd": ".", "command": "uv run ruff check src/services/agent_runtime src/api/internal/agent_capabilities.py tests/services/agent_runtime tests/api/test_internal_agent_capabilities.py", "expected": "Ruff exits zero for every current Gateway/Rust control-plane Python path.", "required": true}
    ],
    "browser_checks": ["No UI is changed; the report must state that browser validation is not applicable to AS-00 and cite the unchanged route surface."],
    "regression_scope": ["built-in Assistant with no Agent allowlist", "knowledge search visibility", "model-native search controls", "Connector visibility", "ToolInvoker policy/approval behavior"],
    "compliance_gates": ["no secrets are read or logged", "tenant/policy uncertainty fails closed", "classification reflects reachable runtime wiring rather than comments", "branch changes are not applied without approval"],
    "acceptance_gates": [
      "A capability matrix names source type, registration point, management API, tenant filter, health state, and verified reachability for every production capability family.",
      "A test proves the explicit allowlist is applied before relevance selection and invocation, while an absent allowlist preserves current built-in Assistant behavior.",
      "MCP and Skills wiring gaps are reproduced or disproved with code/test evidence and assigned to AS-03/AS-04.",
      "Changed files are the minimal change needed for the allowlist seam and evidence.",
      "An independent critic approves requirement coverage, tests, regression impact, and minimal-change scope."
    ],
    "rollback_plan": ["Revert the allowlist seam and its tests if legacy Assistant regression fails; retain the read-only capability report and blocker evidence." ]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-report.md", "deploy/runbooks/agent-studio-prd/reports/as-00-critic-verdict.md"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "target-branch diff summary", "capability matrix", "test evidence", "AI/tool boundary evidence", "regression evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "Only the user may waive a required baseline or regression gate; the actor records the exact missing evidence and downstream risk.",
    "next_phase_handoff": "AS-01 unlocks only when the target branch, capability categories, allowlist seam, wiring defects, and exact validation commands are durable and critic-approved."
  },
  "stop_conditions": ["target branch choice remains unresolved", "origin/main cannot be inspected", "the allowlist requires a public API break", "a tenant policy path fails open and cannot be contained in this phase", "required tests need Docker or credentials without approval"]
}
```

## Requirements

### R1 Branch-Accurate Baseline

The report must identify the exact commit/branch relation and update every planning claim changed by the target branch.

### R2 Honest Capability Inventory

Every reachable capability family must be classified by execution source and wiring state; comments, stale routes, and configuration files are not proof of runtime reachability.

### R3 Non-Expanding Allowlist

The runtime must have a typed seam where `None` means legacy built-in behavior and an explicit set, including an empty set, is a hard upper bound before tool relevance selection and invocation.

### R4 Compatibility and Handoff

Existing Assistant isolation/API behavior must pass, and unresolved MCP/Skills/Connector defects must have an owner Phase and reproducible evidence.

## Critic Protocol

Reject if the actor assumes all tools are MCP, trusts comments over composition-root evidence, silently syncs branches, changes Agent schema/UI, weakens legacy behavior, applies the allowlist after selection, omits an empty-allowlist test, or lacks independent command evidence.
