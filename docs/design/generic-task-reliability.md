# Generic task execution safety

Status: bounded native Gateway source change, 2026-10-12. This is not durable queue delivery or release acceptance.

## Owner and public contract

`/api/v1/tasks` exposes the Gateway's generic asynchronous invocation tasks. Its task ID and statuses (`pending`, `processing`, `completed`, `failed`, `cancelled`) are independent of scheduled Agent executions, managed turns, SDK runs and Knowledge jobs. Native execution/reliability sections describe the native Gateway path; the retained Python profile has the separately bounded conditional lifecycle described below. Neither profile inherits the other's durability or database acceptance.

The existing `gateway.tasks` row is the authority for the native manager's lifecycle transition. The manager and PostgreSQL adapter compare the expected status and immutable task/request/service/user/tenant identity in one conditional update. The memory adapter applies the same rule under one lock.

Allowed transitions:

- pending → processing or cancelled
- processing → completed, failed or cancelled
- terminal states have no outgoing transition

A worker verifies the queued request/service/user/tenant identity against the stored task, then invokes only after winning pending → processing on that same verified snapshot. Duplicate queue entries are consumed without dispatch once another worker has claimed or finished the task. Competing cancellation and completion keep whichever terminal transition commits first. The losing completion cannot overwrite cancellation or send a completion callback. Repeated cancellation of a cancelled task succeeds; cancellation of a completed/failed task returns HTTP 409 `task_state_conflict` without rewriting its result.

Lifecycle timestamps are persisted with the transition. Callback delivery remains best effort and follows the winning completed/failed transition; it is not atomic or durable. Legacy direct storage save/update methods are low-level compatibility operations, not the supported worker lifecycle path.

## Generic task authorization

The retained Python and native resource routes require a nonempty matching tenant
before allowing the owner or an exact `admin` role to read a task, read its result
or cancel it. Tenant `admin` is not platform-global authority. Empty/null legacy
task tenants cannot grant access. Native NULL/malformed row identity already fails
during decoding with the existing sanitized storage-error response; Python NULL
and both profiles' empty/mismatched tenants return 404 before result retrieval or
cancellation. The existing list remains owner-and-tenant scoped even for administrators.

These generic task APIs define no cross-tenant platform-administrator exception.
`platform_admin` alone, administrator tier and wildcard permissions are not aliases
for the existing task `admin` branch. An account with both administrator roles is
still constrained to its own tenant here. Explicit platform-role capabilities in
other APIs, such as shared role-definition management, remain unchanged. Any future
global task-management exception needs a separately approved action/role scope and
audit contract; it cannot be inferred from a role label or historical bypass.

This closes the identical historical Python/native tenant bypass under TK-03/D01
and the existing [tenant administrator model](../architecture/tenant-account-administration.md).
It changes no role assignments, permission schemas, public payloads or cancellation
semantics. Reverting the guard reopens cross-tenant access and is not a
security-preserving rollback.

Production-path tests cover resource get/result/cancel, owner and same-tenant admin
success, cross-tenant and empty-tenant concealment, exact-role handling and denied
side-effect/result-read suppression. Native tests invoke the actual post-authentication
handler functions with resolved principals and recording memory storage. Python tests
mount the actual FastAPI router with identity/storage dependency doubles. Neither
suite establishes JWT/session/role-revocation, live PostgreSQL/Redis, provider abort,
authenticated browser or deployment acceptance.

## Cancellation meaning

A running worker checks the persisted row at up to 250 ms intervals while waiting on dispatch. Observing cancellation drops the local invoke future and signals the existing `AdmissionCancellation` cleanup path. A storage-read failure also drops/signals execution and fails that lane closed. The DELETE receipt acknowledges a winning task-status transition only. It does not prove that remote execution stopped, that an external effect was absent, or that money/quota/accounting was reversed.

A worker abort or process death can still leave a processing row. There is no automatic reset to pending, no redelivery and no retry of an uncertain external invocation.

## Remaining durability and result gates

Creation still saves before enqueue; bootstrap still uses the destructively consumed in-process queue. Restart loses pending envelopes and a crash after claim can leave unknown-dispatch processing work. Status comparison is not a lease, generation fence, transactional outbox or restart reconciliation protocol. A future domain-owned outbox/claim/fence requires explicit persisted identity and unknown-effect handling; unrelated job ledgers must not be reused as execution authority.

