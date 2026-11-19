# Gateway scheduled assistant tasks

## First implementation

The console Tasks page and `/api/v1/scheduled-tasks` expose owner-scoped,
persisted, one-time and fixed-delay recurring assistant tasks. Times are submitted
as RFC3339 instants; the console converts its local datetime input to UTC.
Recurring tasks run the configured interval after the previous execution reaches
a terminal state. Missed ticks are coalesced, never replayed as a backlog.

Only the creator in the same tenant can list, change or inspect a schedule.
The launcher reloads the creator's active account and unexpired roles for every
launch, then uses the existing Assistant session and runtime turn entry. Model
access, provider availability, admission/quota, capabilities and runtime tool
approval retain their existing authority. No saved JWT, administrator impersonation,
new Agent kernel, automatic approval or protected runtime change is involved.
Schedules currently target the built-in Assistant with an explicit model and
text instructions; memory is off. Attachments, arbitrary URLs, Studio versions,
calendar cron expressions and email deliveries are outside this first slice.

## Durable policy and execution receipts

Canonical epoch `011_gateway_scheduled_tasks` creates
`gateway.scheduled_tasks` and `gateway.scheduled_task_executions`. Gateway is
sole application writer/reader, with SELECT/INSERT/UPDATE only. Composite foreign
keys preserve tenant/creator scope, and a partial unique index enforces at most
one active execution per schedule. The migration infers the process-role prefix
from the bounded authority owner; frozen baseline and old orphaned
`assistant_scheduler_jobs` are unchanged.

The host starts/stops the dispatcher with its existing lifecycle. When runtime
is enabled, the required schema must be present before startup succeeds. A tick
runs every five seconds, with a global cap of four preparing, dispatching,
running or uncertain executions. An advisory transaction lock coordinates claims;
the platform's existing singleton deployment requirement remains in force.
Do not infer general multi-replica or fencing readiness from this lock.

Every occurrence has a durable execution ID and unique session ID before launch.
The dispatcher commits `dispatching` before invoking the existing runtime bridge.
It persists the returned run/thread handle, reads current `assistant_runs`, and
consumes the existing runtime event projection to settle completion/accounting.
Event cursors are persisted. A timeout, disconnect, unavailable runtime or
uncertain dispatch never becomes fabricated success.

Safe pre-launch failures (authority availability, admission, session creation)
retry at 30/60/120 seconds, bounded by the configured maximum of 0–3 retries.
Authorization denial or exhausted preflight attempts pauses the schedule.
Resuming resets the preflight retry budget. Once launch may have reached Runtime,
no automatic replay occurs, including failed runs that might have tool effects.
A later recurring occurrence is distinct from a retry.

Restarted `preparing` executions are failed and their schedules paused; a
committed `dispatching` receipt is reconciled by its unique session. Missing
runtime evidence leaves it `uncertain`, occupying its concurrency slot. Operators
must investigate such receipts; this first implementation deliberately has no
force-replay or force-success API. Four unresolved receipts stop new dispatches.

Pausing suppresses future launches but does not stop an active run. Cancelling
marks the schedule cancelled, records cancellation requests on active receipts,
and forwards interrupts through Runtime. It does not fabricate a terminal state;
Runtime's terminal ledger/event remains authoritative. Cancellation racing a
committed launch may briefly permit that launch, followed by interruption on the
next tick. Cancelled schedules cannot resume. One-time schedules complete when
their execution terminates, even when paused while the execution was running.

## Public API

All routes require current authenticated creator identity and a non-public tenant.
JWT bearer credentials or an owner-bound database API key (default header
`X-API-Key`, deployment-configurable) resolve that identity. A bearer credential
takes precedence and failed bearer verification does not fall back to an API key.
Administrators do not bypass tenant/creator ownership. The machine-readable
request, row, receipt and error schemas are in `sdk/openapi.json`.

- GET `/api/v1/scheduled-tasks`: newest 200 owned schedules, `{tasks: [...]}`.
- POST same route: `{title, message, model_id, run_at, interval_seconds?, max_retries?}`;
  unknown fields rejected. Returns `{task: {...}}`.
