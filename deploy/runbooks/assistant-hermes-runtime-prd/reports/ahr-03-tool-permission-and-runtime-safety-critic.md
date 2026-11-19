# AHR-03 Tool Permission And Runtime Safety Critic

Critic: independent fresh-context reviewer

Critic Verdict: approved

Feature: AHR-F004

Phase: AHR-03

Actor Report Reviewed: deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-03-tool-permission-and-runtime-safety-report.md

## Review Scope

Reviewed the AHR-03 actor report, changed Assistant tool/gateway/audit/MCP/code-executor files, focused AHR-F004 tests, full assistant-service regression output, eval golden output, changed-file lint output, and whitespace gate evidence.

## Findings

No completion-blocking issues found.

## Acceptance Review

- Medium/high-risk and confirmation-required tools now fail closed on direct registry execution unless routed through the gateway-approved path or a pytest-only bypass.
- Duplicate tool registration fails by default, while trusted startup refreshes require explicit override call sites.
- Approval DB query, checkpoint approval, and approval consumption failures deny risky actions instead of falling back to permissive in-memory state.
- Gateway execution decisions expose policy, sandbox, approval, and consumption metadata to the invocation path.
- Audit summaries redact secret-like argument keys and values before persistence-friendly summaries are built.
- MCP catalog and parameter descriptions are bounded, sanitized, and marked as external/untrusted capability text.
- Code execution no longer treats missing configured sandbox runtime as acceptable unless fallback is explicitly configured.

## Validation Reviewed

- Focused AHR-03 test slice: 16 passed, 1 warning.
- Expanded code-executor/Confluence/tool-safety slice: 49 passed, 1 warning.
- Changed-file ruff check: passed.
- Full assistant-service tests: 1058 passed, 1 warning.
- Eval golden regression gate: 4 passed, 1 warning.
- `git diff --check`: passed with no output.

## Residual Risk

The gateway-approved marker is an internal server-side convention. That is acceptable for AHR-03 because the registry denial closes accidental direct execution paths and the gateway is the only production path that should set the marker; future review should reject new internal callers that set it outside the gateway.
