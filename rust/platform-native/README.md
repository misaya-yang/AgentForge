# Native Rust migration

## Initial operator

The canonical schema baseline deliberately excludes tenant and administrator
application data. After schema installation, explicitly run the
`bootstrap-operator --create` binary to provision the first operator. Supply a
bounded private JSON object on non-terminal standard input with `database_url`,
`expected_database`, `email`, `allowed_email_domain`, and `password`. Do not put
credentials in command arguments, logs, repository files, or documentation.
The command checks the target database name and installed system roles, creates
the default tenant if absent, and creates the initial operator transactionally.
Existing operator identities, passwords and tenants are never overwritten.
Use the normal authentication API to change the operator password afterwards.
Customer accounts are then managed through the authenticated users API.
See [tenant account administration](../../docs/architecture/tenant-account-administration.md) for tenant-scoped account and role management.

See [session authority](crates/platform-auth/README.md) for password-reset and
account-disable session invalidation and client sign-in behavior.
This command is explicit installation tooling; normal service startup never
creates an operator automatically.

Implementation workspace for the platform client, desktop, authentication and gateway crates. The full Python-to-Rust migration and production host assembly are still incomplete; router factories are opt-in.

## Gateway public health probes

The durable Gateway process mounts the existing `/health`, `/health/live` and
`/health/ready` router before the normal host middleware. It shares the exact
`ReadinessState` used by authenticated `/api/v1/health/services` dependency reads,
including the process resource handles, live configuration and drain authority.
`/health` and `/health/live` are public liveness checks and do not probe dependencies.
`/health/ready` returns 200 only when every core dependency is healthy, otherwise
503; its public response exposes only the aggregate core status. Dependency detail
remains admin-authorized. SDK configuration is not a new readiness prerequisite.

The existing native Axum GET/HEAD behavior is retained for these three probes;
`/api/v1/health` retains its separate liveness-only GET contract and HEAD rejection.
Compose and the operator status scripts use `/health/ready`. Helm currently has
its own probe defaults; this route correction does not change deployment policy.
Mounting a route does not establish a healthy live Runtime, valid OCI release
identity or completed production acceptance.

Build output should be placed outside the repository with `CARGO_TARGET_DIR`. This tree contains production source and required runtime data only. Verification code, synthetic fixtures and migration receipts are maintained separately.

The internal `ai-platform-gateway-foundation` crate owns reusable gateway modules and their bundled JSON data. `ai-platform-gateway` reexports these modules at the existing public paths and retains the host, knowledge, and migration assembly. This is a compilation boundary within the existing processes, not a new service; each source module has one crate owner. Keep cross-boundary visibility limited to the required internal helpers and preserve the original gateway API.

The internal `ai-platform-gateway-integrations` crate owns protocol clients,
knowledge ingestion and the query/index core, plus their shared object-storage
and image-fetch contracts. Gateway retains cross-service query HTTP, agent and
trace adapters and reexports moved modules at their previous public paths.
The query error's HTTP response implementation follows the error type across
this boundary; response status codes and bodies are unchanged. This split
reduces the Gateway compilation unit without adding a process or service.
No runtime readiness or memory ceiling is implied by source relocation alone.

For constrained local compilation, use one Cargo job with the workspace's no-debug, nonincremental development profile. The initial split proof linked both binaries with a gateway-only `codegen-units=1024` override; that result does not establish a large memory safety margin or live service readiness.

## Gateway token presentation

Generic terminal and assistant-history stats add per-component `token_provenance`
labels while preserving all numeric and accounting behavior. Adapter-observed,
locally estimated, locally derived and unknown values remain distinguishable;
historical unlabelled rows remain unknown. SDK usage keeps its separate contract.
See [Gateway token provenance](../../docs/design/gateway-token-provenance.md).

## Adapter streaming framing

OpenAI-compatible, Dify Chat and official SDK adapters share a pull-based UTF-8
line framer. Consume/scan cursors avoid shifting each remaining HTTP chunk per
line and rescanning an unfinished line on every fragment. Connection ownership,
permits, cancellation, UTF-8/CRLF handling and adapter limits are unchanged. This
is an algorithmic reduction, not a measured end-to-end latency claim. See
[native stream framing](../../docs/design/native-stream-framing.md) for scope,
regression coverage and the unchanged oversized-line limitation.

