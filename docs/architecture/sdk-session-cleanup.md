# Durable SDK session deletion

Gateway owns admission evidence and deletion delivery, not SDK conversation state.
Canonical migration epoch 14 adds `gateway.sdk_session_barriers` and
`gateway.sdk_session_scopes`. The latter is both immutable scoped admission index
and idempotent deletion outbox. No service/session FK can cascade away its evidence.
Only the Gateway role has SELECT/INSERT/UPDATE; neither public users nor the SDK
worker have table access. No prompts, provider keys or transcripts are stored.

## Source and concurrency

The public SessionsApi and service dispatcher share `CoreServices.sessions`, a
DatabaseSessionManager backed by PgSessionStorage. Its production table is
`assistant.sessions`; this is the generic service-session table, distinct from
Runtime-owned Codex threads. Ownership uses exact authenticated tenant/user/session
and service identifiers, never editable metadata. Generic native and Python `GET /api/v1/sessions/{session_id}` responses expose the canonical nullable `service_id` after their existing ownership checks. Clients can validate service bindings without trusting metadata or list summaries; legacy null/empty bindings are returned unchanged. PATCH and list projections are unchanged. Admission validates expiry too. A null/empty service binding from public pre-create
is durably claimed under this lock; another nonempty service cannot be rebound.

Durable create/save, SDK admission and deletion use one lock order: Gateway
barrier first, canonical session row second, scope rows last. Existing ownership
is checked before barrier mutation and repeated on the canonical operation.
Create/save retain the fence through their existing supervised transaction; a
creation-conflict savepoint removes an uncommitted new barrier on a lost insert. Admission persists the exact scope before the
private HTTP run. Deletion checks durable core Runtime thread bindings while holding the owned
canonical session lock. Only a genuine core binding calls core cleanup; failure
rolls back without creating SDK deletion intent. SDK-only sessions do not depend
on core Runtime availability. After any required core cleanup succeeds, one
transaction commits canonical session deletion, the permanent barrier and pending
SDK jobs. Sessions with existing Runtime FK references retain a canonical tombstone;
unreferenced sessions are physically deleted so a concurrent first Runtime binding
blocked on the row lock fails its ownership FK. Unexpected other FK failures abort
the entire intent rather than silently fallback to an unsafe tombstone. The public route skips a second fallible manager deletion. Public owned
reads check the durable barrier to defeat stale session caches; local cache entries
are invalidated too. Explicit public recreation of a previously deleted owned
session ID is rejected atomically by the same persistent barrier inside storage
creation. The public pre-check is only a fast path. Concurrent create/delete
serializes at this fence; it cannot return a newly recreated row after deletion. A deletion with no SDK scopes
still blocks late admission but never contacts or requires the optional worker.
Changing/deleting a service or adapter cannot erase previously admitted scopes.

