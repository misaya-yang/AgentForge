# Runtime event fan-out v2 (U-RT-04)

Implemented source, 2026-10-17. No compilation, database test, two-instance run or
product acceptance has run for this change. Thread ownership/routing remains
U-RT-03; these wake hints do not enable a second managed kernel or remove the
single-instance release constraint.

## Durable ordering and scoped delivery

Runtime SSE now subscribes to a per-root-thread hub before its initial Item Store
read. The first subscriber creates its bounded 256-entry channel; the last
subscriber's Drop removes it under the same map lock. Producers never allocate a
channel for an unsubscribed thread. One busy thread therefore cannot overrun
another thread's queue, and historical thread count does not retain channels.

Both local broadcast and PostgreSQL notifications are wake hints. A durable
broadcast never advances the SSE cursor directly. Each wake revalidates the
original tenant/user/session/root identity, reads the ordered V1 backlog and
immediately drains all full pages. This closes the old risk of jumping over a
missed earlier event when a later broadcast arrived. Root sequence gaps are legal:
non-V1 rollout/lifecycle items use the same root cursor. No adjacent-integer rule,
new reset envelope or fabricated missing event is introduced. Duplicate hints
and broadcast lag trigger the same persisted cursor replay; a read/visibility
failure closes the stream as before.

The existing non-durable terminal admission failure still reaches a connected
subscriber during storage failure. Its `i64::MAX` sentinel is not adopted as a
durable cursor; the error closes that stream. It does not become a committed
terminal, grant execution authority or fabricate a reset/success.

The Runtime private SSE endpoint accepts standard `Last-Event-ID` when
`after_sequence` is absent. An explicit query (including zero) overrides the
header. Malformed, overflowing or duplicate header values are rejected when used;
negative query cursors and limits outside 1..1000 are rejected. This changes no
public event envelope. The separate Gateway public V2 route currently supplies
its own explicit query cursor; standard-header propagation at that public edge
belongs to the Gateway owner and is not claimed by this Runtime-only change.

## PostgreSQL wakeups and gate lifetime

The shared production V1 append helper queues
`pg_catalog.pg_notify('agent_runtime_events_v2', hint)` inside its existing
transaction. The hint contains only root UUID and sequence, no tenant data,
prompt, tool output or credentials. PostgreSQL delivers it only after commit.
Rollback or a lost execution fence before commit cannot publish a hint. The
cancellation route's direct append now calls that same notification helper in its
existing transaction. No terminal transaction implementation, migration, function
grant, startup admission or frozen SQL changed. A failure to enqueue a notification
fails the transaction; it cannot commit an event and pretend notification success.
The actual Runtime role's `pg_notify` execution contract must be tested.

One LISTEN task uses a dedicated single-connection pool with the same scoped
connection options, rather than consuming a scarce application pool slot or one
connection per subscriber. Startup/reconnect wakes all existing hubs to replay
any lost hints. Malformed/oversized hints and unsubscribed UUIDs are ignored
without allocating state or querying that UUID's data. Notification UUIDs and
sequences are never membership, routing, lease or execution authority.

LISTEN outage/reconnect retains a 7-second durable SSE backstop (within the planned
5–10 seconds), replacing the previous 500 ms poll. The service cancellation token
stops and joins the listener task and releases its connection. Listener errors
log only a generic degradation message; connection secrets/options are not logged.
This is a notification bus, not a durable queue.

Thread gates remain local mutexes. Access-time and 30-second maintenance pruning
remove only map-owned Arcs (`strong_count == 1`). Returned gates, active owned
locks and waiting tasks all retain their Arc and therefore the same mutex.
Pruning cannot create a second lock while a holder/waiter exists. Cross-process
exclusion still comes from the existing database owner/fence/root locks.

## Implemented verification and execution commands

Pure Runtime tests cover independent-thread flooding/lag, final-subscriber
cleanup, channel closure when hub owners disappear, active/waiting gate pruning,
header/query precedence and invalid cursor/header forms, and bounded hint parsing.
The direct pinned Rustfmt parser accepted the changed modules; that is syntax
parsing only, not Rust type-checking or a test pass. `git diff --check` was run.
No Cargo or Docker was run by the implementation agent.

The explicit ignored PostgreSQL test
`real_postgres_notification_is_commit_scoped_and_rollback_suppressed` calls the
actual production notification helper under an unprivileged login ending in
`_runtime`, asserts no precommit/rolled-back hint, then receives the exact committed
root/sequence. Set `AI_PLATFORM_RUNTIME_NOTIFY_TEST_DATABASE_URL` privately and
run that named test with `--ignored`; a missing DSN or privileged role fails.
It proves notification helper behavior only, not the full append/HTTP path.

The explicit ignored Runtime test
`real_runtime_fanout_replays_header_drains_pages_and_wakes_both_instances` uses the
production store append plus actual HTTP/SSE routes. Primary supplies:

- `AI_PLATFORM_FANOUT_ALLOW_FIXTURE_APPEND=1`
- `AI_PLATFORM_FANOUT_RUNTIME_A_URL` and `AI_PLATFORM_FANOUT_RUNTIME_B_URL`
- `AI_PLATFORM_FANOUT_THREAD_ID`, `AI_PLATFORM_FANOUT_TENANT_ID`,
  `AI_PLATFORM_FANOUT_USER_ID`, `AI_PLATFORM_FANOUT_SESSION_ID`
- `AI_PLATFORM_FANOUT_INTERNAL_TOKEN`
- `AI_PLATFORM_RUNTIME_NOTIFY_TEST_DATABASE_URL` (actual scoped Runtime login)

Use an operator-owned **dedicated synthetic thread** with no active business run,
whose session/membership already exist; the test writes durable empty text probe
envelopes into that fixture. It does not manufacture a model turn or tool effect,
remove immutable records or mutate ordinary tenant threads. Probe data is test
fixture evidence only. It checks Last-Event-ID replay with page limit one, query
override without replaying earlier probes, live delivery to both endpoints in
under four seconds (before the seven-second backstop), identical persisted
sequence/envelope, and wrong-tenant 404. Local hub.send is deliberately absent:
those live deliveries require PostgreSQL wakeup. Primary must verify that the two
URLs represent distinct admitted processes/images; different URL text alone is
not process provenance.

Run both named tests explicitly with `--ignored --test-threads=1` in the existing
controlled Runtime test source workflow. Do not count them as passed merely
because the default library test run ignores them. The normal release assumption
still needs U-RT-03 owner/routing gates before two managed instances are admitted.
No extra dev Compose project, builder or volume is authorized by these tests.

Still required: actual scoped production-append commit/rollback, LISTEN disconnect
and reconnect/lost-NOTIFY tests, sustained-load lag, durable backstop with listener
unavailable, subscriber churn resource measurements, thread deletion/visibility
changes, existing terminal crash matrix and complete public product journeys.
This source change does not claim those gates, multi-instance acceptance or
zero duplicate tool executions from unrun evidence.
