# AS-09 Preflight Regression Repair Critic

- Review date: 2026-07-20
- Role: fresh independent Critic
- Verdict: **approved**
- Blocking findings: **none**
- Scope: the two failures in the first AS-09 aggregate, the three named repair artifacts, and only the router/config/evidence definitions needed to disprove the repair claim

This verdict approves the routed AS-01/AS-02 repair only. It does **not** approve AS-09 or reuse the earlier 37 passing gates as post-repair evidence. The complete 39-gate aggregate must run again against one stable post-repair source hash.

## Original Failure Evidence

`reports/agent-studio/agent-studio-regression-v1-result.json` records a stable-source run with manifest SHA-256 `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d` and source SHA-256 `3518b0df0ff14c41f4ff05d8976b9abbe6c6e06cd26d7b3dbd62a7f16beda58a`:

| Result | Count |
|---|---:|
| Required gates | 39 |
| Executed gates | 39 |
| Passed gates | 37 |
| Failed gates | 2 |

The two failure-log hashes match the hashes recorded in the aggregate result:

- AS-01 `gateway-regression`: `95455469b45c577bb0122e3d2169661b56a85aa8ef0969947e9437c75eaa4466`; 13 passed, 1 failed, 0 skipped. The sole failure expected `403` from `/assistant/mcp/{servers,tools}` but mounted the new `/mcp` registry router, so FastAPI correctly returned route-level `404` before authorization.
- AS-02 `trace-session`: `b227330591bfd1f67349f64b1ddc68ebe76cd002690229a09de55acf9182a364`; 42 passed, 3 failed, 0 skipped. The failures were two frozen-field snapshot drifts (`kb_retrieval_configs`, plus `kb_include_images` in `AgentLoopConfig`) and one stale golden evidence test name.

## Repair Assessment

### MCP authorization test

`tests/security/test_management_api_authorization.py` changes only the router mounted by the test from `mcp_routes.router` to `mcp_routes.legacy_router`. It preserves both legacy request paths and both `status_code == 403` assertions.

This is semantically correct, not a weakened test:

- the current registry router owns `/mcp`, while `legacy_router` owns `/assistant/mcp`;
- the application composition root mounts both routers;
- `legacy_router`'s `/servers` delegates to `list_mcp_servers`, which calls `_authorize_read` before repository access;
- `legacy_router`'s `/tools` calls `_authorize_read` directly;
- `_authorize_read` requires tenant identity and `GATEWAY_MCP_READ`.

The attacker modeled by the test remains an authenticated tenant user without MCP-read permission. The repair makes the request reach the real compatibility endpoint and prove denial at the authorization boundary instead of accidentally proving only that a route is absent.

### Frozen configuration snapshots

`tests/services/assistant/test_agent_loop_golden.py` adds the fields named by the failed gate to the expected frozen sets; it does not remove or relax the exact removed/added set assertions. The definitions and propagation paths confirm the additions are intentional:

- `AssistantConfig.kb_retrieval_configs` exists and is populated by the Agent runtime HTTP configuration builder;
- `AssistantService` passes `kb_retrieval_configs` and `kb_include_images` into `AgentLoopConfig`;
- `AgentLoopConfig` declares and serializes both fields.

Therefore the snapshot now matches an already-wired public/internal contract rather than hiding an unconnected field.

### AS-02 offline golden artifact

`reports/agent-studio/as-02-golden-results.json` replaces the stale normal-response evidence node with the existing `test_non_stream_chat_returns_turn_contract_and_trace_metadata`. That test proves a successful terminal envelope, non-stream context snapshot, trace persistence, and secret redaction; the companion existing evidence node still proves `run_started`, `text_delta`, `streaming_first_completed`, and `run_finished`.

The artifact validator remains strict: it still requires the exact six case IDs, `offline-deterministic`, zero provider calls, every case passed, non-empty evidence lists, and every referenced test definition to exist. No assertion or required case was removed.

## Independent Verification

| Check | Actual result | Skip status |
|---|---|---|
| `uv run pytest -q --no-cov tests/integration/test_gateway_boot.py tests/security/test_management_api_authorization.py` | **14 passed**, 0 failed | **0 skipped** |
| `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | **45 passed**, 0 failed | **0 skipped** |
| `uv run ruff check tests/security/test_management_api_authorization.py tests/services/assistant/test_agent_loop_golden.py` | passed | n/a |
| Golden JSON schema/mode/provider/case/status `jq -e` check | passed | n/a |

The pytest runs emitted one Starlette/httpx deprecation warning and two existing `AsyncMock` coroutine warnings in the gateway gate, plus one Starlette/httpx warning in the Assistant gate. They did not skip tests or affect the repaired contracts.

## Inspected File Hashes

SHA-256 values at review time:

| File | SHA-256 |
|---|---|
| `tests/security/test_management_api_authorization.py` | `d3bfdca67061c197e79a49ce5580b4cbb05e7cefa21ba1481d636000485fb943` |
| `tests/services/assistant/test_agent_loop_golden.py` | `7e84d52871511c280d61342bbf45bd03668dc6da6fd10eaf3a0585b7aeaa6c80` |
| `reports/agent-studio/as-02-golden-results.json` | `def651b749869548573e1bfa6728c37ef5904c138b94761d47b1fc559f04a7ee` |
| `src/api/v1/mcp.py` | `248994cb0d9fa6b3dc3a2cd4a5dd01c63c589e99cefde99dccfc35f919714080` |
| `src/api/router.py` | `1019191d51ec76df6ef0a1d788ba38057ce45ac25e560e85cc9978565a811ba9` |
| `apps/assistant-service/src/assistant_service/api/routes/mcp.py` | `6a9aa8488c00edb5991668865cd4d4882d783c40dd91a44ace9e624ba0519636` |
| `apps/assistant-service/src/assistant_service/api/routes/chat.py` | `d573bc7caab3c94511d69445123f9c75fd461deef0a2d67ef17addf83f54b8fa` |
| `apps/assistant-service/src/assistant_service/core/assistant_service.py` | `0d8e1e7303380f13f86bbc86648f8b4b16b395a6897a0df07503c5ecebbc6630` |
| `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py` | `2b62323a56acff90f3d495a4d789d28e8aa5edb4fb5ea062caebee9b270da661` |
| `apps/assistant-service/src/assistant_service/core/mcp/config.py` | `c9ac819fbd8971a0d423aa9cdeb9e5f1f4fff050b10481f7c1f4d6276ad7c86b` |
| `tests/services/assistant/test_agent_trace_capture.py` | `1b3e726b23472ba6970a1f9e81dbc53bfaada2fee1633e7dec4de1849cc1ce85` |
| `tests/services/assistant/test_agentloop_streaming_first_contract.py` | `fd83b6ba84d2ba99d7d4fc6d52bdd350358d8421cb2854fa50251b4295a059fa` |

## Release-Gate Decision

The repair is minimal, preserves authorization and golden-contract strictness, and closes both exact failed gates with 59/59 tests passing and zero skips. The owning phases may return to AS-09. AS-09 remains blocked from a terminal approval until the full post-repair `make verify-agent-studio` run passes all 39 required gates on one stable source hash.
