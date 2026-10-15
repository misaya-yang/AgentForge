# ATE-03 Eval Console UI Plan

Status: planned

## Design Read

The web app is an authenticated enterprise operations console. Existing UI uses a fixed app sidebar, compact header chrome, Ant Design tables/forms, Tailwind design tokens, and slate/steel operational colors. The Eval module should be data-dense, calm, and scannable; it should not look like a marketing page or standalone landing page.

## Todo

1. Repair ATE-03 runtime pointers that referenced the wrong phase filename and use `phase-03-eval-console-ui.md` as the authoritative phase file.
2. Add `web/src/api/eval.ts` with typed client functions matching ATE-01 list/detail/score schemas.
3. Add `/eval` route and sidebar nav entry behind the existing authenticated app shell and `console:usage:view` permission.
4. Build the Eval page with Assistant, LangGraph Proxy, and RAG tabs; only Assistant is functional, while future tabs are guarded.
5. Build Assistant trace list filters for status, model, user, session, run id, date range, and score status; include loading, empty, error, unauthorized-safe route behavior, and selected-row states.
6. Build trace detail timeline, metadata panels, redaction indicators, linked ids, usage/latency/error views, and sparse-span fallback rendering.
7. Build score panel for existing scores and bounded score submission without tenant_id in the browser payload.
8. Add Playwright open-source route smoke and viewport/focus checks for `/eval`.
9. Run required validation commands and browser checks, then write report, critic, source-packet, ledger, progress, oracle, handoff, and loop-state evidence.

## Minimal Change Boundary

Only frontend Eval API/client/page/route/nav/e2e files and ATE-03 harness evidence should change. No backend schema, assistant-service, LangGraph proxy, RAG trace, deployment, production data, or external observability integration work is in scope.

## Verification Map

- Static: `pnpm -C web lint`, `pnpm -C web type-check`.
- Browser/e2e: `pnpm -C web e2e:opensource` plus direct desktop/mobile screenshots when feasible.
- Backend compatibility: `uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py`.
- Harness: strict validator and ATE-03 completion gate after evidence writeback.
