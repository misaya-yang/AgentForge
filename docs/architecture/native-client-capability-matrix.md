# Native client capability matrix

This document describes implemented source surfaces. It is not a build, live
acceptance or supported-platform receipt. The Gateway remains authoritative for
identity, tenancy, policy, tasks and the one Agent runtime.

| Capability | Shared Rust client | Native desktop | platformctl | Existing independent Agent CLI |
| --- | --- | --- | --- | --- |
| Gateway JWT login / password change | Existing auth methods | Existing login flows | Existing token through bounded non-terminal stdin | Separate provider/profile auth |
| Authenticated JSON transport | ApiRequest, one-shot mutations, bounded response | Delegates JSON requests to shared client | Same client | Separate existing compatibility transport |
| Thread/turn admission and V2 events | Existing create/interrupt/watch | Existing conversation workflow | No Agent conversation loop | Existing protected embedded runtime, unchanged |
| Tool capability catalog / effective policy | Existing Assistant public routes | Capabilities section and policy detail | capabilities / policies | No new integration claimed |
| Execution task list/detail/result | Typed query/status, shared request constructors | Status filter, pages, progress/detail/result | tasks list/get/result | No new integration claimed |
| Execution task cancellation | One DELETE, no retry | Frozen reviewed target, unknown-outcome guard | tasks cancel ID --yes | No new integration claimed |
| Task observation | Read methods; status terminal predicate | Selected-task polling until terminal/error | tasks watch; Ctrl-C stops observation only | No new integration claimed |
| Scheduled task admission/lifecycle | Typed creation, list/executions, pause/resume/cancel on frozen Gateway routes | Reviewed one-time/interval creation, lifecycle controls, execution history | schedules list/create/executions/pause/resume/cancel; explicit --yes | No new integration claimed |
| Scheduled outcome inbox | Bounded typed list/keyset/summary/detail/read/unread contract | Tasks → Updates, explicit per-receipt/page read state, execution output | notifications list/summary/get/read/unread; explicit --yes for writes | No new integration claimed |
| Local-node diagnostics | Public node status/capabilities/doctor/grants | Existing Local Nodes plus diagnostics | nodes list/status/capabilities/doctor/grants | No new integration claimed |
| Local-node events | Device-scoped validated cursor, atomic batch acceptance | Next page/watch/reset, bounded history | nodes events/watch, explicit resume cursor | No new integration claimed |
| Sessions and history | Existing history methods / shared transport | Existing workspace and conversation surfaces | sessions list/history | Existing separate local sessions |
| Agent/KB/provider/admin management | Shared generic public API transport | Existing specialized workspace editors | Not exposed as arbitrary mutation commands | No new integration claimed |
| Binary packaging | platformctl Cargo binary in client package | Five native candidate targets; portable Linux/Windows archives and macOS app bundles | Five native candidate targets and checksum receipts | Existing Linux x64 npm path unchanged |

## Shared safety and correctness boundaries

- HTTPS origin only, except explicit loopback HTTP IP; authenticated redirects are
  refused, and caller headers cannot override authorization or target origin.
- JSON responses are bounded to 8 MiB. Mutations are submitted once; transport
  failure is an uncertain outcome requiring a read before repetition.
- If-Match and Idempotency-Key are the permitted request-header extensions. The
  shared transport does not manufacture revision values or retry a conflicting
  write. Desktop retains its existing upload/download-specific handling.
- Node event pages must match the selected device, contain valid identifiers and
  increase the cursor atomically. Replay rows are ignored. Desktop retains the
  latest 2,000 events while preserving the cursor.
- JWT tokens are not persisted by platformctl, printed in errors or accepted in
  command arguments. Server authorization determines tenant visibility and role
  access; a visible capability is not permission to execute it outside runtime.
- No new endpoints are implied by a generic request type. No client-side Agent
  loop, scheduler, policy engine or direct Runtime/Worker connection is added.

## Release identity and event compatibility diagnostics

