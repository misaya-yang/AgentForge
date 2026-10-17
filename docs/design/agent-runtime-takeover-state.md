# Runtime takeover state (U-RT-02)

Implementation inventory, 2026-10-17; execution is unverified. This document declares storage
and owner-affinity rules; it does not establish two-instance acceptance. The
single in-process Codex App Server remains the managed execution kernel.

| State | Owner / durable authority | Takeover rule |
| --- | --- | --- |
| Native thread history, projections and V1 event cursor | Shared PostgreSQL Item Store | Reload original scoped thread; immutable event keys and root-monotonic cursor prevent a new receipt on exact replay. Never infer success from legacy `completed`. |
| Execution claims and local `execution_claims` cache | Shared `assistant_runtime_execution_owners`; cache belongs to instance | Claim only after expiry with higher fence. Every model/tool/write checks current owner/fence. Never copy a former process's cache as authority. |
| Recovery `StartTurnRequest` | Execution owner's durable `context` | Restore exact original run, snapshot, capability revision and input; do not create another run. Validate scope against persisted thread identity. |
| Read-only payload and local `readonly_by_turn` bindings | Shared snapshot `readonly_capabilities`; local cache is owner-affine | Dispatch always reloads scoped snapshot/model lease. Require original run's snapshot, thread and tenant/user/session match, unexpired snapshot and lease, and no revocation. TTL eviction/restart loses only cache and trace enrichment, never authority or payload. |
| Cancellation intent and `turn_cancellations` | `cancel_runtime_execution` commits cancellation/revocation and receipt; local token is owner-affine | Persist first, then notify local kernel. Restart never resumes a cancelled run; old in-flight owners fail durable fence/authority checks. Cross-instance eager token delivery belongs to U-RT-03. |
| Approval decisions and `ApprovalBroker.pending` waiters | Shared approval rows / deduplicated decision facts; waiters belong to owner | Rebuild original invocation and consult durable approval, respecting exact run/tool/scope. Never copy oneshot senders; cancelled/expired approval cannot revive authority. |
| Dynamic invocation journal, lifecycle/tool receipts | Shared invocation and immutable lifecycle rows | Reconcile exact original call and persisted output. Dispatched writes without conclusive receipt become `side_effect_unknown`; never blindly replay an unknown write. |
| `thread_gates` | Owner-affine mutexes; database root locks/owner fences remain authority | Recreate empty. A local mutex provides no cross-instance exclusion. Idle map-only references are now pruned on access and every 30 seconds; active holders/waiters retain their original mutex. |
| `recovering_threads`, recovery notify and shutdown tokens | Owner-affine scheduling hints | Recreate empty and rescan durable candidates; duplicates lose database claim/fence. A wakeup is never execution authority. |
| Broadcast sender, SSE subscriptions | Subscriber-owned per-thread delivery; durable V1 sequence is shared | Last subscriber drops its hub. One process-level LISTEN connection wakes subscribed threads only; every wake replays PostgreSQL, with a 7-second backstop and standard Last-Event-ID fallback. Broadcast/NOTIFY loss cannot erase a terminal. Source implemented; live cross-instance acceptance remains unrun. |
| `TextProjectionState` deduplication | Owner-affine per-kernel stream cache | Recreate; immutable item/event IDs remain the persistence deduplication boundary. Local text state cannot authorize another turn. |
| `pending_metadata` | Owner-affine pre-persist thread metadata staging | An unacknowledged staged create is not a durable thread. Resume only persisted history/root identity; never recover a staged map entry as an accepted create. |
| `staged_startup` | Owner-affine startup configuration staging | Rebuild from persisted snapshot/resume config and current tool policy; never infer accepted startup from memory. |
| `write_failures` poison set | Owner-affine fail-closed quarantine | Stop writes for poisoned thread in that process. A new process must reload persisted facts and obtain fresh authority; clearing memory is not evidence that an uncertain write failed. |
| `terminal_identity_cache` | Owner-affine lookup cache; persisted member/run scope is shared | Reconstruct from scoped persisted identity. Cache cannot grant access or change terminal identity. |
| Internal trace context, reqwest connection pools, readiness flag | Owner-affine observability/transport/lifecycle | Recreate clients/readiness; validate new transport headers. Lost trace enrichment does not grant capability authority. |

No new SQL is required for read-only payloads or cancellation intent: those
facts already have durable owners. This implementation closes missing original
snapshot identity/expiry checks in read-only dispatch. It does not turn local
cancellation tokens, gates, or waiter maps into a distributed control plane.