Native typed `UnifiedResponse` results now serialize through the existing canonical serde representation into the existing result JSONB column. Task creation initializes private bookkeeping metadata independently of caller context/parameters; those public request fields cannot inject the codec discriminator. Private task metadata records `gateway.native_task_result.v1` and output data kinds; the row decoder reconstructs only explicitly marked typed responses. Binary outputs remain bytes internally and use the canonical base64 projection on task list/get, the result endpoint and callbacks; JSON arrays/strings keep their original kind. Untagged legacy response-shaped dictionaries remain JSON and are never promoted by shape. JSON replacement removes a stale native codec marker while preserving other metadata. No new schema, role, grant, universal result envelope or public field is introduced.

Typed callbacks use the canonical public wire projection after destination validation, retaining existing retry/SSRF rules. Legacy falsy JSON-to-NULL behavior is unchanged. An old binary can still read the same result JSON, but cannot reconstruct the private typed result for the result endpoint; restoring native typed-result behavior requires this reader. Mixed-profile rollout/rollback must account for that semantic limitation.

The worker retains a conditional failure projection after a completion persistence error: an uncertain successful completion COMMIT or a winning cancellation cannot be overwritten. This is never permission to retry dispatch.

Focused in-memory race/cancellation tests do not establish PostgreSQL lock/concurrency, authenticated HTTP, real provider cancellation, crash recovery or full production readiness. Those must be independently measured against the final candidate.

## Native task HTTP binary projection

Task list/get and `/result` now use the same existing canonical wire projection for typed responses. Arbitrary non-UTF8 bytes become base64 strings instead of failing task serialization; response timestamps retain the public UTC `Z` normalization. The public object shape is unchanged. Legacy JSON results are returned unchanged, including any existing arrays or string timestamps. No authentication, ownership or cancellation route behavior changes in this projection slice.

## Verified PostgreSQL adapter boundary (2026-10-12)

The actual live foundation `PgTaskStorage` and `TaskManager` passed nine cases using the genuine Gateway database login: duplicate-claim and cancel/completion row-lock contention, both terminal winners, all four envelope identity mismatches, typed binary/JSON rereads, concurrent metadata preservation, legacy JSON and codec-marker replacement. Canonical epoch17 matched before and after; source and executable hashes matched after execution. Eight uniquely tagged synthetic rows were retained in terminal states using existing Gateway UPDATE permission, and the owned database process was stopped. No DDL, grants or deletes were performed.

Evidence: `platform-verification/generic-task-durability-next/postgres-harness/reviewed-receipt-c240c3a6fd9e4995819ea6a285bccbee.json`; build receipt `platform-verification/native43/generic-task-pg-build.json`. This is adapter-level database acceptance. Authenticated task HTTP, provider cancellation, process restart and durable queue delivery remain unverified.

## Web task-ID query request lifecycle

The task-ID query reads a result only after the matching task receipt reports
`completed`. A pending/processing lookup therefore makes no premature `/result`
request, and completion enables the result read without another search. Failed,
cancelled, unknown or unavailable receipts do not enable it. Result loading and
failure are visible, and a failed result read has an explicit retry action.

Task detail polling runs every five seconds only for a successfully read pending
or processing receipt. It stops after a terminal state, unknown state or exhausted
read error; the existing explicit retry remains available. Background refreshes
keep the task details mounted rather than replacing them with the initial loader.
The inbox retains its existing three-second active / fifteen-second idle refresh
policy because new tasks can appear after the current list is terminal.

All three generic-task query functions consume React Query's AbortSignal and pass
it through the existing Axios client. Replacing a query or removing its final
observer can cancel the obsolete read. This is client request cleanup only; it
never cancels a task or proves that server-side/provider work stopped. Public
HTTP paths, task state transitions and result payloads are unchanged.

Focused dependency-free Node checks execute the actual hook/API functions with
query and HTTP doubles and separately check component wiring. They cover polling
states, signal propagation, obsolete-read cancellation and completed-only result
selection. These checks are not React Query/browser integration, TypeScript/lint,
authenticated HTTP, latency or platform performance acceptance.