After ordinary sign-in the desktop makes one bounded, same-origin GET to the
existing Gateway `/version` route and keeps its result separate from connection
and operation status. Required-password-change sessions defer this check. A valid
`ai-platform/service-version/v1` Gateway identity shows the reported release and
source revision; it does not attest Runtime/Worker images, capability availability,
or upstream Codex synchronization. Missing, unavailable, malformed or unsupported
identity remains explicitly unverified, without preventing login or retrying writes.
The observation is a sign-in snapshot, not a continuous release-health monitor.

The shared event client distinguishes an unsupported `agent-event/v2` envelope
from malformed events or a wrong-thread response. A mismatch stops observation
with a release-unit diagnostic; it never replays turn admission. Missing or altered
capability metadata is not used to manufacture a compatible Runtime declaration.

The current release manifest identifies Runtime/Worker using `container_image`,
not HTTP `/version`. Their live identity and capability-schema agreement still
require the owning release-unit evidence and protected runtime contract. No source
lock, receipt or protected runtime implementation is changed by this client slice.
Native Linux workspace/binary compilation has been exercised; the desktop sign-in
window has rendered. These observations do not establish authenticated release
compatibility or Windows/macOS acceptance.

## Distribution matrix and claims

Canonical build target data lives in `deploy/release/platformctl-targets.json`:
Linux x64/arm64, macOS x64/arm64 and Windows x64. Each job uses a matching native
runner, builds only platformctl with the lockfile and one Cargo job, validates
binary architecture, runs packaged-input `--help` and produces a versioned
archive, SHA-256 companion and source/binary receipt. The SDK-tag workflow uploads
these as run artifacts; it does not publish a release or npm package for them.

Targets are candidate build configurations until their jobs and authenticated
acceptance pass. Windows arm64, Linux musl, installers, automatic updates, signed
binaries and macOS notarization are not implemented by this packaging path.
Checksums detect corruption but are not signatures. The root MIT license text is
preserved verbatim; its mismatch with native workspace Apache-2.0 metadata is
recorded, and public licensing reconciliation remains a release gate.

Desktop candidate targets live separately in `deploy/release/desktop-targets.json`
and use `.github/workflows/native-desktop.yml`. Its read-only candidate workflow
builds Linux x64/arm64, macOS x64/arm64 and Windows x64 without SDK publication.
Desktop `--package-info` and `--help` run offline before GUI/client initialization.
The packager validates compiled source identity, native architecture, archive
member permissions and extracted executable identity/help. It reports GUI and
authenticated acceptance as not run and public-release eligibility as false.

Desktop archive/app-bundle definitions are implemented; they are not installation,
signing or platform acceptance receipts. The Linux x64 development binary's
sign-in window has rendered, while authenticated journeys and native Windows/macOS
execution remain unverified. See [native desktop distribution](native-desktop-distribution.md)
for exact baselines, dependencies, launch/removal steps and release gates.

## Unified acceptance obligations

Verify per-target archive extraction and executable permissions; native help;
authenticated tool/policy visibility; list pagination; cross-tenant denial; task
cancellation/terminal watch; expired session; event cursor replay and wrong-device
rejection; interrupted writes with no repeat; desktop selection/navigation and
sign-out isolation; packaged source/binary/checksum consistency. Neither code
presence nor a candidate artifact replaces these results.


## Scheduled-task contract

Gateway `migration_agent/scheduled.rs` and canonical migration epoch 011 own the
schedule model. Client calls use GET/POST `/api/v1/scheduled-tasks`,
GET `/{task_id}`, GET `/{task_id}/executions`, exact GET
`/{task_id}/executions/{execution_id}`, and POST `/{task_id}/{pause|resume|cancel}`.
Lists and execution history accept paired `before_at`/`before_id` cursors over
immutable created_at/UUID descending keys, with maximum page sizes 200/100.
Native CLI and desktop default to 20-row pages. CLI accepts explicit cursor flags;
desktop provides Older/Latest and exact-ID recovery beyond the loaded page.
Shared parsers bind exact task/execution identities and validate page bounds,
ordering and cursor continuity while preserving open database-row fields.
Desktop retains off-page selection within the view, refreshes matching selected
rows, and abandons read/review state when leaving without dropping submitted writes.
No update, run-now, replay/retry or client scheduling endpoint is exposed.
JWT or owner-bound-key owner/tenant checks remain server-side; these client checks
are response-integrity checks and do not grant authority.

