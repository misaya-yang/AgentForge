# Agent Trace Eval PRD Harness Agent Handoff

**Created:** 2026-06-26

**Harness Folder:** `deploy/runbooks/agent-trace-eval-prd`

---

## Planner Notes

- Product scope: new Eval module for three trace families, with first wave scoped to AI assistant traces.
- Harness path: `deploy/runbooks/agent-trace-eval-prd`.
- First target phase: ATE-00.
- First target phase file: `deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md`.
- First feature-oracle item: ATE-F001.
- Key design decision: implement vendor-neutral trace tables and APIs; use OTel/request traces as correlation inputs rather than the sole source of truth.
- Key privacy decision: persist redacted previews and structured metadata by default; raw full content retention is outside first wave.

## Generator Notes

- Work on one phase and one oracle item.
- Load only `context-profile.json`, `loop-state.json`, and the target phase file before planning.
- For ATE-00, do not edit application code; write evidence and contract updates only.
- For ATE-01, own database migration, backend schemas, route registration, auth/tenant tests, and score API.
- For ATE-02, own assistant-service trace capture and redaction without changing public chat/SSE contracts.
- For ATE-03, own `/eval` route, navigation, API client, Assistant trace UI, score UI, and browser checks.
- For ATE-04, own whole-demand regression, release readiness evidence, and next-wave LangGraph/RAG handoff notes.
- Every phase must write an actor report and request a separate independent critic artifact.

## Critic Notes

- Reject completion if the actor report lacks concrete command output, privacy/tenant-isolation evidence, minimal-change notes, or phase-boundary proof.
- Reject API completion if cross-tenant trace access, trace id enumeration, or raw secret exposure is not tested.
- Reject runtime completion if streaming terminal events or existing assistant session/run behavior regress.
- Reject UI completion if `/eval` lacks desktop and mobile checks, empty/error/loading states, keyboard focus notes, or score workflow evidence.
- Terminal ATE-04 must include whole-demand regression for ATE-F001 through ATE-F004.

## Next Handoff

- Active role: terminal handoff complete.
- ATE-00 result: passed.
- ATE-01 result: passed.
- ATE-02 result: passed.
- ATE-03 result: passed.
- ATE-01 actor report: `deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-report.md`.
- ATE-01 critic artifact: `deploy/runbooks/agent-trace-eval-prd/reports/ate-01-ai-assistant-trace-schema-and-api-critic.md`.
- ATE-02 actor report: `deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-report.md`.
- ATE-02 critic artifact: `deploy/runbooks/agent-trace-eval-prd/reports/ate-02-assistant-trace-capture-critic.md`.
- ATE-03 actor report: `deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-report.md`.
- ATE-03 critic artifact: `deploy/runbooks/agent-trace-eval-prd/reports/ate-03-eval-console-ui-critic.md`.
- ATE-04 actor report: `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-report.md`.
- ATE-04 critic artifact: `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-release-regression-and-handoff-critic.md`.
- ATE-04 result: passed.
- Former blocking gate: required broad ruff over `apps/assistant-service/src/assistant_service` initially found 636 lint errors; lint-baseline remediation cleared it.
- Remediation plan: `deploy/runbooks/agent-trace-eval-prd/reports/ate-04-lint-baseline-remediation-plan.md`.
- Passed ATE-04 checks: broad backend ruff passed; backend whole-demand pytest `33 passed, 2 skipped`; frontend lint/type/e2e passed; `make validate-example-config` passed; harness strict validation, ATE-04 completion gate, and full-demand completion gate passed with quality score 100.
- Post-review trace hardening is included: failed non-stream setup finalizes a failed trace, terminal span status cannot regress to running after delayed writes, non-operator Eval access requires authenticated user scope, and regression tests cover concurrent users plus same-session multi-turn chats.
- Next action: ATE-04 remains the verified terminal phase for harness validator compatibility. No further PRD phase remains; future work should start as a new user-approved expansion for LangGraph Proxy Trace or RAG Trace.
- LangGraph Proxy Trace and RAG Trace expansion contracts are documented in the ATE-04 report, source packet, and continuity ledger. Do not implement those families without an explicit next-stage request.
- Frontend validation caveat: in this Codex runtime, root `pnpm -C web ...` resolves to pnpm 11.x and fails before scripts run; project-version commands from `web/` use `pnpm@10.33.0` and passed.