Pinned-toolchain validation after integration: Node26.11.1/pnpm12.11.2 type-check, lint, 300 tests (zero failures/skips), build with bundle budget, and i18n check all passed serially. All509 source files and dependency lock matched before/after in main and isolated verification copy. Evidence: platform-verification/web-validation-next/receipts/web-verification-aggregate.json. Browser and authenticated task HTTP behavior remain unverified.

## Explicit durable source path (candidate, not activated)

The connected native durable path is selected only with `GATEWAY_TASK_DELIVERY_MODE=durable`; the default remains `memory` and retains the limitations described above. The durable branch connects task+delivery admission, UUID/identity-fenced worker dispatch, current JWT actor revalidation, public cancellation and supervised shutdown to the existing task registry/APIs. Its Gateway-only schema is an unnumbered proposal and has not been applied. Missing authority fails closed; no claimed/unknown execution is replayed. Supported JWT admission, uncertainty receipts, callback limits, mandatory single-mode rollout and real-database acceptance gates are owned by [Native generic task durable dispatch](generic-task-durable-rollout.md). This source path does not establish production durability or supersede the earlier adapter-only PostgreSQL evidence.

## Generic inbox to result navigation (TK-01/TK-02, bounded Web slice)

The generic-task inbox uses the existing array response and `status`, `limit`,
`offset` API parameters. It requests 51 rows, displays at most 50, and enables
Next only when the extra row exists. It reports the loaded window, never a total
count. Previous remains available after an empty or failed later page, including
when rows disappeared between reads. This is the existing live offset ordering,
not a snapshot/cursor contract; concurrently inserted or removed tasks can shift
positions. Detail/result selection is always by the returned immutable task ID.

Filter, offset and selected task are URL state (`task_status`, `task_offset`,
`task_id`). Opening an inbox row goes to the existing Task Query tab, and returning
to the inbox retains its filter/window. Browser navigation and direct task links
restore that selection. A new selection resets the query input and cannot use an
old task's completed receipt to unlock a result. The existing detail/result
routes encode the ID as one path segment. A failed detail read hides its stale
receipt and disables results; a failed result read hides stale output and exposes
only the existing GET retry, never task resubmission.

List, detail and result query keys include the authenticated `user_id`. Queries
are disabled without a token and account identity, and use no credential in their
keys or URLs. The account table makes `user_id` globally unique; the current Web
user model exposes no tenant field. This cache separation prevents another
account's previous observation from being presented as current data; it does not
replace backend authorization or establish revocation within a cached account.
The existing signal cleanup, active/idle inbox polling and terminal/error stop
for detail polling remain unchanged.

This inbox covers only generic async service tasks. Its receipt contains no
source-session/document/evaluation identifier, so this slice invents no source
link or task aggregation. Scheduled task/execution receipts remain separate.
The native desktop already supports list offsets, selected detail and completed
result reads. Its reviewed cancellation request is followed by task observation;
uncertain mutations are not resubmitted. The Web generic inbox/query adds no
cancel, execution retry or deletion control. A result-read Retry only reissues GET.

The separately reviewed TK-03 resource guard now requires tenant equality before
owner/exact-admin access in both native and Python task routes, as documented above.
The Web-only navigation tests do not validate that guard or establish authenticated
HTTP acceptance; its source and production-path tests remain distinct evidence.

Focused Node 26.11.1 checks passed 32/32 using actual production helpers, hooks,
API functions and component handlers with explicit renderer/HTTP/auth doubles.
Actual TanStack QueryObserver checks cover late prior-account and prior-page
responses. These tests cover row 51 through result and return, lookahead/empty
windows, filter reset, URL reselection, unavailable detail and failed-result GET
retry while retaining the earlier polling/cancellation tests. They do not prove
browser rendering/history integration, authenticated HTTP authorization, native
execution, live pagination consistency or full type/lint/build acceptance.
Evidence: `platform-verification/task-result-next/receipts/focused-tests.log`.

## Historical Python observed-terminal cancellation guard

