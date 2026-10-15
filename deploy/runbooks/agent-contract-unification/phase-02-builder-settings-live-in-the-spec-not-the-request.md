# Phase 02 - Builder settings live in the spec, not the request

- PHASE_ID: ACU-02
- FEATURE_ID: ACU-F003
- DEPENDS_ON: ACU-01
- UNLOCKS: none

## Outcome

Retrieval tuning, skill enablement, and execution policy are resolved from the agent's spec. A chat
request carries only what an end user genuinely decides mid-conversation. Existing clients that
still send the old fields keep working and are told, in the response, that those fields are
deprecated.

`ChatRequest` today asks the caller for eleven builder-level decisions
(`src/api/schemas/assistant.py:141-220`): `kb_dataset_ids`, `kb_mode`, `kb_top_k`,
`kb_score_threshold`, `kb_include_images`, `web_search_enabled`, `enable_task_planning`,
`thinking_level`, `execution_profile`, `skills_enabled`, plus MCP binding. `kb_top_k` and
`kb_score_threshold` are RAG tuning parameters; no end user can answer them. Since surfaces are
embedded in other people's products, these belong to whoever built the agent, not to whoever is
talking to it.

## Scope

In:
- Resolve `kb_*`, `skills_enabled`, `web_search_enabled`, and `execution_profile` from the resolved
  `AgentSpec`.
- Keep a small session-level surface for what a user really chooses. One depth control that maps to
  both thinking level and execution profile is the recommended shape; record the mapping in the
  phase report.
- Mark the superseded `ChatRequest` fields deprecated in the OpenAPI description, and honour them
  as an override when present.
- `enable_task_planning` defaults to `False` and has no UI — propose removing it and record the
  decision rather than silently keeping a dead switch.

Out:
- Deleting any `ChatRequest` field. Deprecate now; deletion is a separate release decision.
- Changing retrieval behaviour, ranking, or defaults.
- Frontend rework beyond whatever is needed to stop sending builder-level fields.

## Done when

- [ ] A chat request that carries none of the builder fields runs with the agent spec's values.
- [ ] A request that still carries `kb_top_k` overrides the spec and is reported as deprecated.
- [ ] The console no longer sends `kb_top_k`, `kb_score_threshold`, or `kb_include_images`.
- [ ] The session-level control set is documented and is at most: a depth control plus anything the phase report justifies.
- [ ] `enable_task_planning` is either removed or its retention is justified in writing.
- [ ] Assistant API contract tests pass, including for legacy payloads.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Spec resolution | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_assistant_request_resolution.py` | A request with no builder fields runs on the spec's values, and a legacy field still overrides |
| Contract snapshot | `uv run --all-packages --extra test pytest -q --no-cov tests/integration/test_assistant_openapi_contract.py` | Deprecations are reflected without breaking the published surface |
| Frontend | `pnpm -C web type-check && pnpm -C web lint` | The console builds without the retired builder fields |

## Stop or confirm

- Removing any `ChatRequest` field. Deprecate in this phase; deletion is a separate release decision.
- Changing what the depth control maps to, since that is user-visible behaviour.
- A deprecation that would break an already published SDK version — report it instead of proceeding.