- GET `/{task_id}/executions`: newest 100 execution receipts, `{executions: [...]}`.
- POST `/{task_id}/pause`, `/resume`, `/cancel`: returns
  `{task_id, status, cancellation_requested}`. Terminal schedules reject changes.

Interval is 60–31536000 seconds; title 1–120 characters, prompt 1–200000
characters, retries 0–3 (default 0). Explicit nonblank model ID is required,
limited to 255 UTF-8 bytes. `run_at` must be strictly in the future at validation.
The title is trimmed on storage; message and model ID are stored unchanged.
The request body is limited to 1 MiB. Omitted or null interval means one-time.
Creation returns HTTP 200, as do all successful lifecycle and read operations. API times are UTC-compatible
instants, not a timezone/DST recurring calendar expression. Listing limits are
explicit bounded views rather than a claim of complete history export.

### Response and failure details

Schedule rows contain `task_id`, `tenant_id`, `user_id`, `title`, `message`,
`model_id`, `status`, `next_run_at`, nullable `interval_seconds`, `max_retries`,
`retry_count`, `created_at`, and `updated_at`. `run_at` is creation input only.
Execution rows contain `execution_id`, `task_id`, `tenant_id`, `user_id`,
`session_id`, nullable `run_id` and `runtime_thread_id`, `scheduled_at`, `attempt`,
`status`, `after_sequence`, `cancel_requested`, nullable `error_code`, `created_at`,
`updated_at`, and nullable `finished_at`. Execution status is one of preparing,
dispatching, running, uncertain, succeeded, failed, or cancelled.

Errors use `{detail: {code: ...}}`, except rate-limiter errors whose `detail` is a
string. Forbidden authority headers return 400 `AGENT_RUNTIME_FIELD_FORBIDDEN`;
invalid admission returns 401 `AUTHENTICATION_REQUIRED`; missing/public tenant
returns 403 `TENANT_REQUIRED`. Unknown or other-owner schedules return 404
`SCHEDULE_NOT_FOUND`; malformed IDs return 422 `INVALID_UUID`. Creation validation
(including malformed/oversized JSON) returns 422 `SCHEDULE_INVALID`; creation
also applies the Assistant chat edge limiter (429). Disabled Runtime blocks
creation with 503 `AGENT_RUNTIME_DISABLED`. Unavailable admission/storage/limiter
returns 503. Storage uniqueness conflicts can return 409 `AGENT_CONFLICT`.

Completed or cancelled schedules reject all actions with 409 `SCHEDULE_TERMINAL`,
including repeated cancellation. Action requests require no body. Resume sets
active and resets retries without changing `next_run_at`; cancel returns
`cancellation_requested: true` as recorded intent, not proof of interruption.
No single-schedule GET, edit, delete or force-run API is implemented. The additive
in-app notification endpoints below read terminal execution receipts.

## Execution output and recovery

The console supports `/tasks?tab=scheduled&task=<task_id>&execution=<execution_id>`.
Links select the scheduled tab, load the caller's existing owner-scoped schedule
list, then resolve executions through the existing creator/tenant-scoped endpoint.
The link itself grants no permission. An unavailable receipt produces a bounded-view
notice: the schedule and execution APIs return the latest 200 and 100 rows, respectively.
Malformed schedule IDs are rejected locally without an execution-history request.

An execution with a runtime run ID offers read-only assistant output via the existing
Assistant session history API (limit 200). No direct database access, new API,
runtime replay, permission bypass or state mutation occurs when opening/refreshing
output. History retains its server-side owner/tenant and current source checks;
source-revoked messages are hidden in the result view. Output must carry the matching
`runtime_run_id`, so later manual turns in the same session cannot be misattributed. Text is rendered as escaped
plain text, without executing HTML, fetching links or exposing tool arguments.
This view does not claim to render downloadable artifacts or the complete chat UI.
Failed/cancelled/in-progress output may be partial and does not establish success.