Creation contains a title, message, model_id, future RFC3339 run_at, optional fixed
interval and bounded pre-dispatch retries. Server validates the clock and format;
client enforces field/size bounds. Desktop freezes a request for review, and CLI
requires --yes with a regular bounded payload file. Pausing does not promise an
active execution stopped; cancellation requests are observed through history.
An uncertain dispatch or submission is never automatically replayed. Acceptance
must prove one-time/recurring behavior, pause/resume/cancel, owner denial,
execution history, unknown outcomes and revoked-creator permissions against the
real scheduler implementation, not merely mock responses.


## Scheduled outcome inbox

Gateway `migration_agent/scheduled_notifications` owns the durable receipt and
read-state projection; canonical schema epoch 013 adds acknowledgement state to
existing execution records. The client never creates another notification store.
Only terminal succeeded/failed/cancelled scheduled attempts enter this inbox;
uncertain dispatch is not a terminal outcome and is inspected in Scheduled tasks.

The shared `scheduled_notifications` module uses only
`/api/v1/scheduled-task-notifications`: GET list, `/summary`, `/{id}`; POST
`/{id}/read`, `/{id}/unread`, and `/read` with explicitly supplied IDs. List requests
accept all/unread, 1–100 rows, and the paired `before_at`/`before_id` keyset cursor.
Client parsers validate the receipt/execution/task identity, terminal outcome,
page bounds/order/cursor, and the exact requested mutation IDs and read state.
Listing, opening, refreshing and inspecting output never acknowledge a receipt.
Mutation requests are single-attempt and use the existing authenticated transport.

Desktop Tasks → Updates keeps one 50-row page and at most 128 navigation cursors,
with Latest/Newer/Older controls. The current page refreshes every 15 seconds only
while this view is visible. Reopening the view reloads instead of displaying a
stale count. Opening a receipt by ID loads its exact detail. Selection/navigation
changes abandon old read channels and clear their data; replacing the authenticated
workspace drops its complete inbox state. Submitted writes are neither cancelled
nor retried when the view closes or the account changes.

Mark read/unread applies only to the chosen receipt. Mark this page read freezes
only the visible unread IDs. Invalid or uncertain write responses block further
writes until an explicit, read-only check has reloaded every affected receipt;
a later list/summary response or an empty unread page cannot clear that guard.
Output uses the existing Assistant session-history route with limit 200, validates
the session, and includes only assistant text bound to the selected runtime run,
respecting source-revocation markers. It is a bounded partial projection, not
proof of success or absence of effects. The canonical Web path can be copied;
no Web origin is inferred from the configured Gateway origin.

`platformctl notifications` emits the Gateway-shaped JSON response as one line.
List/summary/get are read-only; read/unread require `--yes`. Read batch IDs are
explicit and bounded, never a server-wide mark-all operation. Exit codes retain
the management CLI contract: 0 success, 1 request/response or unknown outcome,
2 usage/input failure, and 130 interruption. No notification daemon, OS system
push, email, external message, background scheduling, or delivery guarantee is
implemented by any native inbox surface.

Release acceptance still requires compiled client/CLI/desktop checks, authenticated
Web/native cross-client read-state visibility, owner/tenant denial, equal-timestamp
cursor paging, malformed/stale responses, navigation and sign-out during in-flight
reads/writes, explicit unknown-outcome recovery, and source-revoked execution output.
Source implementation alone is not an acceptance or distribution receipt.

## Existing-run recovery across native and Python clients

`platform-client::assistant_run` and `platformctl runs get ID` read the existing
Gateway `GET /api/v1/assistant/runs/{id}` route. The published Python source now
exposes the same recovery as `client.runs.get(id)`. SDK clients must preserve the
Gateway's early `assistant_run_id` and `sdk_framework` identity; a worker session,
request ID or managed thread/turn is not a replacement. No client submits or
resumes execution during status refresh. This is a status/usage snapshot, not a
replay of SDK output or the managed V2 event cursor.

