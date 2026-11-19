# AHR-03 Tool Permission And Runtime Safety Plan

Status: planned

## Selected Phase And Feature

- Phase: AHR-03 Tool Permission And Runtime Safety
- Feature oracle item: AHR-F004
- Dependency state: AHR-02 report and completion gate are passed.

## Observed Code Facts

- `ToolRegistry.execute()` currently logs when `requires_confirmation` is true but still executes direct registry calls.
- `ToolRegistry.register()` currently overwrites duplicate tool names by default.
- `CODE_EXECUTOR_TOOL` is medium risk but `requires_confirmation=False`, so direct registry execution must still fail closed based on risk.
- `AssistantExecutionGateway` already evaluates policy, approval, command queue, sandbox decision, argument matching, and single-use approval consumption.
- `MCPManager` sanitizes top-level MCP tool descriptions but parameter descriptions also need the same untrusted bounded redaction.
- `ToolAuditService.summarize_input()` currently truncates raw JSON without redacting secret-like keys/values.

## Implementation Plan

1. Harden `ToolRegistry` as the low-level backstop:
   - add explicit governance fields to `ToolDefinition` for sandbox profile, audit shape, redaction policy, and capability metadata;
   - reject duplicate/shadowed registration unless an explicit trusted override parameter is used;
   - make direct registry execution fail closed for medium/high/confirmation-required tools unless a test-only direct execution bypass is explicitly present in request metadata.
2. Keep `AssistantExecutionGateway` as the production path:
   - preserve existing gateway approval flow;
   - fail closed when approval DB checks error instead of falling back to in-memory mirror when a DB is configured;
   - retain DB-less dev/test fallback only when no DB is configured.
3. Tighten catalog/audit safety:
   - sanitize MCP parameter descriptions with the same bounded untrusted redaction as tool descriptions;
   - redact secret-like keys and values in tool audit input summaries before truncation.
4. Add focused tests:
   - direct registry denial for medium/high/confirmation-required tools and explicit test-only bypass;
   - duplicate registration failure and trusted override;
   - approval DB failure denial;
   - MCP parameter-description sanitization;
   - audit input redaction.
5. Run validation:
   - focused tests for AHR-03 surfaces;
   - changed-file ruff;
   - required `uv run --package assistant-service pytest -q --no-cov tests/services/assistant`;
   - eval golden regression;
   - harness strict and AHR-03 completion gate.
6. Record evidence:
   - write AHR-03 report and independent critic artifact;
   - update only AHR-F004 `status/evidence/notes`;
   - update source-packet, continuity-ledger, progress-log, agent-handoff, loop-state, and next-window-prompt.

## Minimal-Change Boundary

Code edits are limited to Assistant tool safety surfaces named by AHR-03 and tests. No frontend, DB migration, deployment config, production data, secrets, Hermes import, or OpenClaw import is in scope.
