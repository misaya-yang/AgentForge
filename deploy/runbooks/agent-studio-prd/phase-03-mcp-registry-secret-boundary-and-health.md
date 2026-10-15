# Phase 03 - MCP and Connector Registry, Credential Principals, Secret Boundary, and Health

> Agentic worker: add MCP as one capability provider through the AS-02 resolver; do not convert native tools or build the general Studio UI.

- PHASE_ID: AS-03
- DEPENDS_ON: AS-02
- UNLOCKS: AS-05
- FEATURE: AS-F004

**Goal:** Deliver tenant-scoped remote MCP registration and existing Connector integration with explicit credential principals, safe authentication, discovery/versioning, health and runtime invocation through the existing capability policy.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-03", "number": "03", "title": "MCP and Connector Registry Secret Boundary and Health", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-03-mcp-registry-secret-boundary-and-health.md", "depends_on": ["AS-02"], "unlocks": ["AS-05"]},
  "goal": {
    "target": "Implement tenant-scoped MCP and existing Connector connections with explicit service-account versus user-delegated principals, Streamable HTTP/OAuth/SSRF controls, versioned discovery, health/circuit controls, Agent/channel binding, and fail-closed invocation.",
    "prompt": "Complete AS-03 by following deploy/runbooks/agent-studio-prd/phase-03-mcp-registry-secret-boundary-and-health.md after AS-02 passes; use mock MCP/OAuth services by default, separate Server definitions from service-account/user-delegated credential connections, prevent cross-user or anonymous fallback, unify existing Connector credential/runtime authorization, implement tenant-safe persistence, SSRF/Origin/audience controls, immutable schema snapshots and AS-02 runtime adapters, then produce protocol, credential-principal, security, regression, rollback, critic, and continuity evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-03-mcp-registry-secret-boundary-and-health-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-03-mcp-registry-secret-boundary-and-health-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-03-mcp-registry-secret-boundary-and-health.md"],
    "primary_context": ["apps/assistant-service/src/assistant_service/core/mcp, core/tools/connector_registry.py, and core/tools/confluence_tool.py", "apps/assistant-service/src/assistant_service/core/tool_invoker.py and main.py", "src/api/v1/mcp.py, src/api/v1/tenant_policies.py, Connector APIs, and src/main.py", "database/migrations/044_tenant_soft_isolation.sql and AS-01 Agent capability repositories"],
    "context_budget": "focused",
    "do_not_load_unless": ["architecture-contract.md sections 5 and 6 for API/MCP invariants", "MCP official Transport/Authorization/Security pages for a protocol dispute", "AS-02 report for adapter and policy-reason contracts", "real OAuth/MCP service only after explicit approval", "source-packet.md only for current MCP wiring/source lookup or code-fact writeback", "continuity-ledger.md only for capability/MCP boundary lookup/writeback", "feature-oracle.json only for AS-F004 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["apps/assistant-service/src/assistant_service/core/mcp", "apps/assistant-service/src/assistant_service/core/tools/connector_registry.py", "apps/assistant-service/src/assistant_service/core/tools/confluence_tool.py", "apps/assistant-service/src/assistant_service/main.py", "apps/assistant-service/src/assistant_service/core/tool_invoker.py", "src/api/v1/mcp.py", "src/api/schemas/mcp.py", "src/api/v1/connectors.py", "src/api/router.py", "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/mcp_repository.py", "database/migrations/07*_agent_mcp_registry*.sql", "tests/services/assistant/tools/test_mcp_capability_policy.py", "tests/services/assistant/test_mcp_runtime.py", "tests/services/assistant/test_connector_credential_principal.py", "tests/api/test_mcp_registry_api.py", "tests/security/test_mcp_security.py", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["native platform tools unrelated to existing Connectors", "model-native web search", "new Connector types", "Skill and Knowledge implementation", "general Agent Studio UI", "Hosted/Embed/API channel implementation", "production MCP server configuration"],
    "external_inputs": ["AS-02 passed capability adapter contract", "production Secret Store interface decision", "service-account versus user-delegated grant ownership policy", "public/embed service-account approval owner", "production outbound egress/private-network policy", "OAuth callback and issuer allowlist for an approved live smoke"],
    "secrets_required": ["MCP_OAUTH_CLIENT_ID for approved live smoke", "MCP_OAUTH_CLIENT_SECRET_REF for approved live smoke"]
  },
  "tool_policy": {
    "allowed_tools": ["rg and protocol inspection", "apply_patch", "local mock HTTP/OAuth fixtures", "targeted ruff and pytest"],
    "approval_required": ["contact a real MCP/OAuth service", "store a production secret reference", "Docker/live runtime", "migration execution", "deployment", "commit or push"],
    "dangerous_commands": ["request private/link-local/metadata addresses", "log OAuth tokens", "disable TLS verification", "follow unrestricted redirects", "production migration", "git reset --hard"]
  },
  "risk": {"tags": ["mcp", "external-service", "auth", "security", "database", "migration", "agent"], "data_mutation": true, "migration_required": true, "browser_required": false, "ai_eval_required": true, "external_service_required": true, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "mcp-api-runtime", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_mcp_registry_api.py tests/api/test_internal_mcp_broker.py tests/unit/test_mcp_gateway_broker.py", "expected": "Gateway-owned MCP CRUD, broker protocol runtime, tenant/grant policy, and connector credential-principal isolation are covered by the current tests.", "required": true},
      {"id": "mcp-security", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_internal_mcp_broker.py tests/unit/test_mcp_gateway_broker.py", "expected": "Gateway MCP broker secret, SSRF, OAuth, public-channel and fail-closed security tests pass.", "required": true},
      {"id": "lint", "cwd": ".", "command": "uv run ruff check src/api/v1/mcp.py src/api/internal_mcp_broker.py src/services/agent_runtime/mcp_gateway_broker.py packages/ai-gateway-core/src/ai_gateway_core/mcp packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/mcp_repository.py tests/api/test_mcp_registry_api.py tests/api/test_internal_mcp_broker.py tests/unit/test_mcp_gateway_broker.py", "expected": "Ruff exits zero for every current Gateway-owned MCP path.", "required": true},
      {"id": "assistant-regression", "cwd": ".", "command": "make test-isolation && make verify-assistant-runtime-dev", "expected": "Assistant isolation/runtime gates pass with MCP disabled, unhealthy, and absent.", "required": true}
    ],
    "browser_checks": ["No MCP management UI is added; inspect OpenAPI and API responses to verify secret values, OAuth tokens, internal resolved IPs, stack traces and raw upstream errors are absent."],
    "regression_scope": ["native tools remain native", "MCP disabled/absent startup", "tenant capability filter", "ToolInvoker audit and approvals", "static internal docgen compatibility or an explicit deprecation decision", "Gateway/Assistant management route reachability"],
    "compliance_gates": ["Streamable HTTP is the only tenant-configurable transport", "Server definitions are separate from credential connections", "every connection declares service_account or user_delegated principal ownership", "delegated calls resolve only the current caller grant and never fall back to admin/another user", "anonymous channels deny delegated and service-account calls unless an Admin explicitly authorizes a specified read-only service-account tool/channel", "existing Connectors use the same principal/channel contract", "secret_ref replaces plaintext credentials", "OAuth uses PKCE/resource metadata/audience and least privilege", "URL/DNS/redirect/Origin checks block SSRF and confused deputy paths", "schema refresh cannot mutate a published Version", "timeouts, response limits and circuit breakers are enforced", "all failures redact internals and fail closed"],
    "acceptance_gates": ["The API creates, tests, lists, updates, disables and deletes only tenant-owned Servers/connections and never returns plaintext credentials.", "User A delegated grant is unavailable to User B, anonymous Hosted/Embed and unrelated API principals; revoke/expiry invalidates caches immediately.", "A service-account connection is denied on public/embed unless an Admin explicitly binds the specified read-only tool and channel.", "Existing Confluence Connector follows the same tenant/principal/channel rules and no second runtime authorization path bypasses them.", "Tool discovery records stable server/tool IDs, JSON schema hash and diff; incompatible changes block new publish binding.", "An Agent can invoke only the exact bound MCP/Connector tool through AS-02 policy.", "Mock tests cover no-auth, bearer and OAuth paths; lack of approved real credentials is an external smoke blocker rather than a local failure.", "Independent critic approves protocol, credential principal, security, Agent integration, regression, and minimal-change evidence."],
    "rollback_plan": ["Disable MCP registry and runtime adapters with a feature flag while preserving Server/schema/audit rows.", "Repoint internal docgen to its previously verified native/static path if the new adapter fails, without exposing tenant MCP endpoints.", "Revoke OAuth sessions and invalidate MCP caches during rollback."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-03-mcp-registry-secret-boundary-and-health-report.md", "deploy/runbooks/agent-studio-prd/reports/as-03-critic-verdict.md", "reports/agent-studio/as-03-mcp-security-matrix.json"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "MCP API/protocol test evidence", "security matrix", "schema change diff", "redaction evidence", "Agent capability trace", "external smoke blocker or approved result", "migration and rollback evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "A real third-party OAuth smoke may be deferred by the user when mock protocol coverage passes; SSRF, audience, secret redaction, tenant isolation, exact schema binding, timeout and circuit gates cannot be waived.",
    "next_phase_handoff": "AS-05 receives stable MCP/Connector capability identifiers, principal/channel eligibility, health/status vocabulary, revocation semantics, Agent resolver adapter, audit shape, setup UX fields and cache invalidation contract; AS-05 remains locked until AS-04 also passes."
  },
  "stop_conditions": ["AS-02 is not passed", "Secret Store or egress boundary cannot be represented without plaintext credentials", "the implementation requires tenant-supplied stdio/local process execution", "SSRF/OAuth tests fail open", "a real external service is required without approval"]
}
```

## Requirements

### R1 Tenant-Scoped Registry and Secret Boundary

MCP Server and Tool resources must be tenant-owned, persist only Secret references, redact all responses/logs, and support safe disable/revoke/delete behavior.

### R2 Protocol and Network Security

Tenant MCP uses Streamable HTTP with Origin, URL, DNS, redirect, TLS, response-limit, OAuth resource/audience, PKCE and session controls aligned with the selected MCP protocol version.

### R3 Credential Principal and Connector Boundary

Every MCP/Connector call must resolve an explicit service-account or current-user delegated principal, enforce channel eligibility, and reject cross-user/anonymous fallback; V1 supports existing Connector types only.

### R4 Versioned Discovery and Runtime

Discovery produces immutable schema snapshots/diffs; Agent Versions bind exact tools/hashes and runtime invocation passes through AS-02 capability policy, timeout, concurrency, circuit and audit layers.

### R5 Honest Degradation

Unavailable/changed servers degrade only their bound capability, block incompatible publication, expose stable user errors, and never leak upstream network or authentication details.

## Critic Protocol

Reject if plaintext secrets enter DB/API/logs, tenant `stdio` is enabled, URL checks ignore redirects/DNS changes, OAuth tokens lack resource binding, credential owner/mode is implicit, delegated calls can use another user/admin grant, anonymous channels inherit service credentials, existing Connectors bypass the common policy, schema refresh mutates production versions, unbound tools remain callable, failures bypass policy, or mock security evidence is incomplete.