The shared `sdk/fixtures/run_recovery_v1.json` projection fixtures cover both SDK
engines, zero-text completion, pending and partial usage, explicit zero tokens,
late usage, expired authority, invalid identity/engine and historical managed
`completed` without translating it to success. A token detail is a bounded,
one-level observation. Raw provider metadata and fabricated Runtime lineage are
omitted. No client asserts accounting settlement, zero usage from absence, or
remote-effect cessation from a terminal state or cancellation acknowledgement.

Native cancellation remains `cancel_assistant_run` / `platformctl runs cancel ID
--yes`; Python reuses `client.chat.cancel(id)` on the same existing POST route.
All submit once and require an exact-target boolean acknowledgement. Python
retains its cancellation response shape and adds `UnknownOutcomeError` for an
uncertain submission/invalid response. CLI cancellation treats a redirect as an
unknown acknowledgement and instructs a read before any explicit repeat.

Python now limits automatic retries to GET/HEAD, refuses authenticated redirects
and cross-origin JSON/SSE destinations, and prevents request headers from replacing
authentication or owner scope. Its existing API-key configuration remains; native
platformctl continues using a Gateway token via stdin. Gateway admission and
owner predicates remain authoritative. Mock denied/revoked reads prove no cached
fallback in the client, not real multi-tenant authorization acceptance.

The independent local Agent CLI, Web SDK controls, managed execution and approval
routes are unchanged. Java/Dart SDK parity, native compilation, authenticated
owner/cross-tenant and cancellation-race acceptance, package publication and
platform release gates remain separate, unverified obligations for this slice.


## Native Desktop saved-source reconciliation (2026-10-13)

Desktop Sources now consumes the retained Knowledge owner's `manual_sync_available`
and `current_sync_task` facts for saved Confluence bindings. A separate review names
the dataset and binding and explains full reconciliation, including source removals
and preservation of documents referenced by another binding. Confirmation sends one
POST with a UUID retained before dispatch; progress and recovery use only the exact
request GET. No scheduler, connector credentials, endpoint or execution authority is added.

Restored tasks, unknown submissions, failed reads and mismatched receipts remain
blocked until an exact valid receipt is read. HTTP status alone does not prove
pre-admission rejection: this transport does not expose the service's structured
rejection body, so it conservatively keeps the guard. Pending/processing receipts
are observed with GET; read failures stop automatic observation until explicit recheck.
Terminal failure is distinct from successful zero updates and preserves partial counts;
unknown totals stay indeterminate. A new sync requires another review after terminal recovery.

Dataset navigation retains request identities in the signed-in session and closes
unsubmitted reviews. Account/origin changes clear private state and detach observation,
without claiming cancellation of server work. Copy the visible request UUID before
closing the app: local request retention is session-only. On reopen, refresh sources
and recover the server's current task, or paste the retained UUID into the read-only
exact-ID recovery control. A missing or stale current task is never proof of rejection.
No GUI, live connector, database, provider, restart-process or cross-platform acceptance
is implied by isolated native tests and Linux compilation.


## Native Desktop saved-connection maintenance (2026-10-13)

Desktop Knowledge now adds a bounded saved-connection list, affected-binding impact
review, and one explicit pause/resume action using the existing Python Knowledge
owner. Each request sends the reviewed exact revision and current status. The owner
rechecks connection ownership and current dataset editor authority; client snapshots
never grant access. List and impact pagination use the owner's 50-row pages. Total
binding count covers all datasets; hidden count refers only to the displayed page.
Hidden binding identities are never inferred or shown. Unsupported statuses offer no
pause/resume action, and malformed/mismatched metadata cannot enable confirmation.

Pause blocks new sync admission and existing work at its next authority check.
Already-issued work can finish and partial effects remain. It does not revoke the
provider token, remove indexed content, delete bindings, or edit credentials. Resume
does not start synchronization. Onboarding, credential replacement and scheduler
changes remain outside this Desktop workflow.

The exact CAS intent is retained before PATCH. Success, rejection, malformed reply
and lost response all require metadata-only impact readback and explicit “Use this
current state” before a fresh review. A changed revision or matching status does not
prove this particular attempt committed. No PATCH retries, automatic resume or sync
are issued. A failed or unauthorized read retains the pending intent and cannot unlock
a new action. Dataset/route navigation and capability changes clear confirmations and
queued observations while retaining session-scoped pending metadata. Account/origin
changes clear private state. Recovery controls remain reachable when source loading
fails or current write capability is absent; the service still checks read authority.