Recovery guidance distinguishes a paused pre-launch failure, a launched failed run,
and an uncertain dispatch. It uses existing pause/resume/cancel actions, never a
force-retry control. Uncertain receipts direct the owner to refresh and investigate
execution/run IDs before creating replacements. Poll responses cannot replace
another selected schedule's history, and background refresh does not erase an
unresolved mutation error. Output refresh clears stale text before re-reading it.

## Durable in-app outcome notifications

`/api/v1/scheduled-task-notifications` and the console Tasks → Updates tab expose
one durable in-app notification per terminal execution receipt. The execution ID
is the notification ID. The inbox projects the existing Gateway-owned execution
row; there is no separate delivery worker, copied payload or enqueue gap. Failed
preflight attempts and pre-launch cancellations are included, even when no
Assistant session/output exists. Each attempt is its own receipt, so safe retries
and later recurring occurrences remain distinguishable.

Only `succeeded`, `failed` and `cancelled` executions appear. Preparing,
dispatching, running and uncertain receipts remain visible in Scheduled. In
particular, an uncertain dispatch is never described as failed or successfully
delivered merely to produce a notification. Reading an inbox, opening a link or
refreshing output never launches an execution or acknowledges it.

### Acknowledgement storage and semantics

Epoch `013_gateway_scheduled_notifications` adds only nullable
`notification_read_at` to `gateway.scheduled_task_executions`, a terminal-only
constraint, and tenant/owner terminal-history/unread indexes. Existing Gateway
table grants cover the new column; no other process receives data access. All
existing terminal receipts begin unread. NULL means the owner has not explicitly
acknowledged this receipt; a timestamp means that the owner requested Mark read.
It does not mean the output was available, the execution succeeded, or any
external delivery happened. Mark unread clears this one acknowledgement.
Repeated Mark read preserves the first timestamp until a Mark unread operation.
Read changes leave execution status, `updated_at`, Runtime event cursors,
scheduler retry budgets and cancellation state untouched.

The scheduler receipts have no existing spare acknowledgement field. Runtime
sequence/cancel fields have independent safety meanings; Assistant session
metadata cannot cover failures before session creation and can disappear under
session retention. An unbounded per-user JSON ID list or unrelated gateway-task
rows would introduce competing lifecycle/storage ownership. The one nullable
column preserves one durable source of truth without a new notification table.

### Notification API

Authentication and exact tenant/creator ownership are the same as scheduled
management, including owner-bound API keys. An administrator has no owner bypass.
No caller-supplied tenant, user, session or outcome is accepted as authority.
Successful responses and errors use `Cache-Control: no-store`.

- GET `/api/v1/scheduled-task-notifications`: `{notifications, unread_count,
  next_cursor}`. `filter=all|unread` defaults to all. `limit` defaults to 50 and is
  bounded 1–100. Supply both `before_at` (RFC3339) and `before_id` (UUID) from
  `next_cursor` to read older rows. Unknown query fields, invalid filters/limits
  and incomplete cursor pairs return 422 `SCHEDULE_NOTIFICATION_INVALID`.
- GET `/summary`: `{unread_count, total_count}` across all owned terminal rows.
- GET `/{notification_id}`: `{notification}` for any owned terminal receipt,
  independently of schedule/history pagination caps. The stable console link is
  `/tasks?tab=notifications&notification=<execution_id>`.
- POST `/{notification_id}/read` or `/unread`: idempotently sets acknowledgement
  state and returns `{notifications:[{notification_id,read_at}]}`.
- POST `/read`: `{notification_ids:[...]}` acknowledges exactly 1–100 unique IDs
  atomically. The request body is bounded to 16 KiB and rejects unknown fields.
  The console calls this only for unread IDs already displayed on the current
  page. New/unseen receipts cannot be accidentally swept into the batch.

