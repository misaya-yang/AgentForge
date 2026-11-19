# AS-02 Runtime Resolver and Isolation Plan

- **Phase:** AS-02 - Runtime Resolver and Isolation
- **Feature Oracle:** AS-F003 only
- **Status:** approved contract mapped to current code; implementation pending
- **Date:** 2026-07-18
- **Scope rule:** execute the existing Phase and architecture contracts without replanning, shrinking, or entering AS-03/AS-04 work

## Dependency and Baseline

- AS-01/AS-F002 is `passing`; its Actor and independent Critic each obtained migration `9 passed`, API/RBAC `13 passed`, Gateway regression `14 passed` and clean Ruff with zero skips.
- The strict AS-01 completion gate exited zero with quality score 100 after the final continuity writeback.
- The current repository-owned eight-service Compose runtime is healthy at roughly 720 MiB and is authorized for local database/runtime validation below the 3.5 GiB stop line. API Key changes and live-provider calls remain excluded.
- The dirty worktree contains inherited AS-00/AS-01, user-owned, and separately authorized open-source packaging changes; AS-02 edits remain confined to the Phase paths below.

## Current-Code Observations

1. Gateway `/assistant/chat` and `/assistant/chat/stream` currently authorize model/session inputs and then proxy the client body. `AssistantChatRequest` and Assistant Service `ChatRequest` accept client model, system prompt, KB and runtime controls, which remain valid for the legacy built-in Assistant but cannot be trusted as Agent configuration.
2. Shared internal auth already provides HMAC v2 method/path/query/body binding and an atomic Redis `SET NX PX` replay store. AS-02 will reuse these primitives while adding the distinct canonical Agent Runtime Snapshot/Envelope contract; ordinary internal-auth proof alone is not an Agent resolver.
3. AS-01 persists tenant-safe Drafts, immutable Versions/bindings and Publications, but `DatabaseAgentRepository` has no dedicated runtime-resolution query. Runtime resolution must authorize tenant/caller before loading Draft/Version/Publication data and must return one server-authored closed Snapshot.
4. `AssistantConfig` and `AgentLoopConfig` already expose the AS-00 `CapabilityAllowlist`, but the chat composition path does not populate it from a verified Agent Envelope. Existing streaming prompt construction deliberately demotes generic client `system_prompt`; AS-02 must add a distinct trusted immutable Agent-instruction layer without weakening that protection.
5. Sessions, `assistant_runs`, `assistant_run_checkpoints`, and `agent_traces` currently lack explicit Agent/Version/Publication/channel/runtime-fingerprint columns. `AssistantTraceContext` and trace writes likewise carry only tenant/user/session/run/model dimensions.

## Requirement-to-Change Map

| Contract | Bounded implementation | Primary evidence |
| --- | --- | --- |
| R1 Gateway-only authorized resolver | Add a shared `ai_gateway_core.agents` Runtime Snapshot/Envelope model, deterministic canonical JSON/hash and HMAC signer/verifier; add tenant/ACL-safe preview/version/publication resolver methods; add closed Preview and Published Gateway schemas/routes; reject reserved Agent fields and `X-Agent-*` forgery on generic Assistant routes. | `tests/api/test_agent_runtime_envelope.py` plus live/internal API smoke |
| R1 freshness and replay | Bind tenant, caller, Agent, Version/Draft revision, Publication/channel, session, normalized external body hash, canonical Snapshot/spec hashes, issuer, issued/expiry time and nonce. Assistant recalculates hashes and atomically consumes a tenant/issuer-scoped nonce through the existing replay-store contract; store failure denies execution. | signature mutation, expiry, replay, body/session substitution and unavailable-store tests |
| R2 prompt/capability boundary | Map only verified Snapshot model/parameters/instructions/Knowledge/memory and effective tool names into internal `AssistantConfig`; add explicit trusted Agent instructions and channel/capability policy layers before memory/history/external data; create an explicit `CapabilityAllowlist`, including empty, and preserve the legacy `None` path. | resolver/isolation tests, no-tool golden case, prompt-injection case |
| R3 pinning and evidence | Add forward-only migration `072_agent_runtime_dimensions.sql` with nullable explicit dimensions and tenant/composite references where applicable for sessions, runs, checkpoints and traces. Bind new Agent sessions once; reject a different tenant/Agent/Version/channel on reuse or resume; propagate dimensions/fingerprints through run/checkpoint/trace writes and indexes. | migration-backed trace/session tests and queryable trace assertions |
| R3 publication behavior | Resolve a new Published session from the Publication's current immutable Version, then keep that session pinned even if the pointer changes; return a stable fail-closed error when its pinned Version/Publication becomes unavailable. AS-02 consumes Publication rows but does not implement AS-06 promotion APIs. | two-version pinning/revocation fixtures |
| R4 compatibility | Keep existing `/assistant` schemas/SSE events and `__builtin_assistant__` semantics. Agent-only fields are carried only by the internal verified route/context; Agent feature disablement falls back exclusively to the legacy no-Agent/`None` allowlist path. | existing golden/message/runtime/isolation gates |