The Python DELETE `/api/v1/tasks/{task_id}` now rejects an observed completed or
failed task with HTTP409 `{"detail":"task_state_conflict"}`. It leaves the original
receipt and result intact, so the Web detail keeps its completed-result entry.
Repeated cancellation of an observed cancelled task returns the existing successful
receipt without another write or timestamp change. Pending/processing cancellation
retains its existing response. Authorization runs before lifecycle inspection, so
other-tenant and unauthorized task identities remain concealed by HTTP404.

This earlier bounded change corrected sequential behavior only. At that stage,
Python storage had no conditional compare-status write: cache-backed reads and
unconditional saves could overwrite a terminal winner, and the worker could
overwrite cancellation. The Python conditional lifecycle source slice below
supersedes that implementation limitation. The original guard tests alone did
not establish concurrency acceptance. Neither slice introduces a distributed
lock, schema, durable queue mode, replay or worker interruption guarantee.

Regression tests use the real Python manager, memory queue/storage and production
FastAPI router with a controlled identity over HTTPX ASGITransport. They cover
completed/failed rejection, retained output, repeated cancellation, pending and
processing cancellation and denied-owner/tenant requests. The external round-trip
reproduction feeds actual serialized receipts through the current Web API/hooks
and exact component output-selection code; it is not browser, database/cache,
concurrent-worker, JWT or provider acceptance.


## Python conditional lifecycle (source candidate)

Python generic-task workers now dispatch only after winning pending -> processing
against the same verified task/request/service/user/tenant snapshot. The worker
owns a detached request and roles snapshot across claim and dispatch, including
nested inputs. Admission also copies the envelope before saving/enqueuing, so
caller mutation during admission or before dequeue cannot alter admitted inputs. Completion and failure compare the original claimed identity and
processing state; cancellation and completion preserve whichever terminal update
wins. Terminal rows have no outgoing lifecycle transition. Memory storage copies
inputs and returned snapshots and applies compare/replacement under one lock.
Database storage uses the already-wired task repository's conditional UPDATE and
RETURNING; only lifecycle/result/error fields change, preserving current metadata,
progress and other bookkeeping. Missing or invalid stored status cannot become
pending. Missing database authority fails closed rather than acknowledging a write.

DELETE retains its authorized identity across bounded contention rereads. A
pending -> processing race can still be cancelled; an already-cancelled receipt
is idempotent, and completed/failed still return HTTP409 task_state_conflict.
Callbacks follow only positively acknowledged completed/failed winners, outside
storage locks. A callback failure cannot change the terminal row. Uncertain
completion may attempt a conditional failure projection but never overwrite a
committed completion/cancellation or replay invocation. An uncertain claim never
invokes. Missing/malformed item authority does not silently kill the worker lane.

Database task get bypasses both Redis and local caches because invalidation alone
cannot prevent stale read-fill resurrection. Constructor arguments remain accepted;
legacy task cache keys are ignored. Task-owned authoritative get/CAS decoding keeps
JSON string/list/scalar kinds. Existing unconditional save/update methods remain
low-level compatibility operations, not worker lifecycle authority. All writers
must use the conditional path for this guarantee; old binaries/legacy direct
writers are outside it, and rollback reopens the race. No schema, queue mode,
database epoch window or deployment profile changes here.

The task-owned codec now encodes/reconstructs typed UnifiedResponse results at the
Python database boundary, and projects typed callback/detail/list responses through
the same public binary/base64 representation as the result endpoint. The storage
format matches the native private `gateway.native_task_result.v1` metadata key:
`output_data_types` records none/text/bytes/json, with bytes stored as integer arrays.
Only a trusted UnifiedResponse creates that tag; raw response-shaped JSON and raw
metadata never promote themselves to a typed response. Native JSON arrays, strings
and null preserve their explicit kinds through private decoded-response provenance.
Malformed tagged rows fail closed with redacted errors, rather than becoming legacy
JSON or task-not-found. Response/content metadata and declared schema fields survive.

Repository get/list/pending/RETURNING explicitly cast JSON columns to text, then
perform exactly one task-owned parse; a JSON string such as "null" stays a string.
The existing generic-task facade delegates these bounded operations to its existing
repository. The global database row decoder and all other domains are unchanged.
A supplied result and its reserved marker change in the same conditional UPDATE;
unrelated current-row metadata is preserved, including a concurrent writer's keys.
Typed writes set the marker; actual non-None raw JSON replacements clear it. SQL
NULL, JSON null and non-object metadata are handled explicitly by CASE expressions.

