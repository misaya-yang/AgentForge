# Phase 06 - Eval, Publish, Promotion, and Rollback

> Agentic worker: make saved Agent configuration releasable only through server-owned evaluation and immutable publication gates; do not add public/embed delivery yet.

- PHASE_ID: AS-06
- DEPENDS_ON: AS-05
- UNLOCKS: AS-07
- FEATURE: AS-F007

**Goal:** Bind Eval evidence to an exact Draft revision/spec, create immutable Versions idempotently, atomically promote Publications, and support audited safe rollback.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "hybrid",
  "phase": {"id": "AS-06", "number": "06", "title": "Eval Publish Promotion and Rollback", "status": "ready", "type": "eval", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-06-eval-publish-promotion-and-rollback.md", "depends_on": ["AS-05"], "unlocks": ["AS-07"]},
  "goal": {
    "target": "Extend the existing Eval and Trace stack so an exact Agent Draft revision can be validated, evaluated, diffed, materialized as an immutable Version, atomically promoted to a Publication, and safely rolled back with complete audit evidence.",
    "prompt": "Complete AS-06 by following deploy/runbooks/agent-studio-prd/phase-06-eval-publish-promotion-and-rollback.md after AS-05 passes; bind evaluation to exact Draft revision and runtime fingerprints, implement server-owned validation/diff/idempotent immutable Version creation/atomic promotion/rollback, add Studio publish and version experiences, run golden/eval/API/browser/migration/regression gates, and finish with actor, critic, rollback, and continuity evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-06-eval-publish-promotion-and-rollback.md"],
    "primary_context": ["src/api/v1/eval.py, src/api/schemas/eval.py, and packages/ai-gateway-core/src/ai_gateway_core/eval", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py", "AS-01 Agent repository/API and AS-02 runtime resolver contracts", "web/src/pages/eval and web/src/pages/agents from AS-05"],
    "context_budget": "focused",
    "do_not_load_unless": ["product-requirements.md sections 6.9 and 9 for publish behavior", "architecture-contract.md sections 9 and 10 for eval/migration boundaries", "AS-05 report/browser fixtures for UI extension", "live provider evaluations only after explicit cost/data approval", "source-packet.md only for Eval/Trace source lookup or code-fact writeback", "continuity-ledger.md only for publish/eval boundary lookup/writeback", "feature-oracle.json only for AS-F007 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["src/api/v1/agents.py", "src/api/v1/eval.py", "src/api/schemas/agents.py", "packages/ai-gateway-core/src/ai_gateway_core/eval", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py", "database/migrations/07*_agent_publication_eval*.sql", "web/src/pages/agents", "web/src/services/agents.ts", "web/e2e/agent-publish.spec.ts", "tests/api/test_agent_publish_api.py", "tests/services/eval/test_agent_version_candidate.py", "tests/services/eval/test_agent_publish_gate.py", "tests/database/test_agent_publication_atomicity.py", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["public Hosted route", "Embed Widget", "external Runtime API", "MCP/Skill/Knowledge execution semantics", "existing Eval baseline promotion semantics unrelated to Agent", "deployment configuration"],
    "external_inputs": ["AS-05 passed Draft/Preview UI contract", "approved production Eval datasets and blocking thresholds", "named role allowed to grant a non-security evaluation waiver"],
    "secrets_required": ["DASHSCOPE_CHAT_API_KEY for an approved live-model evaluation only"]
  },
  "tool_policy": {
    "allowed_tools": ["rg and eval/schema inspection", "apply_patch", "offline golden fixtures", "targeted ruff/pytest", "pnpm/Playwright browser validation"],
    "approval_required": ["paid/live model evaluation", "migration execution", "Docker/live runtime", "deployment", "commit or push"],
    "dangerous_commands": ["client-forced evaluation pass", "mutable Version update", "non-transactional Publication overwrite", "production migration", "git reset --hard"]
  },
  "risk": {"tags": ["agent", "ai", "llm", "eval", "database", "migration", "release", "ui", "browser", "security"], "data_mutation": true, "migration_required": true, "browser_required": true, "ai_eval_required": true, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "publish-api-atomicity", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agent_publish_api.py tests/database/test_agent_publication_atomicity.py tests/services/eval/test_agent_publish_gate.py", "expected": "Exact-revision validation, stale-eval rejection, idempotent Version creation, immutable rows, transaction rollback, atomic promotion, append-only audit and rollback tests pass.", "required": true},
      {"id": "agent-eval", "cwd": ".", "command": "uv run pytest -q --no-cov tests/services/eval/test_agent_version_candidate.py && make eval-regression-gate && make verify-eval-dev", "expected": "Agent Draft/Version candidates preserve runtime fingerprints; golden and existing Eval developer gates pass.", "required": true},
      {"id": "frontend", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-publish.spec.ts --config playwright.opensource.config.ts", "expected": "Static checks and Publish/Diff/Eval/Version/Rollback browser scenarios pass.", "required": true},
      {"id": "runtime-regression", "cwd": ".", "command": "make verify-assistant-runtime-dev && make test-isolation", "expected": "Published Version support does not regress built-in Assistant or runtime isolation.", "required": true}
    ],
    "browser_checks": ["At /agents/:id/evals on 1440x900 and 390x844 capture no-dataset, queued, running, passed, failed, cancelled and stale-revision states with keyboard/focus evidence.", "At the Publish Sheet on 1440x900 and 390x844 capture structured diff, resource checks, blocking/non-blocking findings, Eval links, disabled submit and successful promotion.", "At /agents/:id/versions capture version list, prompt/model/capability/Skill/KB/channel diff and rollback confirmation; prove existing versus new session messaging.", "Run axe and record console/network failures for Eval, Publish and Rollback paths."],
    "regression_scope": ["existing Eval datasets/experiments/baselines", "trace candidate ingestion", "Agent Draft save/Preview", "immutable Version repository", "capability/Knowledge health validation", "built-in Assistant", "Studio mobile/accessibility"],
    "compliance_gates": ["server computes all validation/eval status", "evaluation binds exact draft revision/spec/runtime fingerprints", "security/authorization failures cannot be waived", "promotion/rollback is atomic and audited", "published spec contains no secrets", "client retries are idempotent", "rollback rechecks current resource authorization", "paid/live evaluation requires approval and redacted datasets"],
    "acceptance_gates": ["Changing a Draft after Eval makes the prior result stale and unpublishable.", "Repeating the same idempotency key and spec returns the same Version/event; conflicting payload returns a stable conflict.", "Draft edits after publication do not alter the live Version or channel pointer.", "Rollback selects a historical healthy Version, preserves existing session pinning, creates an audit event, and leaves the prior version recoverable.", "Independent critic approves eval sufficiency, transaction/idempotency, UI truth, regression, rollback and minimal-change evidence."],
    "rollback_plan": ["Disable publish mutations while preserving Draft, Version, Eval and audit reads.", "Atomically repoint each affected Publication to the last known healthy Version; record the incident event.", "Rollback application code without deleting immutable Versions or publish events."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md", "deploy/runbooks/agent-studio-prd/reports/as-06-critic-verdict.md", "reports/agent-studio/as-06-eval-matrix.md", "reports/agent-studio/as-06-publish-atomicity.json", "reports/agent-studio/as-06-screenshots"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "Agent eval/golden table", "stale-eval and fingerprint evidence", "idempotency/transaction test evidence", "immutable Version/diff evidence", "publish/rollback browser screenshots", "audit and rollback evidence", "regression evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "A user may defer a non-blocking quality threshold with named owner/reason/expiry; tenant/auth/secret/safety gates, stale evaluation, immutable Version, atomicity, idempotency and rollback readiness cannot be waived.",
    "next_phase_handoff": "AS-07 receives stable Publication IDs/status/auth modes, version resolution, channel policy schema, session pinning, validation errors, promotion/rollback semantics and browser components."
  },
  "stop_conditions": ["AS-05 is not passed", "production Eval thresholds are required but unavailable and no offline release profile is approved", "evaluation results can be client-forged or detached from revision", "publication transaction cannot preserve the prior pointer", "live provider costs/data are required without approval"]
}
```

## Requirements

### R1 Revision-Bound Evaluation

Validation and Eval results must identify Draft revision, resolved spec hash and runtime fingerprints; any material change invalidates the gate.

### R2 Idempotent Immutable Publication

Only the server may create immutable Versions, and retries with the same key/spec must not create duplicate Versions or events.

### R3 Atomic Promotion and Rollback

Publication pointer changes and append-only events occur atomically; rollback revalidates current authorization/resources and preserves the previous recoverable state.

### R4 Truthful Studio Controls

Eval, diff, blocking findings, publish and rollback UI states reflect server evidence and remain responsive, accessible and non-bypassable.

## Critic Protocol

Reject if Eval is not revision/fingerprint-bound, clients can mark gates passed, publication mutates Draft/Version, idempotency is missing, pointer/audit updates can split, rollback ignores current revocation, existing sessions silently change, or browser/golden/transaction evidence is incomplete.