## Dify chat framework adapter

Select `metadata.adapter_type: "dify_chat"` on an HTTP service for the native
Dify Chat API. The historical `dify` alias still uses generic REST mappings;
existing configured services are unchanged. Set `connector_config.base_url` to
the application's API base (for example `https://api.dify.ai/v1`) and provision
the app's bearer authorization header through the existing protected connector
configuration. No app token is supplied in a public request. The shared registry
and dispatcher execute this adapter through the normal authenticated service
path; this is not another agent kernel or service.

Requests require text inputs and authenticated, nonempty `tenant_id` and
`user_id`. Dify end-user identity is a stable SHA-256 digest of the JSON tuple
`[tenant_id, user_id]`, so tenants cannot collide or override the upstream user.
Supported parameters are `inputs` (an object), `conversation_id`, `workflow_id`
(strings), and `auto_generate_name` (a boolean). Unknown parameters fail closed.
The adapter sets `response_mode` itself. `invoke` performs a real blocking
`POST chat-messages`; `stream` performs the real SSE request. Agent apps require
streaming, as documented by Dify. Uploaded files and workflow-only endpoints are
not part of this first adapter.

Blocking responses contain text and usage. Metadata carries
`dify_conversation_id`, `dify_message_id`, `dify_task_id`, and `dify_metadata`
when returned by Dify. Pass the upstream conversation ID as the next request's
`conversation_id`; it never replaces the platform session ID. Stream chunks
carry the same upstream identity metadata. Agent answer repeats are suppressed.
Chatflow streams wait for successful `workflow_finished` after `message_end`;
plain chat streams terminate on `message_end`. Provider errors, truncated
streams, unsupported message replacements and human-input pauses fail explicitly.
Cancellation closes the owned HTTP stream and releases its connection permit;
it does not claim to stop generation remotely.
For a persisted assistant row with text or tools, Gateway records the local
stream observation as `gateway_stream_status`: `completed`, `failed`, or
`interrupted`. History reload preserves failure/interruption instead of marking
partial answers completed. This shared stream marker is additive on native and
Python Gateway paths; existing SDK history keeps its SDK marker precedence.
Rows without a marker retain the legacy display fallback. A present invalid
marker displays a local history-observation failure instead of completion. Interruption,
including closing after a terminal was delivered but before local cleanup
finished, does not establish upstream cancellation, rollback, or side-effect
completion. Native empty non-SDK streams still do not create assistant history rows.
Retained Python now persists normally completed empty streams and successful
empty blocking responses; its failure/interruption behavior is unchanged. See
[the bounded empty-history contract](../../docs/design/langgraph-tool-outcomes.md#empty-successful-history-retained-python-2026-10-14).
Dify Chat remains text-only: structured and file-bearing workflow outputs are
not promoted into artifacts. Workflow-only endpoints remain outside this adapter.

Health checks call `GET info`
and require a successful JSON response, including valid app authentication.

Protocol references: [Dify chat API](https://docs.dify.ai/en/api-reference/chat-messages/send-chat-message)
and [Dify streaming](https://docs.dify.ai/en/api-reference/guides/streaming).
The opt-in adapter requires unified regression and an operator-configured Dify
smoke before production readiness is claimed.


## Shared native management client

`platform-client` owns the authenticated bounded JSON request contract used by
both the desktop workspace and the additive `platformctl` management binary.
The existing independent Agent CLI is unchanged; `platformctl` never embeds a
second Agent loop or executes a capability locally.

Build/package `platformctl` from the `ai-platform-client` package alongside the
native desktop. Its `--help` works offline without authentication. Normal commands
require an explicit `--origin` (HTTPS, or literal loopback HTTP) and `--token-stdin`.
Pipe one existing Gateway JWT access token into non-terminal stdin from a trusted
secret source. Do not pass tokens in process arguments or record them in scripts.
The client does not persist credentials, follow redirects, use a proxy, or retry
mutations. JSON responses are limited to 8 MiB. HTTP status failures are redacted.

Implemented management commands:

- `capabilities` and `policies`: tenant-visible Assistant tools and effective policy
- `tasks list --status processing --limit 50 --offset 0`, `tasks get ID`,
  `tasks result ID`, `tasks cancel ID --yes`
- `tasks watch ID --interval 2`: JSON snapshots until a terminal state; exit code 0
  for completed, 3 for failed/cancelled, 1 for request failure, 130 for Ctrl-C
- `nodes list`, `nodes status|capabilities|doctor|grants ID`
- `nodes events ID --after 0 --limit 100`, `nodes watch ID --after 0 --interval 2`:
  device-bound increasing cursor history; the server may return up to 500 events
  regardless of the requested limit
- `sessions list` and `sessions history ID`

Watch commands emit newline-delimited JSON. Read errors stop observation and are
reported; pass the last successfully emitted sequence to resume node observation.
Ctrl-C stops only local observation; it never requests task cancellation. An
interrupted cancellation may still complete on the server; inspect its task before
resubmitting. Every mutating command requires its specific confirmation flag.

Desktop additions use the same routes and request constructors: a Capabilities
section with selected tool detail and effective policies, task status filtering and
pagination, selected task progress/detail/result, reviewed cancellation and polling
until terminal, and a local-node event history with next-page/watch controls.
Local event rendering retains the most recent 2,000 records while preserving its
validated replay cursor. Changing device/task or signing out clears the associated
selection and pending review. Server authorization is authoritative for every call.

These are client implementation surfaces, not evidence of live backend readiness.
They rely only on the existing public Gateway task, Assistant catalog, local-node
and session routes. Packaging/distribution of `platformctl`, compiled desktop
acceptance, authenticated end-to-end checks and unified regression remain release
gates. Execution-task creation and direct tool execution are not exposed by this
client. Gateway scheduled tasks use the separate contract described below.


### Native management packaging

`deploy/release/platformctl-targets.json` defines five native build targets used
by the existing SDK-tag workflow. `scripts/harness/package_platformctl.py` turns
an already-built native binary into a checksummed archive and source receipt;
`--matrix` prints target metadata without compiling or touching credentials.
Candidates are uploaded to the workflow run, not automatically published as
release assets. See `RELEASE.md` for commands and release gates, and
`docs/architecture/native-client-capability-matrix.md` for client boundaries.

The independent `.github/workflows/native-desktop.yml` workflow defines five
native desktop candidate jobs without invoking SDK publication. Desktop packages
are portable Linux archives, macOS `.app` archives and Windows ZIPs, with clean
source/compiled identity, architecture, checksum and extracted offline-smoke
checks. `ai-platform-desktop --help`, `--version` and `--package-info` exit before
GUI or network initialization. Candidate builds embed source metadata using
`scripts/harness/package_desktop.py --build-env --target NAME`; ordinary builds
without metadata cannot be packaged as identified release candidates.

`docs/architecture/native-desktop-distribution.md` owns prerequisites, native
runner baselines, launch/removal instructions and publisher signing/notarization
gates. Existing Linux x64 linking and sign-in-window launch are limited evidence;
Windows/macOS builds and authenticated desktop acceptance remain unverified.
The workflow and packager do not sign, publish, install or supply credentials.


### Gateway scheduled tasks

After the Gateway scheduled-task routes and canonical epoch 011 are installed,
`platform-client::scheduled_tasks` provides typed creation and explicit lifecycle
requests. Desktop's Scheduled tasks section browses schedules and executions in
20-row keyset pages using Latest/Older, creates a reviewed one-time or fixed-interval
schedule, and pauses/resumes/cancels it. Selecting another page preserves the open
schedule and execution receipt. Recover by exact ID opens an authorized schedule
or an execution identified by its task ID and execution ID, even outside loaded pages. Server owner/tenant authorization is
unchanged. No desktop process owns timing or execution.

`platformctl schedules list [--limit 1..200]` and
`schedules executions ID [--limit 1..100]` default to 20 records. Both accept paired
`--before-at RFC3339 --before-id UUID` values copied unchanged from `next_cursor`.
Omit the cursor to return to Latest; `next_cursor: null` marks the final page.
There is no offset or automatic traversal. `schedules get TASK_ID` and
`schedules execution TASK_ID EXECUTION_ID` recover exact owner-scoped records.
The shared client exposes `ScheduleQuery`, `SchedulePage`, page reads and exact
read methods; existing no-query builders retain their compatibility behavior.
The new reads validate page bounds, descending timestamp/UUID ordering, cursor
continuity and task/execution identity while preserving unknown database fields.
Malformed responses are errors, never silently treated as the end of history.
Desktop retains its previous view on read failure and labels exact receipts
separately. Leaving Scheduled tasks abandons read/review state without cancelling
an already-submitted write. Fresh matching page rows refresh selected status;
off-page selections remain available until navigation. Reads never submit,
acknowledge or replay work. A successful read does
not clear an uncertain mutation; the user must inspect the prior submission and
explicitly acknowledge that review before another mutation.

`schedules pause|resume|cancel ID --yes` uses the unchanged action constructor.
`schedules create --file PAYLOAD.json --yes` reads a bounded regular JSON file:
`title`, `message`, `model_id`, `run_at` (future RFC3339 with explicit offset),
optional `interval_seconds` (60–31,536,000) and `max_retries` (0–3, default 0).
Token input remains on non-terminal stdin. Unknown JSON fields are rejected.
`--yes` approves the supplied payload and its future Gateway executions; recurring
runs may consume quota until paused or cancelled. No secret or payload file is
created automatically by the client.

Pause stops future dispatch; an existing execution may continue. Cancel marks
future runs cancelled and requests cancellation of active execution; inspect
history for its eventual outcome. `uncertain` is displayed as unknown dispatch,
never interpreted as permission to replay. Unknown submission outcomes require
reloading schedules/history before another attempt. All creation and lifecycle
requests remain single-attempt. Server validates time syntax and futurity.


### Scheduled outcome inbox

`platform-client::scheduled_notifications` and `platformctl notifications` consume
the same owner-scoped durable outcome receipts as Web. Desktop exposes them under
Tasks → Updates with All/Unread filters, bounded 50-row keyset pages, explicit
read/unread controls, receipt detail and read-only execution output. Opening or
refreshing an update does not mark it read. The copied Web path points to the same
receipt; Gateway and Web origins are not assumed to be identical.

Use `notifications list --filter unread --limit 50`, `notifications summary`, and
`notifications get ID`. Continue a page with both `--before-at TIMESTAMP` and
`--before-id ID` copied from `next_cursor`. `notifications read ID --yes` and
`notifications unread ID --yes` explicitly change only the supplied receipt's read
state. `notifications read ID [ID ...] --yes` acknowledges an explicit batch of
1–100 unique IDs; there is no implicit acknowledgement or mark-all command.
All mutations remain single-attempt. Inspect affected receipts after an interrupted
or unknown outcome before repeating a change.

These are in-app records of terminal scheduled attempts, not external or OS push
notifications. Uncertain launches remain in schedule execution history. See
[native client capability matrix](../../docs/architecture/native-client-capability-matrix.md)
for the exact contract, bounds, stale-response guards and remaining acceptance gates.

### Web registration

The Web Services registration dialog offers separate LangGraph and Dify tabs.
Dify uses the existing authenticated `POST /api/v1/services` path with
`service_type: conversational`, `metadata.adapter_type: dify_chat`, an HTTP
API base URL, and a protected bearer header. Only service administrators may
register it. This does not modify the historical `dify` generic REST alias or
introduce another public route. Use an application API key rather than a
workspace key; prefer HTTPS for hosted services. Registration does not imply
that the upstream is reachable or that credentials have been verified.

The Dify quick form enables text sync/stream and platform sessions, and disables
automatic retries. Playground stores upstream conversation IDs in authenticated
platform-session metadata, bound to the selected service. It reloads this binding
for subsequent turns and after reopening history; it never substitutes a platform
session ID for the Dify conversation ID. New sessions have no upstream binding;
turns with session persistence disabled remain independent. Service/account
switches invalidate in-flight identity updates. Metadata persistence failures and
Dify stream failures do not trigger a duplicate blocking invocation.

Apps requiring custom inputs still use the unified API's `inputs` parameter.
Agent apps require streaming. Browser acceptance of resumed conversations, real
upstream acceptance, and credential-safe configuration editing remain release
checks; source wiring and successful registration alone do not prove those flows.

The existing oversized Playground stream hook receives only dispatch integration
calls; Dify session ownership lives in `utils/difyConversation.ts`. This scoped
2026-10-12 exception is owned by Web/Gateway integration and should be removed
when the existing stream hook is split into protocol-specific dispatch modules.

### Run and governed SDK approval management

`platformctl runs get RUN_ID` reads the owner-authenticated Assistant run route.
`runs cancel RUN_ID --yes` posts once to `/api/v1/assistant/tasks/{id}/cancel`.
This is distinct from generic `/api/v1/tasks` task management. Cancellation receipts
are not proof that external effects were rolled back; get the run to check status.

`approvals inspect APPROVAL_ID` displays the SDK approval's server-curated preview
and binding. Review the target and parameters, then copy the exact returned
`run_id`, `tool_call_id`, and `arguments_hash` into:

```text
approvals approve APPROVAL_ID --run-id RUN_ID --tool-call-id TOOL_CALL_ID --arguments-hash SHA256 --yes
approvals reject APPROVAL_ID --run-id RUN_ID --tool-call-id TOOL_CALL_ID --arguments-hash SHA256 --yes
```

Both decisions require all three reviewed binding values and explicit `--yes`.
The client re-fetches the approval and rejects a changed/nonpending binding before
submission. Approve additionally requires a currently verifiable, active, unexpired
preview with the same hash. The POST carries `expected_run_id`,
`expected_tool_call_id`, and `expected_arguments_hash`; the Gateway checks identity
and uses the expected hash in its transactional SDK decision. These commands require
a Gateway supporting those binding fields. Runtime thread approvals are not exposed
by this SDK approval command. No resume, execution, or automatic decision loop is added.

Requests and decisions are never retried. On an uncertain write, inspect the exact
run/approval before deciding whether to resubmit. Ctrl-C only stops the CLI and cannot
undo a submitted decision. Output projects known fields, excluding raw argument objects
and arbitrary server envelope additions; the preview intentionally includes the
reviewable action content. Treat that content as private. Exit 0 means the request
succeeded, 1 means failure or unknown outcome, 2 means invalid input, 130 means Ctrl-C.

## Knowledge batch deletion recovery

A document cleanup failure retains its durable dataset deletion fence, so reads
and unrelated index writes remain unavailable until exact-target cleanup succeeds.
The native batch-delete endpoint accepts an editor-authorized retry containing
that pending document ID and gives it the first ordinal in the new durable batch.
The existing worker rechecks access and completes vector, asset and database
cleanup under the dataset-exclusive lease before clearing the fence. Admission
never clears the fence. Other deletion operations, other target selections and
malformed markers remain rejected; request and response shapes are unchanged.

Retry the failed document first; submit remaining failed IDs after it succeeds
when using multiple worker replicas. Ordering is a scheduling preference, not a
cross-replica execution barrier. If recovery fails again, the fence remains retryable, including recovery after
the source-row delete committed but fence clearing failed. A new batch does not
overwrite the earlier result.


## Knowledge ingestion publication contention

Ordinary ingestion holds its document owner lease through processing and terminal
persistence. If the dataset publication lease is busy before any publication
mutation, the worker preserves the running execution and immutable process-rule
pin. It returns that exact indexing generation to the durable waiting queue with
a document/dataset/execution/rule CAS. A terminal/replaced execution, deleted or
archived dataset, deletion fence, inactive document, or special publication cannot
be requeued by this path. Pending lifecycle restores retain their existing marker.

Only that pre-publication admission point is retryable. Cancellation, owner loss,
failed generation validation, and uncertain vector/database publication results
retain their existing handling; this is not permission to replay a mutation.
If the requeue CAS is refused or its database result is unknown, the worker does
not overwrite either the document or execution with an error terminal. Durable
recovery must reconcile the remaining original state. The ordinary dataset
publication fence can belong to another document while admission is busy; this
path never clears that fence or marks an index publication complete.

The worker-only contention marker is mapped back to the existing lease-conflict
error at the ingestion entrypoint. Public HTTP shapes, ACL checks, migrations and
schema are unchanged. Source/unit evidence does not establish multi-replica live
acceptance; verify publication contention, deletion/cancellation, owner loss, and
late-result races against the paired PostgreSQL/vector release before rollout.


## LangGraph SDK thread search compatibility

The native `POST /api/v1/langgraph/threads/search` accepts the LangGraph SDK JSON
envelope: `metadata`, `limit`, `offset`, and upstream search controls such as
`status`, `values`, `ids`, `sort_by`, `sort_order`, `select`, and `extract`. These
controls remain at the top level; only the actual metadata object receives the
Gateway's existing authenticated owner filter. The conversation facade uses the
same normalizer. Existing administrator recognition and upstream authorization
headers are unchanged. This does not broaden thread permissions.

The historical flat-metadata body still works when it contains no reserved SDK
search-control keys. A metadata field whose name is a reserved control must be
nested under `metadata`; the flat form cannot unambiguously represent both.
Explicit `limit`/`offset` query parameters take precedence over body pagination.
The existing Gateway page bound remains 1–100 (upstream allows more), offset is
nonnegative, and defaults remain 10/0. Invalid bodies, nested metadata or effective
pagination return 422 before sending a request upstream.

Contract reference: [LangGraph thread search](https://docs.langchain.com/langsmith/agent-server-api/threads/search-threads).
Actual SDK request capture and extracted native-normalizer tests are bounded
offline evidence; they do not establish authenticated live Gateway-to-LangGraph
server acceptance or cover Dify/provider execution.


## LangGraph streamed failures

The native and retained Python unified LangGraph adapters treat HTTP-200 SSE
`error` and `error|<subgraph namespace>` events as terminal failures. The latter
matches the installed official SDK's subgraph event type contract. A data-only
JSON envelope with the same top-level `event` is handled identically. An `error`
field or nested event inside an explicitly named `custom`/`updates` payload does
not imply a failed run.

Both adapters return the fixed `langgraph_provider_error` validation error. The
upstream message, credentials, URL and stack trace are neither returned nor logged.
Already delivered text/tool deltas remain delivered; no final-success chunk or
final usage summary follows the failure, even if the upstream subsequently sends
`end`, `[DONE]` or EOF. The owned HTTP response is closed; native also releases its
connection permit and clears pending output/usage. Local close does not establish
remote cancellation, provider billing, a persisted run status, or a refund.

This corrects the inherited behavior that ignored error events and could complete
successfully. Non-200 response diagnostics, ordinary completion and the transparent
LangGraph REST proxy are unchanged. Generic EOF/truncated-line policy is unchanged;
this correction does not make a stream without an explicit error fail on EOF.

Shared wire fixtures drive the production Python adapter with httpx MockTransport
and the production native stream with in-memory reqwest bodies, covering errors
before/after partial text, namespace errors, malformed error bodies, redaction,
terminal cleanup, and successful/custom-data controls. An external verification
packet also decodes the valid fixtures through installed official SDK 1.9.22
`Client.runs.stream` using in-process fetch. These are bounded offline regressions,
not authenticated Gateway-to-Agent-Server or Dify/provider acceptance. Test
presence alone is not a passing result; execution evidence belongs in the program
ledger and verification receipt.

## Quota overview effective-limit observations

The opt-in quota administration overview keeps its existing configured
`daily_tokens_limit`, `monthly_tokens_limit`, and `monthly_cost_limit_cents`
fields. Each user additionally has a `limit_observation` with one response-wide
UTC `observed_at`, `effective` daily/monthly token and monthly cost limits,
`sources`, and the stored `temporary_boost` amounts, UTC expiry and active flag.
Sources are `unlimited` for null/zero effective limits, otherwise
`configured_plus_temporary_boost` only when that dimension has a nonzero active
boost, and `configured` otherwise. This path does not resolve inheritance.

A boost is active strictly before its expiry; missing expiry is inactive. Pure
checked arithmetic is shared with admission. Null limits remain unlimited, and
legacy stored zero plus an active nonzero boost can become finite. The setter's
normalization and quota GET's configured limits/boost fields are unchanged.
Overview status and counts use the observed effective limits with the existing
usage-snapshot formula (used >= limit). They are not an admission guarantee:
admission separately evaluates estimated usage, policy, request caps and RPM,
and the boost can expire before another request. Overview never calls that
side-effecting admission path. The native adapter remains opt-in/unmounted;
synthetic projection tests do not establish production wiring or live DB health.