Compatibility: existing lifecycle/direct-update `result=None` remains omission,
retaining both result and marker. Explicit JSON-null replacement is not supported
by that existing API; nullable fields inside a typed response remain supported.
Legacy unconditional save retains its historical falsy-result-to-SQL-NULL behavior,
while conditional/direct status updates retain non-None false/0/empty-list/object
JSON values. Low-level saves still replace snapshot bookkeeping and are not race-safe
lifecycle writes. Facade methods retain their disabled-pool no-op/empty behavior;
authoritative adapter reads/writes still fail closed when unavailable. An internal
keyword-only `result_format` carries codec evidence separately from arbitrary raw
metadata and is not a public request parameter.

Naive, UTC and integral-second timezone offsets are supported. Fractional-second
timezone offsets are explicitly rejected on encode, projection and typed decode:
native chrono FixedOffset cannot represent them, and Python fromisoformat can
silently change some such instants. Public timestamp rendering uses the existing
Pydantic datetime serializer, matching the existing API; storage preserves supported
offset precision. Invalid completion encoding cannot trigger a success callback or
repeat invocation. Transport/projection failure after a committed terminal write
cannot change that receipt. No durable callback delivery, queue recovery or restart replay is added.
Cancellation does not prove a running provider stopped or external effects were undone.

### Python cooperative local cancellation

After the conditional claim, the Python worker checks the same full task identity
and processing state before starting invocation. While invocation remains active,
it rereads authority at most once per second per active worker lane (plus the
initial read). Built-in stores read only the exact task/request/service/user/tenant
identity and status: memory snapshots these immutable values under the same lock,
and the wired database repository selects those six columns directly without caches
or JSON/result decoding. The frozen observation is not a claim or transition receipt;
claim, terminal CAS and callbacks retain their existing full-row paths. The base
storage method falls back to a full authoritative get for legacy subclasses.
Missing authority, invalid status and read errors still fail closed. Post-claim
corruption limited to unselected payload/metadata/timestamps is no longer detected
by this observer; full claim/public reads and terminal receipt decoding remain
unchanged. The projection intentionally observes lifecycle authority only. Read
latency and system throughput are not bounded by this cadence. Queued tasks create no observers; the invocation and observer are
owned by that lane and joined before terminal persistence. Persisted cancellation, identity replacement, another terminal state,
or an authority read error cancels and joins the owned local invocation. An
unavailable authority does not create a failed receipt or callback. Independent
adapter cancellation leaves the task outcome unconfirmed and does not kill the
reusable worker lane. Nothing is replayed.

Observation ends before terminal persistence/callback starts; the existing exact
claim CAS and acknowledged-winner callback rules remain authoritative. Shutdown
cancels and joins worker lanes, retaining cleanup across repeated/concurrent stop
calls. Cleanup is awaited rather than detached; a cancellation-resistant adapter
can delay shutdown and reuse of its lane. The one-second interval is a normal
observation cadence, not a hard cancellation latency bound when authority reads,
provider cleanup or the event loop are slow. This mechanism stops cooperative
local awaitable work only: already accepted upstream work, charges and external
side effects may continue. A cancelled status confirms persisted task state only.

Controlled ASGI submit/status/result/cancel and actual dispatcher/queue/worker
regressions cover the memory and Redis-serialized envelope paths without listeners.
They do not establish real Redis/database, authenticated CLI binary, live provider,
remote cancellation, settlement or process-recovery acceptance.

Focused tests use production memory manager/worker and ASGI routes, plus the wired
repository/adapter with a recording SQL interpreter. They exercise claim duplicates,
identity and request mutation, both terminal winners, uncertain acknowledgements,
callback winner semantics, detached snapshots, cache bypass and JSON kind retention.
The SQL interpreter is not PostgreSQL lock/concurrency evidence. Controlled copy
cost and statement-count fixtures are not live latency or throughput acceptance.
Live Python-profile database contention, provider, browser, restart and rollout
verification remain open; native epoch17 evidence does not satisfy Python's2..10
schema admission window.