Retention is session-only. Save the displayed non-secret dataset/CAS recovery JSON
before closing the app; paste it into the same dataset after reopening for read-only
recovery. This does not recover unsaved intent automatically, persist credentials,
or establish real process-restart acceptance. Unit checks cover requests, validation,
production state transitions and closed queued-read exclusion; Linux compilation is
not GUI, live service/ACL/database/provider, cross-platform or release acceptance.

During a pending connection decision, new manual-sync reviews are blocked while exact
submitted UUID recovery remains available. Accepting current connection metadata
requests a dataset-keyed saved-binding invalidation and read-only source refresh after
navigation and any prior source job finish. Sync controls stay unavailable until that refresh succeeds; failed
refresh retains recovery and never submits a synchronization.

### Reviewed provider catalog sync

Web Services and Desktop Provider now use the additive native preview/apply/readback
workflow. No client falls back to legacy immediate synchronization when review is
unsupported. Users see proposed metadata, preserved overrides/policy, discovery
warnings, retained models and accessible Agent references before explicit selection.
New models remain disabled; there are no automatic removals. Review receipts are
actor/tenant/provider-bound, one-attempt, process-local and expire after 15 minutes.
Readback reports current models separately from the committed result; unknown outcomes
are never automatically replayed. An applied receipt establishes database commit,
not guaranteed delivery of postcommit audit/cache/publication effects. Agent impact
coverage is explicit-member current drafts/active publications only; process/service
defaults and other references remain unassessed. Python/hybrid ownership is retained.
Offline fixture/compilation evidence is not live provider, PostgreSQL concurrency,
GUI, restart, macOS/Windows or complete native release acceptance.

## Desktop official SDK service setup and settings (2026-10-13)

Services adds typed OpenAI Agents and Claude Agent SDK registration and editing,
using shared Rust client request builders. New registration uses the native-only
`POST /api/v1/service-registrations` create-only endpoint with a generated,
noneditable service ID. The exact-ID absence check before review is advisory;
the server atomically rejects an existing ID instead of overwriting it. An
existing record found during preflight is loaded for a separate edit review.
Unsupported routes/profiles (404/405/501) surface an explicit unavailable message.
Neither Desktop, CLI nor typed Web SDK creation falls back to the legacy
`POST /api/v1/services` upsert, which remains unchanged for legacy consumers.

Editing refreshes the exact selected service before freezing model, instructions,
and maximum turns with the latest unrelated metadata, including tool/Knowledge
bindings. Metadata PUT has no CAS/version precondition: a change after this read
can still be overwritten. A framework change during refresh invalidates review.
Blank replacement credentials are omitted, preserving the saved key. Credentials
are masked, excluded from review JSON and never loaded from server connector
fields; leaked credential projections are rejected. Framework/navigation changes
clear draft keys and reviews. Signing out discards in-memory editor state.

Each reviewed write is submitted once. Its acknowledgement triggers a separate
exact-ID detail GET; failed or abandoned observations retain a write fence,
including HTTP 400 (which can occur after persistence). No failure automatically
replays a registration or update. A valid exact-ID public detail read releases the
fence, but a configured-key flag cannot identify the key that won a replacement.
After an attempted exact-ID recovery read, users may explicitly acknowledge that
an unresolved service and credential grant may still appear, then abandon only
the local draft. The unresolved ID remains copyable/recoverable during that
session. A separate New action generates a different ID; abandoning or signing
out does not cancel or undo remote work. SDK tool/Knowledge selection remains in
the existing Web configuration flow; this editor preserves those saved bindings.

Typed Web SDK creation uses the same create-only endpoint and surfaces unsupported
profiles or conflicts without echoing transport bodies or provider keys. Its
submitted form is fenced after an attempted write; users must close it and inspect
the named service before a separate registration. Other Web registration flows
retain their existing legacy endpoint. Creation requires an SDK-capable native
Gateway plus the optional Python SDK worker. The supported candidate hybrid
composition can retain Python Knowledge; the retained Python Gateway itself does
not implement either SDK adapter or the atomic registration route. Enabling only
the worker is insufficient. The Web form states this before provider-key entry.
Its localized error messages name the exact attempted service ID; an unavailable
endpoint response is not used to infer which Gateway profile is running.

