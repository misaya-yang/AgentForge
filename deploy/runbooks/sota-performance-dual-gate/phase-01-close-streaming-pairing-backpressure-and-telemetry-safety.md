# Phase 01 - Close streaming pairing, backpressure, and telemetry safety

- PHASE_ID: SPD-01
- FEATURE_ID: SPD-F002
- DEPENDS_ON: SPD-00

## Outcome

All provider transcripts are mechanically paired before network I/O, SSE applies bounded
backpressure, and asynchronous telemetry has bounded resource ownership.

## Scope

In:

- Provider request builders, SSE heartbeat transport, stop/repair state, and telemetry queues.

Out:

- Model routing, retrieval tuning, Web bundle work, or multi-worker topology.

## Done when

- [ ] Unpaired, orphan, and duplicate tool exchanges make zero outbound HTTP calls.
- [ ] Slow clients permit producer-ahead of at most one and disconnect releases each resource once.
- [ ] Stop closes the full published tool batch and preserves unknown side effects.
- [ ] Telemetry queue depth and drops are bounded and observable.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Pairing and stop | `pytest` provider-boundary, streaming-first, task-cancel, and Responses tests | Cross-provider pairing and stop invariants hold. |
| Backpressure | `pytest tests/proxy/test_sse_heartbeat.py` | Producer-ahead and cleanup are bounded. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Preserve every staged Grok tool-block and usage-accounting regression test.
