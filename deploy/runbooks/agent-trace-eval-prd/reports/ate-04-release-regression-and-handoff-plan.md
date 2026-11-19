# ATE-04 Release Regression and Handoff Plan

Status: planned

## Scope

Execute terminal whole-demand regression for the first-wave AI Assistant trace Eval feature and write handoff contracts for LangGraph Proxy Trace and RAG Trace. Do not implement new runtime, frontend, database, migration, deployment, or production-data changes in this phase.

## Todo

1. Confirm ATE-F001 through ATE-F004 are passing with actor reports and critic artifacts.
2. Run backend whole-demand regression covering Eval API, assistant trace capture, latency guard, streaming contract, assistant isolation, OpenAPI compatibility, and usage traces.
3. Run backend lint regression for trace API/repository/assistant-service/test surfaces.
4. Run frontend lint, type-check, and open-source e2e with `/eval` coverage.
5. Run open-source environment validation.
6. Run strict harness validation after terminal writeback.
7. Record command deviations honestly, especially the root `pnpm -C web` runtime-version issue observed in ATE-03.
8. Write LangGraph Proxy Trace next-wave contract without implementing it.
9. Write RAG Trace next-wave contract without implementing it.
10. Write terminal actor report and separate critic artifact.
11. Update source packet, continuity ledger, progress log, handoff, next-window prompt, feature oracle, and loop state.

## Validation Map

| Phase Gate | Planned Evidence |
| --- | --- |
| Backend whole-demand regression | `uv run --extra dev --extra test pytest -q --no-cov ...` from ATE-04 contract |
| Backend lint regression | `uv run ruff check ...` from ATE-04 contract |
| Frontend lint/type/e2e | Project-version `pnpm` scripts from `web/`; record root `pnpm -C web` deviation if still present |
| Open-source env | `make validate-example-config` |
| Browser evidence | Reuse latest ATE-03 Playwright screenshots and e2e output; rerun e2e if evidence becomes stale |
| Harness completion | strict validator with quality score |
| Latency guard | ATE-02 latency tests included in whole-demand regression |
| Security/privacy | ATE-01 API tests, ATE-02 redaction tests, ATE-03 unauthorized/no-tenant browser test |

## Minimal-Change Boundary

ATE-04 should edit only the runbook evidence and state files named in `LIKELY_EDIT_PATHS`. If a regression fails because of implementation code, stop and document the blocker unless the fix is surgical, clearly belongs to a previous phase surface, and is necessary to complete first-wave regression.
