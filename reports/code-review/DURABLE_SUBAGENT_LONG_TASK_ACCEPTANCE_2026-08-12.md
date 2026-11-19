# Durable Sub-agent Long-task Acceptance Contract

Date: 2026-08-12  
Status: production capability **not implemented / not accepted**  
Scope: 30–120 minute legal, finance, research, and engineering delegations

## Evidence boundary

`tests/services/assistant/durable_subagent_harness.py` and
`test_durable_subagent_protocol_oracle.py` are a deterministic test-only oracle.
They prove that the target state machine and failure scenarios are internally
testable without real sleeps; they do **not** prove that the Assistant uses a
durable store or background worker. Production remains synchronous and
process-local until the vertical slice below is wired and its black-box tests
pass against the real persistence adapter.

Do not use the oracle's 16 passing cases, migration/schema presence, a Protocol
type, or an in-memory registry as release evidence.

## Confirmed current-state gaps

The active runtime in `subagent_dispatch_runtime.py` explicitly describes
itself as process-local and non-durable. It cannot survive worker/process
restart, detach execution from SSE, or enforce cross-process idempotency and
concurrency. A draft PostgreSQL store, migration, and worker contract explored
the boundary during this review but were withdrawn because they did not close a
working vertical slice. In particular, the draft exposed these integration
failures:

1. Store and worker protocols did not share one callable API or composition-root
   adapter.
2. A durable control advanced the row version, but polling did not return the
   refreshed version needed for the worker's next acknowledgement/heartbeat.
3. Cancelling queued work made it unclaimable without atomically producing a
   cancelled terminal receipt.
4. A repeated exact control acknowledgement was not idempotent.
5. The terminal outbox event carried only a status and digest, so cursor replay
   alone could not reconstruct the terminal result/artifact receipt.
6. `blocked` recovery state and terminal-commit semantics disagreed.
7. No store-wide tenant/session concurrency reservation, token/USD reservation
   ledger, result spill implementation, or transitive child usage aggregation
   was wired.
8. No process-scoped worker lifecycle, submission path, reconnect path, or
   ordinary Docker runtime wiring existed.

With those gaps, keeping the draft would increase code size without making a
real 30–120 minute task more durable. Withdrawal was the correct outcome.

## Minimum vertical slice

Implement one path end-to-end before adding optional abstractions:

```text
spawn_subagent(background=true)
  -> durable_store.create_or_reuse(delegation + immutable child payloads)
  -> atomic budget + tenant/session slot reservation
  -> worker.claim_next() returns Lease
  -> worker rebuilds host-bound authority and invokes SubAgentManager
  -> append_event()/heartbeat()/poll_controls() with one fenced row version
  -> complete_terminal() atomically writes terminal + event + delivery outbox
  -> SSE/API reads events strictly after cursor, independently of worker
```

The common types must be implemented once, not duplicated between worker and
store:

```python
Lease(
    scope, attempt_id, claim_token, lease_epoch, row_version,
    event_cursor, control_cursor, immutable_payload,
)

MutationResult(
    row_version, event_cursor, control_cursor, idempotent,
)

TerminalCommit(
    row_version, terminal_cursor, terminal_digest,
    terminal_payload_or_artifact_ref, delivery_outbox_id, idempotent,
)
```

Every mutation is scoped by tenant/user/session/parent-run/attempt and fenced by
claim token, lease epoch, and expected row version. Exact retries return the
existing result even if the caller's old version is now stale; changed content
under the same stable ID conflicts. Control polling must return the current row
version (or an equivalent refreshed lease), because the control writer and
worker race legitimately.

### Recovery rule

An expired lease is restartable only if no tool start exists, or every tool
start is backed by a host-trusted tool-definition digest proving a read
operation. Never trust a model/task's `read_only` claim. Any write, unknown, or
missing evidence becomes a durable `blocked` recovery checkpoint. A recovery
checkpoint is **not** a completion terminal and must not enter the terminal
delivery outbox; it preserves the pause/read-back facts while the workflow can
still be resolved or cancelled. The terminal CAS remains exclusively for
completed/failed/cancelled outcomes. Read-back that
proves absence may authorize a **new attempt/epoch**; it never revives the stale
epoch. A committed terminal is delivered, not re-executed.

