# Phase 07 - Hosted App, Embed Widget, and Runtime API

> Agentic worker: expose only AS-06 Publications through three versioned delivery channels with channel-specific auth, origin, memory, capability, rate and token controls.

- PHASE_ID: AS-07
- DEPENDS_ON: AS-06
- UNLOCKS: AS-08
- FEATURE: AS-F008

**Goal:** Deliver stable new-window Hosted chat, origin-isolated Embed Widget, and scoped server Runtime API without exposing internal Agent configuration or credentials.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-07", "number": "07", "title": "Hosted App Embed Widget and Runtime API", "status": "ready", "type": "release", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-07-hosted-app-embed-widget-and-runtime-api.md", "depends_on": ["AS-06"], "unlocks": ["AS-08"]},
  "goal": {
    "target": "Expose immutable Agent Publications through private/tenant/public Hosted pages, an origin-restricted iframe-based Widget, and scoped streaming Runtime API tokens with isolated sessions, memory, capability, quota and abuse controls.",
    "prompt": "Complete AS-07 by following deploy/runbooks/agent-studio-prd/phase-07-hosted-app-embed-widget-and-runtime-api.md after AS-06 passes; implement Hosted, iframe/Widget and Runtime API channel contracts, server-owned version resolution, origin/CSP/postMessage and hashed-token security, anonymous memory/tool/rate policies, browser fixtures and API SDK examples, then finish with security, accessibility, regression, rollback, critic and continuity evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-07-hosted-app-embed-widget-and-runtime-api-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-07-hosted-app-embed-widget-and-runtime-api-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-07-hosted-app-embed-widget-and-runtime-api.md"],
    "primary_context": ["web/src/router.tsx, web/src/pages/SharePage.tsx, existing Assistant chat components, and web/nginx.conf", "src/api/v1/assistant.py, Agent public/embed routes, auth/rate-limit middleware, and streaming proxy contracts", "deploy/helm/ai-gateway/templates/frontend-configmap.yaml and scripts/new deployment-test patterns", "AS-06 Publication API plus AS-02 session/Envelope resolver and ux-spec.md sections 9 through 11"],
    "context_budget": "focused",
    "do_not_load_unless": ["architecture-contract.md sections 5 and 8 for channel/auth invariants", "product-requirements.md section 6.10 for observable channel behavior", "AS-06 report for Publication resolution and rollback", "public DNS/CDN/deployment only after explicit approval", "source-packet.md only for channel/auth source lookup or code-fact writeback", "continuity-ledger.md only for channel/API boundary lookup/writeback", "feature-oracle.json only for AS-F008 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["src/api/v1/agent_runtime.py", "src/api/v1/agent_public.py", "src/api/schemas/agent_runtime.py", "src/core/auth", "src/core/rate_limit", "web/src/pages/agent-public", "web/src/embed", "web/src/services/agentRuntime.ts", "web/src/router.tsx", "web/nginx.conf", "deploy/helm/ai-gateway/templates/frontend-configmap.yaml", "scripts/new/test-agent-embed-headers.sh", "web/e2e/agent-hosted.spec.ts", "web/e2e/agent-embed.spec.ts", "tests/api/test_agent_runtime_api.py", "tests/security/test_agent_channel_security.py", "tests/deployment/test_agent_embed_headers.py", "sdk/python", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["read-only /share/:shareId semantics", "Agent Draft/Version mutation", "MCP/Skill/Knowledge capability semantics", "custom domains", "billing/marketplace", "DNS or production deployment state", "global removal of Hosted/console anti-framing headers"],
    "external_inputs": ["AS-06 passed Publication/channel policy contract", "public/tenant/private access policy", "allowed test origins", "privacy/abuse/retention copy", "approved public rate and cost limits"],
    "secrets_required": ["AGENT_RUNTIME_TOKEN_SIGNING_KEY_REF for approved live runtime", "GATEWAY_ASSISTANT_SHARED_SECRET for approved internal smoke"]
  },
  "tool_policy": {
    "allowed_tools": ["rg and route/auth inspection", "apply_patch", "targeted ruff/pytest", "pnpm/Playwright multi-origin fixtures", "local SDK examples"],
    "approval_required": ["public internet exposure", "DNS/CDN production change", "build/run the frontend Nginx image for header smoke", "live provider costs", "Docker/live runtime", "migration execution", "deployment", "commit or push"],
    "dangerous_commands": ["embed server token in JavaScript", "allow wildcard origin with credentials", "log bearer tokens", "enable anonymous high-risk tools", "production deployment", "git reset --hard"]
  },
  "risk": {"tags": ["release", "frontend", "browser", "auth", "security", "external-service", "agent", "api"], "data_mutation": true, "migration_required": true, "browser_required": true, "ai_eval_required": true, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "runtime-api", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agent_runtime_api.py tests/security/test_agent_channel_security.py", "expected": "Hosted/Embed/API auth, Publication resolution, hashed token create/rotate/revoke/expire/scope, SSE, idempotency, Origin, rate/quota, memory, attachment, feedback and error contracts pass.", "required": true},
      {"id": "frontend-build", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build", "expected": "Hosted and Widget code passes lint, type, i18n and production build.", "required": true},
      {"id": "embed-header-contract", "cwd": ".", "command": "uv run pytest -q --no-cov tests/deployment/test_agent_embed_headers.py && bash scripts/new/test-agent-embed-headers.sh --config-only", "expected": "Gateway, web/nginx.conf and Helm agree that /a remains SAMEORIGIN/self-only while /embed/agents is routed to a dynamic response with no SAMEORIGIN XFO and exact Publication-derived frame-ancestors.", "required": true},
      {"id": "built-nginx-header-smoke", "cwd": ".", "command": "bash scripts/new/test-agent-embed-headers.sh --built-image", "expected": "After explicit Docker approval and compose-ownership checks, actual responses from the built frontend/Gateway image show anti-framing headers on Hosted/console and Publication-specific frame-ancestors with no SAMEORIGIN XFO on the dedicated Embed document.", "required": true},
      {"id": "channel-browser", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-hosted.spec.ts e2e/agent-embed.spec.ts --config playwright.opensource.config.ts", "expected": "Hosted and allowed/rejected multi-origin Embed scenarios, accessibility, network redaction, postMessage and responsive behavior pass.", "required": true},
      {"id": "runtime-regression", "cwd": ".", "command": "make verify-assistant-runtime-dev && make test-isolation && corepack pnpm@10.33.0 -C web e2e:opensource", "expected": "Built-in Assistant, service isolation and existing Web routes remain valid.", "required": true}
    ],
    "browser_checks": ["At /a/:publicId on 1440x900 and 390x844 capture private auth, tenant auth, public welcome, streaming, citations, attachments, feedback, disabled, revoked-version, quota and provider-error states; confirm SAMEORIGIN/frame-ancestors self headers prevent cross-origin framing.", "On allowed and rejected Origin fixtures at 1280x800 and 390x844 load the dedicated /embed/agents/:publicId document and verify actual frame-ancestors/XFO headers, launcher/inline mode, ready/resize/open/close/new_message/error postMessage source+origin checks, focus return, reduced motion and token expiry.", "Inspect browser source, local/session storage, network headers/bodies, console and errors to prove no server API token, Secret, internal Prompt/Snapshot, upstream URL or stack trace is exposed.", "Run axe and keyboard tests on Hosted and Widget; capture zero critical/serious violations and no horizontal overflow."],
    "regression_scope": ["/share/:shareId remains read-only", "protected /assistant and Studio", "SSE event compatibility", "Publication/session pinning", "anonymous memory and capability policy", "API idempotency and rate middleware", "SDK authentication"],
    "compliance_gates": ["browser never receives a reusable server token", "API tokens are hashed, one-time displayed, scoped, expiring, rotatable and revocable", "ordinary Hosted/console responses retain SAMEORIGIN and frame-ancestors self", "dedicated Embed responses remove SAMEORIGIN XFO and generate exact Publication frame-ancestors", "Nginx and Helm preserve the dynamic Embed route instead of applying global SPA headers", "allowed origins are exact and wildcard credentials are forbidden", "postMessage validates origin/source/version", "CSP and iframe sandbox are restrictive", "anonymous channels default to session-only memory and no high-risk/write tools", "IP and Publication quotas bound abuse/cost", "errors and analytics minimize PII"],
    "acceptance_gates": ["A Hosted Publication opens in a new window at a stable public ID, enforces access and cannot be framed cross-origin.", "The dedicated Embed document initializes only on allowed origins, has actual dynamic frame-ancestors/no SAMEORIGIN XFO, contains no server token, and maintains accessible focus/resize/message behavior.", "Config-level and built Nginx/Gateway response tests both prove production header behavior; a Vite-only success is insufficient.", "A scoped Runtime API token can stream, continue its own sessions, attach allowed files and submit feedback but cannot access another Publication or forbidden capability.", "Disabling/rolling back/revoking a Publication or token propagates with stable errors and cache invalidation.", "Independent critic approves channel/header security, anonymous policy, browser/network evidence, API compatibility, rollback and minimal-change scope."],
    "rollback_plan": ["Disable public, embed and API channels independently while keeping private Studio/Preview available.", "Revoke issued tokens and invalidate Publication/channel caches.", "Remove Agent public navigation/Widget loader without changing /share or /assistant routes."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-07-hosted-app-embed-widget-and-runtime-api-report.md", "deploy/runbooks/agent-studio-prd/reports/as-07-critic-verdict.md", "reports/agent-studio/as-07-channel-security.json", "reports/agent-studio/as-07-browser-network.md", "reports/agent-studio/as-07-screenshots"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "Hosted/Embed/API test evidence", "Origin/CSP/postMessage matrix", "browser token/secret redaction evidence", "anonymous memory/tool policy trace", "rate/quota and token lifecycle evidence", "desktop/mobile/axe screenshots", "SDK example result", "rollback evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "A production DNS/live-provider smoke may be deferred by the user with local fixture evidence; Origin/CSP/postMessage, token secrecy/scope/revocation, anonymous memory/tool defaults, rate limits, tenant isolation and stable disable/rollback behavior cannot be waived.",
    "next_phase_handoff": "AS-08 receives stable channel IDs/events, auth and quota dimensions, token audit events, public/browser probes, rollback switches and required operational metrics."
  },
  "stop_conditions": ["AS-06 is not passed", "public quota/retention policy is required but absent", "browser delivery requires a reusable server token", "Origin or anonymous capability policy fails open", "the built Nginx/Gateway header contract cannot be verified with explicit Docker approval", "production DNS/deployment/live provider access is required without approval"]
}
```

## Requirements

### R1 Stable Hosted Delivery

Hosted Publications use stable public IDs, resolve server-side Versions, support private/tenant/public access, and present complete chat states without exposing internal configuration.

### R2 Origin-Isolated Embed

Iframe/Widget delivery must enforce exact origins, CSP, sandbox, versioned postMessage and accessible focus/resize while keeping all reusable server credentials out of the browser.

### R3 Scoped Runtime API

Hashed, rotatable, revocable and expiring tokens scope every stream/session/attachment/feedback request to a Publication, caller policy, quota and idempotency namespace.

### R4 Safe Anonymous Defaults and Rollback

Anonymous Hosted/Embed is session-memory-only with high-risk/write capabilities disabled, and each channel can be disabled/rolled back independently with cache invalidation.

## Critic Protocol

Reject if public IDs bypass auth, browser code/storage/network contains server tokens or protected config, ordinary Hosted pages lose anti-framing headers, Embed relies on the global SPA/Vite headers, built Nginx/Helm responses are not tested, wildcard origins combine with credentials, postMessage omits source/origin checks, anonymous memory/tools fail open, tokens are reversible/unscoped, sessions cross channels, or /share and /assistant regress.
