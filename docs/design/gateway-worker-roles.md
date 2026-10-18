# Gateway process roles and singleton effects (U-GW-12)

`gateway --config-env` and `gateway --config-stdin` still default to `all`.
Add `--role api|worker|eval|all`, or set `GATEWAY_PROCESS_ROLE` in the resolved
configuration environment; the CLI wins when both are supplied. All four existing
config/check modes accept the role flag. Invalid or repeated flags fail closed.
Split API/worker roles require durable task delivery; `memory` remains supported
only by `all` because an in-process queue cannot cross a process boundary.

- `api` serves the existing HTTP/SSE graph and starts no singleton or task worker.
- `worker` serves only `/health`, `/health/live`, `/health/ready`, and the existing
  authenticated `/metrics`. Business routes and optional TLS callback/companion
  listeners are disabled in this process.
- `eval` keeps only the leased evaluation outbox worker and per-process local
  services. Select `control_plane.evaluation.placement="dedicated"` across the
  deployment so general workers do not compete for the judge lease. It uses the
  same health/metrics-only listener policy as `worker`.
- `all` retains the existing combined functions. Its singleton jobs now require
  the same database authority as a dedicated worker process.

Each process retains its own usage recorder, publisher, service cache and health
monitor. Usage buffered by API requests must flush in the process that owns it.
The general task worker remains distributed by the existing per-item durable
claim/fence protocol; it is filtered out of API roles, not globally serialized.
Provider-batch reconciliation follows that task protocol rather than adding a
second independent batch scheduler.

## Authority integration

Install [the SQL proposal](proposals/gateway-singleton-worker-leases.sql) through
a **new** native-authority ordered migration and its ACL/ownership inventory.
Do not modify frozen migration fingerprints, grant application table DML, or run
DDL during startup. The proposal has a gateway-only EXECUTE surface with fixed
search paths and no PUBLIC privileges. Prefix-render role names through the
existing native authority. API-only processes do not require this new surface;
all/worker startup fails closed if it is missing or inaccessible. Catalog/schema
admission and ordinary per-process initialization still precede listener startup.

The lease lasts 30 seconds, renews every 5 seconds on the independent control
pool, and has a conservative monotonic deadline measured before each database
call. Database acquisition/renewal and permit acquisition have 3-second budgets.
Effect permits use a separate lazy pool capped at 12 connections (zero minimum,
30-second idle timeout), so long-running permits cannot exhaust the business pool
needed by their own guarded SQL. Across the default singleton lanes at most 11
permits are simultaneously live; deployment connection sizing must account for
this bounded pool in addition to the existing business/control pools.
A lease can be acquired only after server expiry; every acquisition increments
its fence. Neither renewal nor a stale release can resurrect/expire a successor.
Rows are retained so fences never restart. Failed drains are terminal failures,
not permission to release and restart a job.

The supervisor starts a job only after acquisition. Contending replicas remain
healthy standby. Lost renewal revokes local authority, cancels current effect
scopes, stops the actual job, waits for retained effects, releases the old lease,
and then retries with bounded backoff. Startup/stop failures remain visible to
readiness and shutdown. Partial lifecycle startup runs canonical cleanup; a stop
failure does not skip later cleanup owners or turn a retry into false success.

## Real effects

| Singleton | Protected boundary |
| --- | --- |
| Evaluation outbox | Claim, judge/candidate invocation, heartbeat and terminal outcome per poll iteration |
| Usage scheduler | Both quota and daily-maintenance iterations; actual supervised SQL transactions |
| Scheduled agent tasks | Reconciliation, claim and dispatch tick, including durable dispatch markers |
| Image task worker | Durable claim, provider dispatch, artifacts and outcome per task |
| File cleanup | Scheduler invocation and retained blocking thread; local authority checked before every unlink/rmdir |
| SDK session cleanup | Durable deletion drain including remote acknowledgement |
| Trace retention | Cleanup pass and actual supervised PostgreSQL statements |
| Assistant trace reconciler | Reconciliation page and actual supervised PostgreSQL statements |
| Usage consumer | Group/reclaim, read, processing and ACK; publication remains per process |

Every effect scope obtains PostgreSQL `FOR KEY SHARE` on the current
`(job, owner, fence)` authority row. Acquisition explicitly takes `FOR UPDATE
NOWAIT`, which conflicts with effect permits; renewal updates only the non-key
expiry and remains compatible. A successor cannot enter while a predecessor's
permit drains. A local revoked/expired owner never polls a new effect future.

Owned PostgreSQL supervisors capture the effect before spawning, revalidate it
on the **actual statement transaction**, and hold the row lock through commit or
rollback even if their caller is cancelled. Blocking file threads capture the
permit themselves because dropping a `spawn_blocking` waiter cannot stop them.
Artifact uploads also retain an owned waiter through the storage adapter's hidden
blocking write; cancellation cannot release its permit while bytes are still being written.
Lease release waits for those retained permits. These hooks protect real effects,
not merely the scheduler's running flag.

Remote services keep their existing per-item idempotency/dispatch/uncertainty
protocols. A request already transmitted can complete after local cancellation;
U-GW-12 does not claim exactly-once execution at an external target lacking a
fence-aware endpoint. Database failover/clock faults and actual kill-9 behavior
still require the deployment acceptance suite below.

`gateway_worker_fence_rejections_total` is exported by authenticated metrics.
It counts rejected attempts rather than manufacturing a zero counter for
unobservable remote effects. A healthy non-faulted two-replica run should keep it
at zero. Fault tests deliberately exercising a stale owner may increment it.
Readiness includes singleton health, distinguishes standby from active image
ownership, and reports the process role; a failed singleton makes readiness fail.

## Verification

Run through the existing native container entry point (one shared cache/job):

```sh
scripts/new/native-container.sh test -p ai-platform-gateway-foundation --lib -- worker_lease
scripts/new/native-container.sh test -p ai-platform-gateway --lib -- worker_roles
scripts/new/native-container.sh test -p ai-platform-gateway-foundation --lib -- billing_scheduler
scripts/new/native-container.sh check -p ai-platform-gateway --bin gateway
```

The unit fixtures exercise role/route isolation, preserved local usage flush,
two supervisor replicas, takeover, renewal loss, expiry cancellation, stale owner
rejection before polling, retained-effect drain, startup failures and failed
shutdown. The explicit ignored PostgreSQL fixture
`worker_lease::tests::postgres_two_owners_expiry_effect_lock_and_stale_real_write`
requires `AI_PLATFORM_TEST_WORKER_LEASE_DSN` and rejects any database name not
beginning `test_`. It requires the authority-installed schema and tests real row
lock compatibility, server expiry, monotonic fences and rejected stale writes.
It is never silently counted as a unit-test pass.

Remaining deployment acceptance: native-authority migration/ACL negative matrix,
actual same-binary two-worker plus API processes, real provider/usage/file paths,
kill-9 and database connectivity loss during effects, and observed metric and
readiness behavior. Compilation and deterministic fixtures alone do not close
that acceptance gate.