Rows contain `notification_id`, `occurred_at`, nullable `read_at`, a minimal
`task` (`task_id`, current `title` and `status`), and the existing execution shape
without tenant/user identifiers or the redundant acknowledgement column. Prompts
and output bodies are not copied into the inbox. The notification API returns
only terminal execution statuses. `occurred_at` uses `finished_at`, falling back
to the original `updated_at` only for a legacy terminal row without a finish time.
The keyset tuple is `(occurred_at,execution_id)` descending. A read mutation never
changes that tuple. The page and unread count use one database snapshot, while
later pages observe a fresh snapshot: new outcomes appear on the latest page,
and another device can change unread membership between requests.

Malformed UUIDs return 422 `INVALID_UUID`; unknown, other-owner/tenant or
nonterminal IDs return 404 `SCHEDULE_NOTIFICATION_NOT_FOUND`. A mixed batch with
any unavailable ID rolls back without disclosing or modifying the matching
subset. Storage/schema failures return 503 `AGENT_STORAGE_UNAVAILABLE`, never a
fabricated empty inbox or zero unread count. A failed/timed-out mutation can be
ambiguous at commit; the UI retains its error until the owner checks again or
requests a new action. It does not optimistically claim acknowledgement or
retry mutations automatically.

### Console behavior and scope

The Updates label refreshes its account-scoped count every 30 seconds while the
Tasks page is mounted; the mounted inbox polls every 15 seconds. It supports
All/Unread, older/newer pages, latest-page return, explicit per-receipt read/unread,
page-only acknowledgement, and direct receipt lookup. There is no 200-schedule or
100-execution cap on this inbox's keyset traversal or direct lookup. Failed reads
hide stale inbox data and counts. Account changes and newer navigation discard
old asynchronous responses. Read-only output reuses the existing session-history
API and matching `runtime_run_id`/source-revocation checks described above.
Output access can fail independently of a valid durable receipt.

These are in-app records and explicit owner acknowledgement, not delivery
receipts. No email, push, browser/system notification permission, webhook or
external message is sent. This is not a global header, background push or
exactly-once external delivery contract. Retention follows the existing execution
receipt lifecycle; no new deletion or retention job is introduced.

## Activation and verification

2026-10-12 integration update: native workspace checking passed. Epoch012/013
canonical references have been measured and registered, including repeated
verify/migrate/verify, an independent empty-database fresh chain and unchanged
scheduler ACLs. Application notification acknowledgement, dispatcher behavior,
frontend execution and current binary linking are not yet accepted. The staged
proposal notes below describe the implementation pass before this integration.

This is implementation, not a verified release receipt. Epoch011's existing
canonical verification remains a separate recorded prerequisite. The notification
addition needs epoch013 after the reserved epoch012, with its exact structural,
ACL, extension and reference-data fingerprints generated on an isolated canonical
authority database. Do not reuse epoch011/012 fingerprints, alter frozen SQL or
relax authority verification. No epoch013 verification artifact is fabricated in
this implementation proposal. Missing epoch013 fails only the new notification
API; it does not create an empty successful inbox or disable existing scheduling.

The consolidated gate must compile the changed Rust/TypeScript surfaces, apply
and verify the isolated baseline + epochs, and exercise: future one-time launch;
recurrence/no overlap; global cap; preflight retries; expired creator role and
disabled account; tenant/owner negative reads/writes; pause/resume/cancel races;
process death before/after dispatch marker; event reconnect/cursor; uncertain
launch without replay; actual runtime terminal status and quota/accounting.
No live database, runtime provider or real scheduled execution was exercised in
the staged implementation pass.

Notification acceptance must additionally prove terminal-only visibility;
creator/tenant isolation including administrators; direct old-receipt lookup;
keyset ties and concurrent arrivals; all/unread and exact counts; initial unread
backfill; repeat read/unread across sessions/restarts; atomic mixed-ownership
batch rejection; concurrent acknowledgement lock ordering; no change to any
execution-control field; storage/commit failures; stale response suppression on
account/navigation changes; and permission-revoked output. Marking a page read
must leave newly arrived/unseen receipts unread. No notification API, browser
journey, Rust/TypeScript build or epoch013 database execution ran in the staged
implementation pass; these remain consolidated regression work.