### Cancel and steer

- Queued cancel: atomically terminalize as cancelled and emit the terminal
  outbox event.
- Running cancel: persist idempotent control; worker acknowledgement means only
  "observed"; terminal cancelled is a separate CAS.
- Steer: current manager has no safe live provider-stream injection boundary.
  Persist it as `accepted_for_followup`, cooperatively cancel at a safe boundary,
  and create a successor turn. Unknown write recovery blocks steering.
- Client/SSE disconnect: detach only. It never creates a cancel control.

## Production-bound red acceptance checklist

These must be implemented as black-box tests against the real persistence
adapter and worker composition. They are acceptance requirements, not optional
unit coverage:

| Gate | Required proof |
|---|---|
| Atomic claim | Two independent connections/workers race on one delegation; exactly one lease wins. |
| Restart/read-only | Kill after a host-attested read start; advance a controllable database clock past lease; second worker claims a new epoch and stale worker is fenced. |
| No write replay | Kill after write/unknown or unauthenticated tool start; no worker can reclaim it; durable read-back/manual recovery receipt is visible. |
| Heartbeat CAS | Heartbeats extend lease and advance version; stale heartbeat/terminal cannot overwrite a later worker. |
| Event idempotency | Same event ID+digest replays exactly; same ID+different digest conflicts, including after restart. |
| Terminal exactly once | Two terminal writers race; one truth, one terminal event, one completion outbox, one budget settlement. Exact retry is idempotent. |
| Terminal replay | Crash after terminal transaction but before SSE send; reconnect from prior cursor reconstructs the complete terminal payload or verifiable artifact ref without model re-execution. |
| Disconnect | Close the HTTP/SSE consumer; worker remains leased and can complete. |
| Cancel | Queued cancel terminalizes; running cancel survives restart; ACK is not a fake terminal; exact control/ACK retries are idempotent. |
| Steer | Steer survives restart, is tenant-scoped, never mutates the active provider prompt, and produces a successor only after safe cancellation. |
| Distributed limits | Separate processes cannot exceed tenant or session active-child limits. Reservation release is crash-safe. |
| Budget | Token and USD ceiling reserve is atomic with claim; settle is exactly once; concurrent attempts cannot overbook; unknown usage settles conservatively. |
| Usage tree | Parent receipt includes each child/grandchild once, with prompt/completion tokens and USD; retries cannot double count. |
| Artifact spill | Oversized result is persisted outside row/event bounds with tenant-scoped ID, SHA-256, size and media type; inline partial result never masquerades as complete. |
| Long clock | A 30, 60, and 120 minute scenario runs under a controllable clock in seconds, exercising heartbeat, lease expiry, restart, controls and replay. |
| Live wiring | Ordinary Docker compose (no Sandbox) exposes submission/reconnect; kill and recreate assistant worker; the same delegation completes or blocks according to the above rules. |

Release must fail while any row above lacks authoritative evidence. In
particular, LLM-as-judge quality >=92 cannot compensate for a failed durability,
idempotency, isolation, side-effect, or budget gate.

## Deterministic oracle coverage

The current test-only oracle covers:

- two-worker atomic claim;
- read-only lease takeover with fencing, and write/unknown/forged-read blocking;
- event and terminal exact-once CAS;
- terminal-committed/SSE-not-sent cursor replay;
- disconnect-as-detach;
- durable cancel and successor-turn steer semantics;
- two virtual hours of heartbeats without wall-clock sleep;
- tenant/session concurrency;
- token and USD reserve/settle;
- transitive child usage; and
- oversized, tenant-scoped artifact spill.

It is intentionally kept independent of provider, Docker, database, and current
production internals so it remains a stable protocol oracle while the minimal
vertical slice is built.