## Planned Files and Minimal Boundary

### Shared runtime contract

- `packages/ai-gateway-core/src/ai_gateway_core/agents/__init__.py`
- `packages/ai-gateway-core/src/ai_gateway_core/agents/runtime.py`
- targeted extensions to `persistence/repositories/agent_repository.py`, `session_repository.py`, and session models/manager only where explicit pinning requires them

### Gateway resolver surface

- `src/api/schemas/agent_runtime.py`
- `src/api/v1/agent_runtime.py`
- `src/api/v1/assistant.py` and `_assistant_proxy.py` for reserved-field/header rejection and server-authored internal forwarding
- `src/api/router.py` for the additive route registration

### Assistant verification and execution

- `apps/assistant-service/src/assistant_service/api/routes/chat.py`
- `apps/assistant-service/src/assistant_service/core/assistant_service.py`
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `apps/assistant-service/src/assistant_service/core/tool_invoker.py` only if a verified-context propagation gap remains
- `apps/assistant-service/src/assistant_service/core/trace_writer.py`

### Persistence and evidence

- `database/migrations/072_agent_runtime_dimensions.sql`
- the four Phase-owned test files, existing compatibility tests named by the Phase, `reports/agent-studio/as-02-golden-results.json`, the Actor report and required Harness writebacks

No MCP registry/auth, Connector adapter, Skill persistence, Knowledge retrieval implementation, Web UI, Publication promotion, public Hosted/Embed page, deployment, provider key, commit, or push is in this Phase.

## Execution Sequence

1. Write failing contract tests for closed external schemas, Gateway-only resolution, canonical hashing/signing, Assistant verification, atomic replay rejection and generic-route forgery rejection.
2. Implement the shared closed Snapshot/Envelope types and signer/verifier using deterministic canonical JSON, constant-time comparison, bounded time window and the existing atomic replay-store protocol.
3. Add tenant/ACL-first runtime resolver repository methods and Preview/Published Gateway routes; forward only the server-authored internal request and preserve the public SSE body.
4. Verify the Envelope before constructing Agent execution config; enforce trusted prompt layers, explicit non-expanding allowlist, repeated fail-closed resource checks and legacy `None` compatibility.
5. Apply the additive explicit-dimension migration and propagate immutable session pin plus Agent runtime dimensions/fingerprints through sessions, runs, checkpoints and traces.
6. Complete isolation, pinning, revocation, golden, trace-redaction and legacy compatibility fixtures; produce the golden JSON without a live provider.
7. Run every required command exactly, record initial failures and final reruns, write the Actor report/state, freeze the diff, and request a fresh independent Critic. Keep AS-F003 `failing` until that approval and the strict AS-02 completion gate.

## Validation Evidence Gates

| Gate | Exact command | Required outcome |
| --- | --- | --- |
| Gateway Envelope | `uv run pytest -q --no-cov tests/api/test_agent_runtime_envelope.py` | Closed schemas plus signature/hash/time/nonce/body/session/Snapshot forgery and replay cases pass with no skips. |
| Resolver isolation | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py tests/services/assistant/test_agent_capability_allowlist.py` | Verified mapping, prompt/capability isolation, fail-closed resource checks and generic-Assistant rejection pass. |
| Trace/session | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_message_persistence.py tests/services/assistant/test_agent_loop_golden.py` | Explicit dimensions, immutable pinning, checkpoint/idempotency isolation, trace redaction and existing golden behavior pass. |
| Runtime regression | `make verify-assistant-runtime-dev && make test-isolation` | Both existing runtime and live service-isolation gates pass without required skips. |
| Lint | `uv run ruff check packages/ai-gateway-core/src/ai_gateway_core/agents src/api/v1/assistant.py src/api/v1/_assistant_proxy.py src/api/v1/agent_runtime.py src/api/schemas/agent_runtime.py apps/assistant-service/src/assistant_service/core apps/assistant-service/src/assistant_service/api/routes/chat.py tests/api/test_agent_runtime_envelope.py tests/services/assistant/test_agent_runtime_resolver.py tests/services/assistant/test_agent_runtime_isolation.py` | Ruff exits zero on the exact Phase paths. |

Supplemental gates are the migration contract against isolated PostgreSQL, full OpenAPI inspection, SSE field-leak check, queryable trace SQL, Compose ownership/memory sample, `git diff --check`, structural Harness validation and a fresh independent Critic. No browser route or live provider call is applicable.

## Rollback and Stop Conditions

- Runtime application rollback disables the additive Agent runtime routes/feature flag and preserves the built-in Assistant `None`-allowlist path.
- Migration rollback is forward-only: new nullable dimensions remain, existing `__builtin_assistant__` rows stay null, and captured evidence is not deleted.
- Stop and record a blocker if the implementation would trust a client Snapshot/capability/prompt, authorize by UUID without tenant/ACL, consume nonce non-atomically, fail open when policy/replay storage is unknown, hot-swap an existing session Version, leak protected prompt/Snapshot data, break legacy Assistant/SSE behavior, exceed the memory ceiling, or require a forbidden downstream Phase change.

