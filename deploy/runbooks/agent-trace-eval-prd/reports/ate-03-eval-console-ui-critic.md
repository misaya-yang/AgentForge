# ATE-03 Eval Console UI Critic

Critic: independent fresh-context reviewer for ATE-03 / ATE-F004

Phase: ATE-03

Feature: ATE-F004

Actor Report Reviewed: deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md

Critic Verdict: approved

## Review Scope

The review checked the actor report, Eval frontend files, route/nav integration, Playwright coverage, browser screenshots, auth guard, score submission payload, redaction display, guarded future tabs, command deviation, and minimal-change boundary.

## Findings

- No blocking findings.
- `/eval` is protected by the existing authenticated route shell and `console:usage:view`; unauthorized browser coverage redirects a user without that permission to `/403`.
- The Assistant tab covers list filters, selected trace detail, latency/usage metadata, session/run/request ids, redacted previews, spans, ordered events, and score display/submission.
- Score submission is bounded and browser-side tests assert `tenant_id` is not sent from the client.
- Browser evidence is sufficient for ATE-03: e2e passed with `4 passed`, desktop and mobile screenshots were generated, horizontal overflow assertions passed, and keyboard row focus/Enter selection was covered.
- LangGraph Proxy and RAG tabs are correctly guarded and do not show fake trace data.
- The visible latency note preserves the user requirement that trace recording must not affect agent latency.
- The `web/package.json` edit is an acceptable boundary expansion because it is required to include the Eval smoke in the phase-mandated `e2e:opensource` validation path.

## Requirement Coverage

| Requirement | Evidence | Verdict |
| --- | --- | --- |
| Protected `/eval` route and nav | `web/src/router.tsx`, `web/src/layouts/AppLayout.tsx`, unauthorized e2e | covered |
| Assistant trace list filters and states | `AssistantTraceList.tsx`, e2e seeded list, empty/error/loading components present | covered |
| Trace detail timeline and metadata | `AssistantTraceDetail.tsx`, desktop/mobile screenshots, e2e assertions for redaction/events/scores | covered |
| Score workflow | `TraceScorePanel.tsx`, score POST e2e, no `tenant_id` assertion | covered |
| Guarded future tabs | `EvalPage` future tabs, e2e assertions | covered |
| Responsive/focus/browser checks | Playwright 1440x900, 390x844, focus/Enter, no overflow | covered |

## Test and Regression Assessment

Inspected validation evidence:

- `CI=true pnpm lint` from `web/`: exit 0, 0 errors, 39 existing warnings outside Eval files.
- `CI=true pnpm type-check` from `web/`: exit 0.
- `CI=true pnpm e2e:opensource` from `web/`: exit 0, `4 passed`.
- `uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py`: exit 0, `6 passed`.

The root `pnpm -C web ...` command deviation is documented and does not indicate an ATE-03 code failure because the scripts pass under the package manager version declared by `web/package.json`.

## Minimal-Change Assessment

ATE-03 changed only the frontend Eval route/module, its e2e smoke, a required e2e script inclusion, and harness evidence/state files. It did not modify database schema, assistant runtime capture, LangGraph proxy capture, RAG capture, production data, deployments, or secrets.

## Whole-Demand Regression Assessment

Whole-demand regression is reserved for ATE-04. ATE-03 includes targeted regression sufficient to unlock ATE-04: existing dynamic route e2e smoke and Eval API tests still pass.
