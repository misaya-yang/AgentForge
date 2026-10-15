# Phase 02 - Optimize Gateway Assistant and Knowledge hot paths

- PHASE_ID: SPD-02
- FEATURE_ID: SPD-F003
- DEPENDS_ON: SPD-01

## Outcome

Confirmed local hot paths remove redundant PG/Redis/Qdrant/provider work and event-loop
blocking while retaining existing request and state semantics.

## Scope

In:

- Gateway policy loading/Lua/trace threshold, Assistant read-only batches/MCP/memory,
  and Knowledge retrieval/embedding/pool/runtime-role paths.

Out:

- Assistant multi-worker, semantic response caching, Qdrant quantization/sharding, or session-message migration.

## Done when

- [ ] 100 concurrent cold rate-policy resolves use one PG read; warm resolves use zero.
- [ ] Two 250 ms read-only tools finish within 1.2x of one; writes/unknown remain serialized.
- [ ] Twenty same-connection MCP calls initialize and resolve DNS once inside TTL.
- [ ] Interactive retrieval has no redundant collection ping, respects its deadline, and ingestion does not starve retrieval.
- [ ] Runtime role and pool configuration preserve `all` compatibility and isolate the local worker.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Gateway | Focused rate-policy/Lua/usage tests and counted fake PG/Redis probes | Request-path round trips meet the gate. |
| Assistant | Focused tool-loop/MCP/memory/trace tests | Parallelism, reuse, order, and pairing remain correct. |
| Knowledge | `make rag-eval-regression-gate` plus focused retrieval/embedding/worker tests | Latency changes preserve retrieval quality and ingestion state. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- A structural scale change requires a measured trigger and a separate migration review.