Controlled module tests cover payloads and editor transitions without sockets or
provider calls. Desktop compilation, authenticated GUI behavior, live persistence,
provider credentials and SDK execution remain separate acceptance scopes; consult
the current program evidence for checks actually run.


### platformctl official SDK service configuration (2026-10-13)

The management CLI now supports typed `sdk-services create`, `edit ID`, and
`get ID` with the Gateway authorization and create-only registration route. This is
separate from the independent local Agent CLI. Both `openai_agents` and
`claude_agent_sdk` reuse the shared Rust SDK-service request builders.

Supply the Gateway token through `--token-stdin`. Supply a provider credential
only through `--provider-key-file PATH`, a bounded regular UTF-8 file, never an
argument value or settings field. Protect these existing files with appropriate
owner-only access; do not put credentials in shell history or diagnostic logs.
The CLI does not change permissions, persist credentials, or read any implicit
credential source. Provider files remove trailing CR/LF characters and
otherwise use the shared 16–4096-byte, no-whitespace key contract.

Create settings JSON accepts `name`, `framework`, `model`, optional
`instructions` (default empty, normalized by Gateway to its helpful-assistant
default), and `max_turns` (default8, range1–32). Edit accepts only `model`,
`instructions`, and `max_turns`; omitted fields retain exact-read values, an empty
instructions string selects the server default. Omitted replacement-key files
preserve the saved key; an explicit empty key file is rejected. Unknown settings
fields, credential fields, and edit name/framework changes are rejected. Model
and instruction byte limits remain160 and32768. Settings files are capped at64KiB.

Example with already prepared files (no credentials in the command):

```sh
platformctl --origin https://gateway.example --token-stdin sdk-services create \
  --file sdk-settings.json --provider-key-file provider-key.txt < gateway-token.txt
platformctl --origin https://gateway.example --token-stdin sdk-services create \
  --file sdk-settings.json --provider-key-file provider-key.txt --yes < gateway-token.txt
platformctl --origin https://gateway.example --token-stdin sdk-services edit sdk-example \
  --file sdk-edit.json < gateway-token.txt
platformctl --origin https://gateway.example --token-stdin sdk-services get sdk-example < gateway-token.txt
```

Omitting `--yes` performs read-only review (GET absence or current metadata); it
does not submit a write. `--yes` explicitly permits one write using the supplied
files. Each create invocation generates a fresh timestamp/process-based ID; a
review ID is neither reserved nor reused by the next invocation. IDs are not
cryptographic uniqueness guarantees. Exact-ID absence preflight is advisory;
`POST /api/v1/service-registrations` performs atomic create-if-absent and rejects
collisions. Unsupported 404/405/501 responses fail closed without a legacy upsert
fallback. These failures still never trigger automatic retry. Editing preserves unrelated metadata
including saved tool/Knowledge bindings, but its metadata read/PUT has no CAS and
can overwrite a concurrent change after the read.

Before dispatch, stderr prints the exact service ID and recovery command. Every
write is attempted once, with no retry on any status, malformed response, or
transport failure. A write acknowledgement triggers a separate exact-ID GET.
Output projects only the named service configuration fields and key-configured
boolean; it never prints raw connector fields, metadata, write/error bodies, or
provider credentials. Instructions are intentional user-data review output and
may be private. Readback compares framework/model/instructions/turns and required
key presence, accounting for server defaults; differences return failure with
current settings for inspection. Key presence never proves which key was saved.

After any unconfirmed write or interruption, retain its printed ID and use
`get ID`. A404 is not permission to replay an uncertain create: it may still
complete. Starting another create generates another service and may leave the
first grant active. The CLI does not cancel, undo, retry, or automatically resolve
unknown outcomes. SDK command exit codes:0 successful review/readback,1 request,
readback mismatch or unconfirmed write,2 invalid local input,130 Ctrl-C. Stdout
is JSON; diagnostics/recovery target are on stderr. Authenticated persistence,
provider-key functionality, and cross-platform execution need separate acceptance.
