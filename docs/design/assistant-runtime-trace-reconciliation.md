# Durable Assistant Runtime Trace reconciliation

The Gateway writes one Eval Trace for each terminal Agent Runtime turn. The
stream observer can disappear when a browser disconnects or the Gateway
restarts, so a Gateway background reader also reconciles terminal turns from
the durable Runtime tables. It never starts, resumes, or replays a turn.

`assistant_runs` supplies the original run UUID, scope, status, and timestamps.
`assistant_runtime_snapshots` must match that run's tenant, user, session, and
snapshot identity. `assistant_runtime_items` must contain a terminal V1 event
for that same original turn and scope. `completed` and `succeeded` map to Trace
`succeeded`; `failed` and `cancelled` retain their status. A run without a
matching terminal event is skipped until a later scan.

The reader scans at most 100 runs per page inside the Trace retention window,
advancing by terminal timestamp and run UUID. It reads visible `text_delta`
chunks by database cursor, so long turns do not depend on the SSE 1,000-event
page limit. Only the first 2,000 visible text characters feed the existing
bounded redaction helper. Other event types contribute counts only; raw
reasoning and tool payloads are not copied to the Trace. Measured model tokens
and exact micro-USD cost are included only when the durable model-call ledger
has complete receipts for every dispatched call.

The original run UUID is the Trace UUID. The single Trace terminal event uses
the original Runtime event UUID and carries its Runtime sequence. A database
transaction locks the scoped run, inserts the Trace/span/event once, and
creates one `trace.ingested` outbox job. If live ingestion stopped after writing
the Trace, reconciliation verifies its scope and fills missing span/event rows
before enqueueing; existing rows are preserved by their unique keys. Normal live Runtime Trace ingestion
locks the same Trace row and treats an outbox job in **any** state as already
enqueued. Repeated Gateway restarts therefore leave the same Trace and job.

The original HTTP request ID is not stored in the Runtime ledger, so recovered
Traces use the original run UUID as `request_id`. First-token latency is set to
zero because a cold read cannot reconstruct when the first token was observed.

Eval recovery can encounter this terminal Trace before its own worker resumes.
The worker reads the original scoped model lease/call ledger and the immutable
Agent Version pin (matched against the turn snapshot), fills missing observed
model spans for scoring, and verifies the requested model and Version. It does
not start a candidate again. Prompt/tool hashes are recorded in new turn
snapshots from the actual instructions and dynamic tool projection; older
snapshots without hashes remain unverifiable for those dimensions.

Known provider stream validation failures retain their classified error code
in the model-call ledger. A dispatched call remains `unknown`, with unknown
usage and no automatic retry. Transport interruption still uses its fallback
code; classified protocol failures no longer collapse into `stream_interrupted`.

V2 observation and approval/recovery controls also accept correctly bound Draft
Preview and published channel sessions. Draft controls recheck current Agent
access without replacing the original turn with the latest draft; published
controls resolve the original pinned Version and current publication access.
Creating a V2 turn retains its stricter Version-only binding check. Rejection
remains available for parked actions when execution access is revoked.

Trace-to-case imports first verify the caller can read the original Trace and
selected span. The dataset/tenant/Trace/span identity selects one existing case
or a deterministic UUID protected by the existing primary key. Reimports return
the original case without overwriting review or expectations. New automatically
derived cases always start with `review_status=pending` and unconfirmed behavior.

Evaluator job receipts normalize database UUIDs to API strings before response
validation. An accepted rescore job must return its existing run/job handle;
response formatting must not make accepted work appear rejected.

Live terminal Trace ingestion also reads complete scoped model-call receipts
in its background task. The browser stream remains independent of that read.
Incomplete usage is marked unknown; the Trace UI displays a dash for missing
token or cost measurements instead of zero. Measured zero remains a valid
receipt, and small positive costs are shown as less than one cent.

Eval observations project native capability execution receipts as tool spans,
including safe result identifiers such as a Quiz UUID. They do not expose raw
arguments or result payloads. Selected-case retry also checks the original
capability ledger and tool events; an older Trace that omitted tools cannot
prove that the original turn had no effects.

Gateway startup leaves a provider's revision unchanged when its decrypted
credential, API type and configured endpoint are unchanged. Re-encrypting the
same credential must not invalidate an original turn's model lease after a
Gateway restart. Real configuration changes still advance the revision and
the model plane rejects a stale lease before dispatch.

Publication expiry is canonicalized to UTC in Gateway snapshot and release
identity checks. The legacy launch adapter projects the three established
Runtime channel permission fields; expiry remains Gateway publication metadata
and current channel access checks enforce it. Unknown permission fields still
fail the closed Runtime contract.

An explicitly empty Agent capability allowlist with no attachment tools pins
an empty Thread/Turn catalog. It needs no authenticated capability-catalog
lookup and inherits no discovery bridges. Built-in inheritance and nonempty
selected capabilities retain the original catalog authorization checks.

Agent operations use explicit Trace identity columns. Live ingestion and cold
reconciliation fill missing columns only from the original scoped Runtime
run/snapshot and its immutable session pin. Caller-supplied metadata is not
attribution authority, existing identity is preserved, and missing dimensions
remain eligible for reconciliation even when the Trace outbox already exists.
This restores Agent/version/channel and feedback joins without replaying calls.

Embed grant verification accepts both the signed token and its declared origin
header through the general forgery guard. The Embed handler still verifies the
signature, nonce cookie, allowlisted origin and publication expiry; other
reserved Agent headers remain forbidden.


## Trace ingestion authority and atomicity

The public Eval ingest route uses the same scoped identity as its read/score
routes: exact `admin` and `operator` may act across users within their authenticated
tenant; other authorized callers may ingest only their own user identity.
`platform_admin`, a tier, a wildcard permission or a payload user/tenant field does
not independently grant tenant-wide ownership. Internal capture retains its
explicit subject attribution. No caller has a cross-tenant Trace exception.

Both implementation profiles enforce the tenant and optional owner scope inside
`ON CONFLICT (trace_id) DO UPDATE`. The existing row's tenant cannot be reassigned.
An authorized same-tenant operator can still update or reattribute a Trace within
that tenant; ordinary callers cannot replace another owner's Trace. This is an
application of the existing Eval policy, not a shared global administrator rule.
Agent Studio continues to apply tenant predicates before its independent member/
tenant-admin ACL; global platform capabilities retain their own explicit policy.

One transaction now owns the Trace update, server-derived Runtime dimensions,
spans, events and optional outbox enqueue. Global span UUID conflicts must belong
to the authorized Trace, and referenced parent spans/event spans must belong to it
as well. A missing mutation `RETURNING` row is a concealed 404 and rolls back all
prior writes. Storage failure and cancellation likewise cannot leave an accepted
partial tree or a queued job. Existing Eval outbox claim validation remains in
the same transaction and must match the ingestion tenant. Runtime capture still
treats an outbox job in any state as already enqueued; other capture preserves its
queued/running deduplication semantics.

The public response shape and schema are unchanged. The change adds no role,
permission, schema or global access exception; existing audit/read capabilities
are unchanged. Offline handler/SQL-boundary doubles and native compilation do not
prove PostgreSQL lock, constraint, concurrent cancellation or authenticated live
behavior. Those remain explicit acceptance gates. Rolling back this code restores
unsafe ingest authority and is not a security-preserving rollback.
