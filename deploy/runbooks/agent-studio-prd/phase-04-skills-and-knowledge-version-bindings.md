# Phase 04 - Skills and Knowledge Version Bindings

> Agentic worker: make Skills and Knowledge safe, exact Agent capabilities through AS-02; do not build Studio screens or publication channels.

- PHASE_ID: AS-04
- DEPENDS_ON: AS-02
- UNLOCKS: AS-05
- FEATURE: AS-F005

**Goal:** Persist exact instruction-only Skill versions, isolate Skill catalogs, bind authorized Knowledge Datasets/configuration through normalized rows, and record traceable live-content provenance with fail-closed revocation.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-04", "number": "04", "title": "Skills and Knowledge Version Bindings", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-04-skills-and-knowledge-version-bindings.md", "depends_on": ["AS-02"], "unlocks": ["AS-05"]},
  "goal": {
    "target": "Fix Skill persistence/isolation, normalize user uploads to exact instruction-only db versions, add normalized Draft/Version Knowledge bindings, enforce save/publish/run authorization, and capture Skill hashes plus traceable live-content revisions/provenance without claiming replayability.",
    "prompt": "Complete AS-04 by following deploy/runbooks/agent-studio-prd/phase-04-skills-and-knowledge-version-bindings.md after AS-02 passes; repair Skill persistence and tenant cache boundaries, reject user-controlled executable/source/entrypoint schemes and normalize instruction-only db versions, create normalized Draft/Version Dataset bindings, enforce revocation at save/publish/run, record fingerprints/live-content provenance and explicit replay limits, and finish with isolation, RAG/eval, regression, rollback, critic, and continuity evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-04-skills-and-knowledge-version-bindings.md"],
    "primary_context": ["src/api/v1/skills.py and packages/ai-gateway-core/src/ai_gateway_core/skills", "apps/assistant-service/src/assistant_service/core/skills and core/agent/agent_loop.py", "apps/assistant-service/src/assistant_service/core/tools/builtin_tools.py and core/tool_invoker.py", "database/migrations/037_assistant_skills.sql and Knowledge Dataset API/repository contracts"],
    "context_budget": "focused",
    "do_not_load_unless": ["architecture-contract.md section 7 for binding/revocation invariants", "product-requirements.md sections 6.5 and 6.6 for observable behavior", "AS-02 report for capability adapter and trace vocabulary", "live Knowledge/provider runtime only after explicit approval", "source-packet.md only for Skill/Knowledge source lookup or code-fact writeback", "continuity-ledger.md only for Skill/Knowledge binding lookup/writeback", "feature-oracle.json only for AS-F005 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["src/api/v1/skills.py", "packages/ai-gateway-core/src/ai_gateway_core/skills", "apps/assistant-service/src/assistant_service/core/skills", "apps/assistant-service/src/assistant_service/core/agent/agent_loop.py", "apps/assistant-service/src/assistant_service/core/tool_invoker.py", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py", "database/migrations/07*_agent_skill_knowledge_bindings*.sql", "tests/api/test_skills_api.py", "tests/services/assistant/test_skill_version_binding.py", "tests/security/test_skill_tenant_isolation.py", "tests/security/test_skill_entrypoint_policy.py", "tests/services/assistant/test_agent_knowledge_binding.py", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["MCP protocol/security", "general Studio UI", "Knowledge indexing/retrieval algorithms", "Publication channels", "built-in Skill content unrelated to persistence"],
    "external_inputs": ["AS-02 capability/revision trace contract", "Knowledge Service Dataset permission and revision response contract", "confirmation whether production Knowledge supports historical revision reads; absence means provenance-only V1"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["rg and schema inspection", "apply_patch", "targeted ruff/pytest", "offline Skill and Knowledge fixtures"],
    "approval_required": ["live provider/Knowledge call", "Docker", "migration execution", "deployment", "commit or push"],
    "dangerous_commands": ["global unscoped registry mutation", "delete production Skill/Dataset", "production migration", "git reset --hard", "rm -rf"]
  },
  "risk": {"tags": ["agent", "ai", "eval", "database", "migration", "security", "multi-tenant", "knowledge"], "data_mutation": true, "migration_required": true, "browser_required": false, "ai_eval_required": true, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "skill-api-isolation", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py", "expected": "CRUD persists or fails honestly; list/get are tenant/user scoped; user entrypoint/source/builtin/path/network/exec forgery is rejected and valid uploads normalize to server-owned instruction-only db versions.", "required": true},
      {"id": "skill-runtime", "cwd": ".", "command": "uv run pytest -q --no-cov tests/services/agent_runtime/test_runtime_configuration.py", "expected": "Published Skill/runtime configuration bindings resolve immutably and fail closed across tenant, Agent and Version scope.", "required": true},
      {"id": "knowledge-binding", "cwd": ".", "command": "uv run pytest -q --no-cov tests/services/knowledge/test_capability_plane.py tests/api/test_internal_agent_capabilities.py", "expected": "Knowledge capability retrieval and Gateway-authorized Agent capability snapshots preserve tenant scope and fail closed.", "required": true},
      {"id": "lint", "cwd": ".", "command": "uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills src/services/agent_runtime/runtime_configuration.py tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/agent_runtime/test_runtime_configuration.py", "expected": "Ruff exits zero for every current Skill and Knowledge control-plane path.", "required": true}
    ],
    "browser_checks": ["No Studio UI is added; inspect API payloads to verify Skill content exposure follows role policy and Knowledge responses include stable IDs/status without leaking inaccessible Dataset metadata."],
    "regression_scope": ["existing Skill upload/list/test contract", "built-in skill_create", "Assistant skills feature flag", "knowledge search and citations", "Dataset revision hash", "capability selection", "Trace/eval ingestion"],
    "compliance_gates": ["Skill caches and queries include tenant/user/version", "database persistence errors return failure", "tenant uploads are instruction-only and server-normalized to db entrypoints", "user source/builtin/path/network/exec entrypoints are rejected", "bundled executable Skills are platform-managed hashed artifacts inaccessible to tenant upload", "full Skill content is untrusted below platform policy", "Skill permissions cannot authorize new tools", "normalized Draft/Version Dataset binding rows include tenant composite constraints", "Dataset ACL is checked at save, publish and run", "revocation invalidates caches and fails closed", "RAG provenance distinguishes retrieved, unavailable and no-binding states", "revision fingerprint is not presented as deterministic replay without historical revision reads"],
    "acceptance_gates": ["A valid instruction-only SKILL.md is normalized to a server-owned db version and round-trips exact content; user-supplied executable/source/entrypoint schemes are rejected.", "A newer Skill version cannot alter the old Agent Version and tenants with the same Skill name remain isolated.", "Draft and Version Knowledge bindings are normalized tenant-scoped rows with reverse references and stable Dataset IDs/config.", "Each run records current live-content revision/provenance and explicitly avoids a replayability claim when historical content cannot be fetched.", "Disabling/deleting/revoking a bound Skill or Dataset blocks new execution despite immutable references.", "Independent critic approves persistence honesty, entrypoint policy, isolation, exact version/provenance, regression and minimal-change evidence."],
    "rollback_plan": ["Disable Agent-bound Skills and Knowledge separately while preserving existing built-in Assistant request-level behavior.", "Keep additive binding/version rows and invalidate new caches; do not delete historical Skill content or trace fingerprints."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md", "deploy/runbooks/agent-studio-prd/reports/as-04-critic-verdict.md", "reports/agent-studio/as-04-skill-kb-golden.json"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "Skill persistence and isolation evidence", "exact-version runtime trace", "Knowledge ACL/provenance/revision evidence", "golden-question table", "revocation and failure matrix", "migration/rollback evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "Exact Skill version loading, tenant isolation, honest persistence errors, Knowledge authorization, revocation, provenance and capability boundaries cannot be waived for Studio work.",
    "next_phase_handoff": "AS-05 receives capability catalog APIs with source/risk/setup/health/version data, Agent Draft field schemas, validation errors, Preview API behavior, and stable degradation vocabulary."
  },
  "stop_conditions": ["AS-02 is not passed", "Skill persistence cannot load immutable content", "user-controlled executable entrypoints cannot be excluded", "global registry/cache remains cross-tenant", "Knowledge permission/revision contract is unavailable", "revocation fails open", "live external systems are required without approval"]
}
```

## Requirements

### R1 Honest Skill Persistence and Isolation

Skill CRUD must persist or return a failure, scope all catalog/cache access by tenant/user, and never expose another tenant's same-named Skill.

### R2 Exact Skill Version Execution

Agent Versions bind `skill_version_id` and execute full immutable instruction content/manifest; tenant upload entrypoints are server-normalized, executable/bundled classes are platform-only, updates do not drift old Versions, and Skill permissions cannot expand runtime capability policy.

### R3 Authorized Knowledge Binding

Drafts and Agent Versions use normalized tenant-scoped Dataset binding rows after authorization at save/publish/run; each run records current live-content revision/provenance without claiming historical replay.

### R4 Fail-Closed Revocation and Degradation

Disabled, deleted, revoked or unavailable resources invalidate execution and produce stable, honest states without silent model-knowledge substitution.

## Critic Protocol

Reject if API success hides DB failure, registries/caches are global, Runtime reconstructs Skills from metadata only, user uploads retain arbitrary source/entrypoint or executable schemes, Skill content can authorize tools, Dataset bindings live only inside JSON, Dataset ACL is checked only at save, revision/provenance is presented as replayability without historical reads, or immutable references override later security revocation.
