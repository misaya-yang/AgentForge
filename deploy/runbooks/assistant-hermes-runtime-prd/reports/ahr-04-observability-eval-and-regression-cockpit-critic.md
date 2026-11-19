# AHR-04 Observability Eval And Regression Cockpit Critic

Critic: independent fresh-context reviewer

Critic Verdict: approved

Feature: AHR-F005

Phase: AHR-04

Actor Report Reviewed: deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-04-observability-eval-and-regression-cockpit-report.md

## Review Scope

Reviewed the AHR-04 actor report, runtime trace writer changes, Eval repository/schema/service changes, golden fixture additions, Eval UI/i18n changes, focused backend tests, web static gates, browser route evidence, and minimal-change boundary.

## Findings

No completion-blocking issues found.

## Acceptance Review

- Assistant trace capture now records bounded runtime trajectory evidence and tool-safety decisions without raw secret payloads.
- Eval dashboard responses expose runtime health fields needed for AHR-05 doctor/status and release-gate work.
- Trace-to-dataset and trace feedback paths preserve expected runtime trajectory metadata for golden and review workflows.
- Golden regression includes runtime safety, memory-observability, context capability, and trajectory cases.
- `/eval` surfaces runtime cockpit and detail trajectory state with synchronized English and Chinese i18n keys.
- Browser route checks covered desktop, thread view, deep Chinese theme state, mobile viewport, family switching, RAG/LangGraph/KB RAGAS paths, score submission, export, dataset, and no-horizontal-overflow checks.

## Validation Reviewed

- Focused eval/API/golden tests: 40 passed, 13 warnings.
- Assistant trace capture tests: 18 passed, 1 warning.
- `make verify-eval-dev`: passed, including golden gate for 16 cases with `trajectory_pass_rate=1.0`.
- Changed-file ruff check: passed.
- Web lint: passed with 0 errors and 38 existing warnings.
- Web type-check: passed.
- Web i18n check: passed.
- Web build: passed with existing large chunk warnings.
- Eval Playwright route check: 2 passed after synchronizing the test to the 16-case fixture and stabilizing duplicate toast matching.
- `git diff --check`: passed.

## Residual Risk

The dashboard runtime-health SQL aggregation was validated through repository/API contract tests and static checks, not through a production or migrated live database. This is acceptable for AHR-04 because the phase forbids production mutation and does not add a schema migration; AHR-05 should include it in read-only doctor/status coverage.
