# Phase 05 - Agent Studio Frontend and Preview

> Agentic worker: build the Agent catalog, configuration Studio and Draft/Version Preview against passed APIs; do not implement external publication channels.

- PHASE_ID: AS-05
- DEPENDS_ON: AS-03, AS-04
- UNLOCKS: AS-06
- FEATURE: AS-F006

**Goal:** Deliver a complete, responsive and accessible Agent creation/configuration experience whose Preview reveals the exact saved Draft or immutable Version in use.

## Machine Contract

```json
{
  "schema_version": "prd-phase-harness/v3",
  "harness_role": "execution",
  "phase": {"id": "AS-05", "number": "05", "title": "Agent Studio Frontend and Preview", "status": "ready", "type": "implementation", "repo_path": ".", "docs_path": "deploy/runbooks/agent-studio-prd", "phase_file": "deploy/runbooks/agent-studio-prd/phase-05-agent-studio-frontend-and-preview.md", "depends_on": ["AS-03", "AS-04"], "unlocks": ["AS-06"]},
  "goal": {
    "target": "Add Agent list/create/Studio routes, typed API state, conflict-safe autosave/manual save, capability and knowledge configuration, responsive accessible forms, and isolated Draft/Version Preview using the existing Assistant visual language.",
    "prompt": "Complete AS-05 by following deploy/runbooks/agent-studio-prd/phase-05-agent-studio-frontend-and-preview.md only after both AS-03 and AS-04 pass; inventory current UI patterns, implement the Agent directory, creation flow, Studio sections, configuration states and isolated Preview, preserve existing Assistant routes/components, verify all route/state/viewport/accessibility/network matrices, and finish with screenshots, tests, critic, rollback, and continuity evidence.",
    "plan_required": true,
    "plan_output": "deploy/runbooks/agent-studio-prd/reports/as-05-agent-studio-frontend-and-preview-plan.md",
    "completion_report": "deploy/runbooks/agent-studio-prd/reports/as-05-agent-studio-frontend-and-preview-report.md"
  },
  "runtime": {"context_profile": "deploy/runbooks/agent-studio-prd/context-profile.json", "feature_oracle": "deploy/runbooks/agent-studio-prd/feature-oracle.json", "loop_contract": "deploy/runbooks/agent-studio-prd/loop-contract.json", "loop_state": "deploy/runbooks/agent-studio-prd/loop-state.json", "progress_log": "deploy/runbooks/agent-studio-prd/progress-log.md", "handoff": "deploy/runbooks/agent-studio-prd/agent-handoff.md", "continuity_ledger": "deploy/runbooks/agent-studio-prd/continuity-ledger.md", "next_window_prompt": "deploy/runbooks/agent-studio-prd/next-window-prompt.md", "session_boot": {"read_progress": true, "run_baseline_check": true, "update_progress_before_exit": true, "check_loop_stop_before_iteration": true}, "agent_roles": ["planner", "generator", "critic"]},
  "context": {
    "read_first": ["deploy/runbooks/agent-studio-prd/context-profile.json", "deploy/runbooks/agent-studio-prd/loop-state.json", "deploy/runbooks/agent-studio-prd/phase-05-agent-studio-frontend-and-preview.md"],
    "primary_context": ["web/src/router.tsx and existing protected layout/navigation", "web/src/pages/assistant and web/src/components/ConversationSidebar.tsx", "web/src/pages/knowledge and existing API/query/form patterns", "deploy/runbooks/agent-studio-prd/ux-spec.md sections 2 through 11"],
    "context_budget": "focused",
    "do_not_load_unless": ["product-requirements.md sections 6.1-6.9 for a disputed UI behavior", "AS-04 report for capability API payloads and errors", "current deployed site only after browser runtime approval", "Figma or external design assets only if the user supplies them", "source-packet.md only for current UI/API source lookup or code-fact writeback", "continuity-ledger.md only for UI/API boundary lookup/writeback", "feature-oracle.json only for AS-F006 evidence writeback", "progress-log.md only for the latest blocker or exit-state append"]
  },
  "boundaries": {
    "likely_edit_paths": ["web/src/router.tsx", "web/src/pages/agents", "web/src/components/agents", "web/src/services/agents.ts", "web/src/types/agents.ts", "web/src/locales", "web/e2e/agent-studio.spec.ts", "web/e2e/fixtures/agent-studio.ts", "deploy/runbooks/agent-studio-prd"],
    "do_not_edit": ["existing /assistant behavior beyond reusable component extraction", "backend schemas not covered by a documented blocker", "Hosted /a/:publicId", "Embed Widget", "Runtime API tokens", "visual language unrelated to Agent routes"],
    "external_inputs": ["AS-03 passed MCP/Connector principal/health API contract", "AS-04 passed Skills/Knowledge API contract", "test users for Owner, Editor and Viewer browser states", "approved product logo/icon assets or existing asset reuse"],
    "secrets_required": []
  },
  "tool_policy": {
    "allowed_tools": ["rg and UI pattern inspection", "apply_patch", "pnpm lint/type/build/i18n", "Playwright browser tests", "screenshots and axe"],
    "approval_required": ["change backend API contract", "install frontend dependency", "live deployment or production browser mutation", "commit or push"],
    "dangerous_commands": ["hardcode credentials", "disable accessibility checks", "overwrite existing Assistant route", "git reset --hard", "rm -rf"]
  },
  "risk": {"tags": ["ui", "frontend", "browser", "accessibility", "agent"], "data_mutation": true, "migration_required": false, "browser_required": true, "ai_eval_required": true, "external_service_required": false, "release_blocking": true},
  "validation": {
    "commands": [
      {"id": "frontend-static", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build", "expected": "Lint, TypeScript, i18n key check and production build all exit zero.", "required": true},
      {"id": "agent-studio-e2e", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-studio.spec.ts --config playwright.opensource.config.ts", "expected": "List/create/Studio/save/conflict/permission/degraded/Preview/responsive/accessibility scenarios pass against deterministic fixtures.", "required": true},
      {"id": "existing-route-e2e", "cwd": ".", "command": "corepack pnpm@10.33.0 -C web e2e:opensource", "expected": "Existing dynamic route and eval trace open-source E2E tests pass.", "required": true},
      {"id": "preview-contract", "cwd": ".", "command": "uv run pytest -q --no-cov tests/api/test_agents_api.py tests/services/agent_runtime/test_control_plane.py", "expected": "The frontend-consumed Agent/Preview API and current Gateway-owned resolver contracts remain valid.", "required": true}
    ],
    "browser_checks": [
      "At /agents on 1440x900 and 390x844 capture loading, empty, populated, filtered-empty, API error and permission-denied states; verify keyboard order and no horizontal overflow.",
      "At /agents/:agentId on 1440x900, 1024x768 and 390x844 capture clean, dirty, saving, saved, field-error, 409 conflict, degraded resource and Viewer states.",
      "In Studio Preview verify Draft rN versus Version N labels, unsaved-change warning, new-session behavior on config/version switch, effective capabilities, tool/RAG events, clear session and Trace link.",
      "Run axe on Agent list, creation flow and Studio; record zero critical/serious violations, visible focus, error association, reduced-motion behavior, console errors and failed network requests."
    ],
    "regression_scope": ["/assistant", "/knowledge", "/playground", "/eval", "protected navigation", "existing streaming chat/tool/RAG components", "mobile sidebar", "i18n fallbacks"],
    "compliance_gates": ["no Secret/token fields enter browser state or local recovery", "UI never treats hidden controls as authorization", "all inputs have labels and errors", "keyboard and focus management cover dialogs/sheets", "390x844 has no horizontal overflow", "colors are not the only status signal", "user-visible text uses i18n", "Preview exposes effective state without internal prompts/secrets"],
    "acceptance_gates": ["Owner can create from blank/template and configure every V1 section without copying credentials, sessions, memory or inaccessible resources.", "Save states distinguish dirty/saving/saved/error/conflict and preserve local non-secret edits through recoverable network failure.", "Capability rows show source, risk, setup, health, version/schema and permission; unavailable combinations block with actionable errors.", "Preview runs only a saved Draft revision or Version in a separate session namespace and never hot-swaps an active session.", "Independent visual/accessibility critic approves route/state/viewport coverage, existing visual language, tests and minimal-change scope."],
    "rollback_plan": ["Remove the Agent navigation entry and disable Agent routes behind the frontend feature flag while leaving backend data intact.", "Preserve existing /assistant components and routing; revert only Agent-specific extractions if regression occurs."]
  },
  "evidence": {
    "outputs": ["deploy/runbooks/agent-studio-prd/reports/as-05-agent-studio-frontend-and-preview-report.md", "deploy/runbooks/agent-studio-prd/reports/as-05-critic-verdict.md", "reports/agent-studio/as-05-browser-matrix.md", "reports/agent-studio/as-05-screenshots"],
    "required_artifacts": ["phase report", "progress log entry", "feature oracle evidence", "continuity ledger writeback", "source packet code-fact update", "handoff update", "frontend command evidence", "browser route/state/viewport matrix", "desktop/tablet/mobile screenshots", "axe/keyboard/focus evidence", "console/network summary", "Preview golden trace", "regression evidence", "independent critic evidence", "minimal-change scope note"],
    "waiver_policy": "A specific non-critical visual polish item may be user-waived with screenshot and residual risk; save correctness, effective configuration truth, mobile reachability, keyboard/focus, critical/serious axe, Secret handling and authorization cannot be waived.",
    "next_phase_handoff": "AS-06 receives stable Draft revision UI state, Version selectors, Preview session behavior, validation summary components, API error mapping, and browser fixtures for Publish/Diff/Eval additions."
  },
  "stop_conditions": ["AS-03 or AS-04 is not passed", "frontend requires an undocumented backend contract change", "Owner/Editor/Viewer test identities are unavailable and cannot be mocked", "critical accessibility or save-conflict behavior cannot be fixed in scope", "an external design rewrite is requested without product approval"]
}
```

## Requirements

### R1 Complete Agent Information Architecture

`/agents`, `/agents/new` and `/agents/:agentId` must cover list, identity, instructions, model, capabilities, Knowledge, memory/safety, eval/publish entry and analytics/channel entry without replacing `/assistant`.

### R2 Truthful Conflict-Safe Editing

UI state must reflect server revisions and validation; stale edits produce a recoverable diff/conflict path and no failed save is shown as success.

### R3 Isolated Explainable Preview

Preview always names the saved Draft revision or immutable Version, starts a new session on configuration change, and displays effective capabilities/trace without revealing protected internals.

### R4 Responsive Accessible Quality

All required states work at desktop/tablet/mobile sizes with keyboard, focus, screen-reader labels, AA contrast, reduced motion and no critical/serious axe failures.

## Critic Protocol

Reject if the UI hides capability source/risk/setup, sends secrets, relies on hidden buttons for auth, silently overwrites Drafts, previews unsaved content without labeling, hot-swaps versions, omits error/permission/degraded/mobile states, introduces generic landing-page styling, or lacks browser/axe/console/network evidence.