Codec acceptance additionally uses the actual Python worker, manager, repository,
storage adapter, schemas and in-process ASGI task routes with a controlled recording
SQL seam and request-only callback transport. It verifies successful binary results,
fresh reads, marker set/clear/omission, legacy JSON, native-format source-derived
fixtures, scalar read decoding, projection parity and metadata preservation. These
checks are not real PostgreSQL execution, network callback delivery, provider or
cross-process native/Python acceptance; those live boundaries remain open.

### Python async consumer regression boundary

`tests/services/task/test_task_consumers.py` follows public `/submit` through the
actual request validator/RBAC, dispatcher, task manager, queue and CAS worker to
public task/result handlers. Eight ordinary-suite cases cover memory and recorded
Redis-envelope paths for completion, provider failure, pre-dispatch cancellation
and session propagation. They also check authenticated identity, duplicate delivery,
canonical binary-result projection and detached result reads. Recorded Redis members,
successful pops and unused memory fallback are asserted explicitly. Service lookup,
rate-limit/session/usage persistence and the provider adapter are controlled seams;
HTTP uses in-process ASGI transport. These checks do not establish live Redis,
PostgreSQL, provider, socket-listener or process-recovery acceptance.


### Python observation projection measurement boundary

Bounded deterministic in-process reads compare the unchanged full memory get with
the lifecycle projection using small and 8,000-item synthetic request_data payloads.
The large payload is 1,372,691 serialized JSON bytes; this is fixture size, not
measured database wire traffic. Single-read traced peak allocation was 3,465,016
bytes for full get versus 712 bytes for the projection. Timing is recorded separately
from allocation instrumentation; evidence and reproduction are in
`platform-verification/task-observation-cost-next/measurements.json` and `measure.py`.
These fixtures demonstrate avoided deep-copy work, not production throughput,
provider cancellation latency or live PostgreSQL behavior. Projection regression
tests compare all lifecycle states and identity components, missing/unavailable
authority, SQL selection, lock snapshots, legacy fallback and owned cleanup.


### Native memory list pagination cost

The native memory store filters and stably sorts references, then clones only the
requested page. Public list rows retain their complete results and bookkeeping;
no result-summary schema or payload truncation is introduced. User/tenant/service/
status matching, descending timestamp/task-ID ordering, mixed naive/aware timestamp
rejection, offset/limit semantics and detached returned rows are unchanged. The
mutex remains held through reference sorting and selected-row cloning, without
awaiting. Previously it was released after cloning every matching row. This changes
the critical-section shape: fewer payload clones, but sorting now holds the lock;
no lock-wait or concurrency-latency claim follows without measurement.

The PostgreSQL list still filters/orders/paginates in SQL before task decoding,
and Python memory storage already copies only its selected page. Those paths and
the legacy storage traits remain unchanged. API actor checks and Web lookahead
pagination are unchanged. The public list contract includes full result data, so
large selected results remain an intentional cost. Source tests and a bounded
source-extracted standalone comparison are staged under
`platform-verification/task-list-cost-next`. Standalone type doubles do not establish
full native-library, database, browser, throughput or provider acceptance.


### Python Redis task queue recovery (2026-10-14)

With the built-in memory fallback, a Redis pop/decode error no longer strands the
worker forever waiting on an empty fallback. It checks any already queued memory
work and otherwise waits one second before polling Redis again; healthy empty
polls retain their existing cadence and Redis-first scheduling. Diagnostics omit
backend exception details. Worker cancellation still propagates and joins its lane.
No ambiguously popped member is recreated or replayed; a lost pop acknowledgement
can still strand that task. This is same-process observation recovery, not a durable
claim/ack queue. Custom fallback implementations retain their previous blocking
dequeue behavior and do not gain this recovery guarantee.

Controlled production queue/manager/worker checks cover recovery, malformed removed
members, pending cancellation, shutdown and fallback paths. Redis command and
provider seams are controlled; no live Redis, database, provider, process restart
or complete J03/J21 acceptance is established by these offline checks. Persisted
cancellation still does not prove external side effects or charges have stopped.