Worker cleanup uncertainty is a separate durable refusal. An active
`.cleanup/<scope>.json` marker blocks worker admission before receipt replay and
blocks deletion/retention even after a worker-process restart drops the OS lock.
Only the supervised local cleanup protocol may clear it; signal delivery does not
prove complete process-tree termination. Legacy pending runs without valid local
cleanup evidence also remain refused. Busy/cleanup-unknown responses leave this
outbox pending; no operator reconciliation or force-delete endpoint is introduced.
See the [worker shutdown contract](../deployment/sdk-agent-worker.md#execution-deadline-and-failure-ownership).

A request already admitted before the deletion barrier can race delivery. The
worker's permanent tombstone and exclusive scope lock are necessary: if run starts
first deletion returns busy and retries; if deletion wins the late run is rejected.
Do not enable either half without the worker lifecycle implementation.

## Delivery semantics and operations

Public DELETE acknowledges Gateway logical session deletion and a committed private-cleanup
intent. It does not attest to instantaneous physical erasure from worker storage,
backups or remote providers. The host-owned loop polls every 30 seconds, claims at
most 16 due rows with SKIP LOCKED and holds the per-row transaction across a bounded
10-second request. The private DELETE /v1/sessions has exactly five scope fields,
bearer service authentication and no provider/user credential. Only HTTP 200 with
`deleted:true` marks success; busy, unavailable, malformed and unknown outcomes
remain pending and retry. Abort/restart rolls the claim back. A tombstone makes
replay safe if HTTP succeeded before the database acknowledgement committed.

Shutdown cancels/awaits this loop before pool closure. Pending jobs survive restart.
When no jobs are pending, worker URL/token are not read. Invalid/missing operator
configuration leaves work pending; restoring the same worker origin and persistent
volume permits retry. Changing the worker origin/state volume is a migration and
must preserve old scopes/tombstones until erasure completes. No target URL comes
from service metadata. Retain barriers and receipts until a separate audited
retention policy can prove no session-ID reuse or pending cleanup. Operators can run the read-only bounded diagnostic
`deploy/runbooks/sdk-session-cleanup-status.sql` with their authorized Gateway
database connection. It reports the oldest 100 pending scopes and attempt/due
state. The native `status` projection supplies a bounded (1001-row) aggregate
with an explicit truncation flag; it is not exposed as an unauthenticated endpoint.
A dedicated dashboard is not supplied.

## Verification and rollout

This is staged, not verified by a live provider or database. Unit target/shape tests
are included; they do not prove concurrent database or network behavior. Required
isolated tests: owned/foreign admission; delete/admit races in both orders; scope
survives service removal and metadata edits; multiple frameworks; busy/disconnect;
crash before and after worker acknowledgement; restart retry; no-SDK deletion with
worker offline; denied non-Gateway grants. The worker must prove fsynced tombstone
and process-lock behavior, including unknown/pending executions.

Never edit frozen epochs 1–13 or invent epoch 14 fingerprints. Use the canonical
authority runner against an isolated empty reference database to measure all four
fingerprint classes and save the epoch-14 verification artifact. Verify fresh,
upgrade and role matrix before deployment. Drain old adapters before enabling the
new worker because they bypass the durable admission barrier. No dependencies,
services, database changes or provider calls were performed by this staging step.

## Storage ownership and cancellation

The fence executes only on the data connection inside the existing
PgOperationStorage supervised mutation transaction. The independent control pool
still only validates/cancels that data backend and drains cancellation; it neither
writes barriers nor borrows owner/migrator grants. Canonical epoch14 grants the
Gateway role SELECT/INSERT/UPDATE on barriers and scopes. Existing canonical
session write rights are unchanged. No new database role, migration or credential
is required. SDK admission/deletion use that same Gateway data pool. Nullable
legacy tenant/user writes have no authenticated SDK scope and retain their
existing behavior; SDK paths still reject empty owner identities.

## Session cache read cost

The shared DatabaseSessionManager projects bounded history tails, title-presence
checks and map-update existence checks directly from a borrowed cached record.
Those operations do not first clone unrelated history/state/config payloads.
Public full-session reads still return independent owned snapshots. Cache-miss
reads still load and normalize the complete durable row; this is not a database
history-pagination or cache-memory bound.

The projection uses the same monotonic cache expiry, strict durable expiry renewal
on cache miss, LRU touch, storage error and mutation-invalidation paths as full
reads. Python-compatible zero/negative history limits are unchanged, including
zero returning the full history. Identity, service binding, durable deletion
barriers and quota decisions remain with the existing callers. The generic public
history route reads a small owned user/tenant snapshot before its existing durable
deletion check and bounded history read. Other item routes and public full get keep
their full snapshots. Other manager implementations retain the get-based fallback.

Ownership and history remain separate awaits, as before; this is not an atomic
owner/history read or a new distributed-cache guarantee. The PostgreSQL save path
preserves user/tenant identity and rejects an owner-conflicting upsert; service pin
checks remain unchanged in get_or_create. Cache misses still decode all history.

In-process regression and allocation evidence can cover the actual native router,
request parsing, identity/ownership/deletion seams, manager calls and serialized
response with deterministic collaborators and no listener. This does not establish
HTTP throughput, real authentication/PostgreSQL latency, provider performance,
distributed cache consistency or live tenant acceptance.

## Native assistant DELETE compatibility

The native host also mounts DELETE `/api/v1/assistant/sessions/{session_id}`.
It authenticates through the assistant JWT identity seam and injects the same
`NativeServicesRuntime` cleanup owner used by generic SessionsApi. Canonical
ownership is rechecked inside the existing durable deletion transaction, before
any barrier/outbox mutation. There is no extra unscoped manager delete and no
fallback that could bypass Runtime cleanup or the SDK admission barrier.

The assistant response preserves Python's `{"session_id":id,"status":"deleted"}`,
404 `Session not found`, 500 `Failed to delete session`, and 503
`Session deletion was not completed` vocabulary. Runtime/storage failures remain
sanitized 500s on this surface; generic-session error details are not exposed.
A missing or non-durable collaborator fails closed with 503. Success additionally
checks the permanent durable deletion barrier: a retained Runtime FK parent is
logically deleted even though its physical row remains. Worker erasure can remain
pending; this response makes the same logical-deletion guarantee described above.

Repeated or concurrent DELETE after committed deletion returns 404. If a commit
succeeds but the postcondition read fails, the caller can receive 500 although
logical deletion completed; retry then returns 404. No compensating recreation or
remote action replay is attempted. Existing cleanup/commit uncertainty and worker
retry behavior remain unchanged. Read/create/history behavior is unchanged.

The combined Gateway library was compiled in a root-authorized, guarded, offline
single-job validation window. Eight production response-adapter tests passed with
deterministic cleanup collaborators (scope forwarding, retry, failure and durable
barrier outcomes), alongside three strict Runtime decoder tests and two retained
session serializer/query-extractor tests. These do not prove live PostgreSQL
ownership/concurrency, authenticated DELETE routing, Runtime transport or worker
erasure. No listener, database, provider or deployed product was exercised.
The separate [native declaration evidence](native-assistant-session-contract.md#post-declaration-delete-reconciliation)
keeps implemented DELETE distinct from its still-absent OpenAPI declaration.

## Runtime cleanup acknowledgement vocabulary

The production Runtime cleanup endpoint returns HTTP 200 with the requested
`session_id` and either `status: deleted` or `status: not_found`. The store reports
not_found for a scoped root thread that is absent or already tombstoned. HTTP 404
is also retained as an idempotent absence response by both Gateway control clients.
Thus a false cleanup result is not inherently failure and must not block deletion.

Both native and Python control clients accept only those two bound success
statuses. Unknown, missing or non-string status values and wrong-session payloads
raise the existing `AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID` error.
They cannot silently become false/absence and allow Gateway deletion. Public
assistant routes keep their existing sanitized 500 mapping; the native durable
cleanup owner aborts its deletion transaction on this error. No automatic Runtime
request replay, new acknowledgement kind, producer change or schema is introduced.

This validates logical tombstone/absence acknowledgement only. A Runtime 404 does
not prove why the response occurred, and neither deleted nor not_found establishes
active execution or external-effect cessation. Existing thread tombstone semantics,
SDK outbox delivery and worker erasure guarantees are unchanged. Twenty-one isolated
Python cleanup tests passed with 110 imported production modules bound to the exact
source snapshot; three decoder tests passed in the actual compiled Gateway library.
The Python root conftest was excluded and no live network or SQL behavior was tested.
The earlier Python source receipt is `70fdec6da9a98c30a857b22b533e45729decdfe2fa408f39c4b8e1cc960ee7c0`.
These are bounded candidate results, not actual-main execution or release acceptance.
