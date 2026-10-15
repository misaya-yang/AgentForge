# ATE-01 AI Assistant Trace Schema and API Plan

## Selected Scope

- Phase: ATE-01 AI Assistant Trace Schema and API
- Feature oracle item: ATE-F002
- Dependency: ATE-00 passed with actor and critic evidence
- Implementation scope: additive trace schema, Eval schemas/routes, repository helper, router registration, API tests, and OpenAPI compatibility
- Excluded scope: assistant-service trace capture, frontend UI, LangGraph Proxy capture, RAG capture, production migration execution

## Todo List

1. Inspect existing auth, permission, repository, and API test patterns needed for a tenant-scoped Eval route.
2. Add `database/migrations/060_agent_trace_eval.sql` with root trace, span, event, and score tables plus indexes and constraints.
3. Add Eval Pydantic schemas for list, detail, span, event, score, and score create payloads.
4. Add an async repository helper that filters every read/write by server-side tenant context.
5. Add FastAPI Eval routes under `/eval/traces` for list, detail, and score write.
6. Register the Eval router in `src/api/router.py`.
7. Add API tests proving same-tenant access, cross-tenant rejection, assistant-only filtering, score write, and OpenAPI route presence.
8. Run ATE-01 validation commands, fix failures, and then write report/critic evidence.

## Minimal-Change Boundary

ATE-01 edits only files named by the phase contract. If an existing auth or permission helper needs inspection, it is read-only unless a failing test proves a phase-scoped edit is unavoidable.

## Validation Commands

```bash
uv run ruff check src/api/v1/eval.py src/api/schemas/eval.py packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py tests/api/test_eval_traces.py tests/contract/test_openapi_schema_compat.py
uv run --extra dev --extra test pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_gateway_tenant_isolation.py tests/contract/test_openapi_schema_compat.py
rg -n 'agent_traces|agent_trace_spans|agent_trace_events|agent_trace_scores|tenant_id|trace_family|assistant' database/migrations/060_agent_trace_eval.sql
python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score
```

## Review Method

The critic must verify tenant isolation, redaction fields, additive migration design, rollback path, OpenAPI compatibility, test sufficiency, and minimal-change scope before ATE-02 is unlocked.