## Real terminal crash matrix (U-RT-01)

The existing Stage A canonical vocabulary and Stage C atomic
`commit_recovery_terminal` are retained. Debug builds now accept an operator-owned
`$AI_PLATFORM_AGENT_HOME/terminal-acceptance-fault.json` containing exactly a known
`stage` and the target UUID `run_id`. Configuration is one-shot renamed to
`terminal-acceptance-consumed.json`; `terminal-acceptance-hit.json` signals the
boundary. No public API arms it, and release builds ignore it. Remove the hit
file to continue, or kill the selected Runtime process to exercise recovery.
Never kill unrelated containers. A pause expires after 300 seconds.

| Stage | Required observation after kill/restart |
| --- | --- |
| `terminal_before_transaction` | No terminal transition/receipt; expired-owner recovery eventually commits original outcome once. |
| `terminal_after_row_update` | Transaction rolls back ledger, lifecycle admission, approval changes and cursor; retry commits one correct terminal. |
| `terminal_after_event` | Row and receipt/cursor roll back together; no observer sees an uncommitted event. |
| `terminal_after_commit` | Exactly one canonical terminal row plus original immutable envelope/cursor remains; SSE backlog delivers it despite absent broadcast. |

Run each applicable boundary for succeeded/failed/cancelled Core tails and
expired-authority closure; also retain stale-owner, cancellation-wins,
revocation, duplicate retry, unknown-write and tenant-scope cases. The final
fence check may intentionally reject a paused owned transaction after lease
expiry, leaving recovery to the next valid claim. Record process/image/source
identity and actual database/SSE observations. Unit selection/release tests do
not substitute for this matrix, real Worker/provider journeys or U-RT-03 chaos.

## Executable opt-in matrix

The existing native `runtime_durable_recovery_gate` now accepts:

```sh
ENV_FILE=/absolute/path/to/selected/.env.dev \
  /absolute/path/to/runtime_durable_recovery_gate \
  --terminal-matrix --allow-runtime-sigkill \
  --evidence-dir /absolute/external/cache/terminal-matrix
```

Build that existing native binary through the primary writer's serialized native
workflow. Run from the repository root with the selected existing development
stack and dedicated ignored E2E account. There is no new Make/Cargo target or
library wiring. The helper honors `ENV_FILE`; its existing `.env` default remains
for older callers. The agent writing this change did not read either private file.

The explicit command performs ten real cases serially: all four boundaries plus
normal commit, for both a real persisted Core success and real snapshot revocation
leading to authority-expired failure. A `terminal_core_saved` debug bootstrap
pause catches the real `TurnCompleted` notification before V1 projection; the
harness verifies persisted Core completion and an active ledger, then kills and
restarts that exact existing container. It never resets terminal rows or inserts
fabricated Core outcomes. Each later boundary kill exercises the actual recovery
terminal transaction. Owned recovery must acquire a newer fence; its original
execution/attempt/dispatch-fence, one Core call/output and one quiz effect must
remain unchanged. Revoked runs must have zero dispatches and zero quiz effects.
Duplicate recovery must preserve the exact durable cursor, timestamp and envelope.

At `terminal_after_commit`, the process stays paused before broadcast while the
harness consumes public V2 SSE from one cursor before the committed terminal.
It verifies original run/status/cursor/timestamp, then kills/restarts and verifies
replay again. Uncommitted boundaries must expose neither ledger terminal nor
terminal envelope. A PASS summary is written only after all ten cases; failure
records completed and unrun counts and invalidates any previous PASS. Raw errors,
credentials and container environment values are never included in receipts.

Each kill rechecks the exact dev project/worktree labels and container ID and
refuses unrelated active Runtime runs. No Worker kill, builds, Compose recreation,
volume removal, migration activation or publication occurs. Cases keep durable
evidence and clean up their one-shot pause files. Release binaries ignore all
hooks, so a release-only image will fail by never reaching a requested marker.

The harness's negative unit regressions cover duplicate/wrong-run/wrong-outcome
terminal rows and V2 replay cursor/timestamp identity. Runtime tests cover exact
run selection and one-shot filesystem pause/release. These are implemented but
have not run here. Full cancelled-Core-tail, stale-owner two-instance chaos,
unknown-write and provider/Worker product acceptance retain their original gates;
this ten-case matrix does not claim those results.
