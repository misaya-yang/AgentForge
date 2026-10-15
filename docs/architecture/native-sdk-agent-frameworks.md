# Official Agent SDK framework adapters

Status: staged implementation, not production-ready. Observed 2026-10-12 against
`385e9d8666f2447b10cfedbf6fec30ba5b7ab6ec` plus the current native migration worktree.
The existing Codex Assistant remains the default managed Runtime. These opt-in
framework services are separate explicit execution choices, like remote framework
adapters; they never silently replace the Assistant kernel.

## Boundary decision

The native Gateway, identity, service authorization, quotas and dispatcher remain
Rust. Official Python SDKs execute in an optional isolated worker role. This is a
real SDK execution dependency, not a Rust reimplementation or completion-only
adapter renamed as an Agent SDK. The worker has no import edge into `src`, another
application, or Gateway-owned database tables. It must not receive capability lease
signing secrets, user session tokens, cloud credentials or operator filesystem access.
The proposed [ADR-012](ADR-012-optional-official-sdk-execution.md) narrowly amends
the old blanket single-loop/service-freeze wording for these explicit SDK services,
while preserving the Codex core. Maintainer adoption and release gates remain open.
It does not authorize reintroduction of the deleted first-party Python Agent loop.

## SDK evidence and exact proposed versions

- [OpenAI Agents Python 0.23.1](https://github.com/openai/openai-agents-python/releases/tag/v0.23.1),
  released 2026-10-02: pinned as `openai-agents==0.23.1`.
- [Claude Agent Python 0.2.165](https://github.com/anthropics/claude-agent-sdk-python/releases/tag/v0.2.165),
  released 2026-10-08, bundled CLI 2.1.294: pinned as `claude-agent-sdk==0.2.165`.
- [OpenAI configuration](https://openai.github.io/openai-agents-python/config/):
  supports API-key clients; explicit per-run client avoids process-global tenant
  credential mutation. This implementation disables exported SDK traces.
- [OpenAI model support](https://openai.github.io/openai-agents-python/models/):
  Responses is supported by the SDK; this slice intentionally selects
  `OpenAIResponsesModel`. Other provider/model transports are not implied.
- [OpenAI streaming](https://openai.github.io/openai-agents-python/streaming/):
  `Runner.run_streamed` runs the SDK loop; iterator exhaustion and completed state,
  not the last visible token, establish completion.
- [OpenAI sessions](https://openai.github.io/openai-agents-python/sessions/):
  SQLiteSession preserves SDK conversation items; it is private worker state.
- [Claude overview](https://code.claude.com/docs/en/agent-sdk/overview): the Agent SDK
  is a library embedding Claude Code execution, distinct from the plain API client.
  Third-party products must use API-key authentication unless Anthropic separately
  approves subscription login. No consumer-login/subscription path is offered here.
- [Claude quickstart](https://code.claude.com/docs/en/agent-sdk/quickstart): native
  binaries normally accompany wheels, but source distributions can require a
  separately installed binary. The deployment must verify its target architecture.
- [Claude Python reference](https://code.claude.com/docs/en/agent-sdk/python): custom
  MCP tools, partial stream events, model selection, and session resume are exposed.
  `allowed_tools` is auto-approval, not a complete security allowlist. This worker
  sets built-in tools to empty, ignores settings sources, forces strict MCP
  configuration, and denies all permission callbacks outside owned pure tools.
  Result `terminal_reason` is checked even when subtype says success.

Models are administrator-selected, provider-entitled IDs; no stale hardcoded model
catalog or claim of all-model compatibility. Bedrock, Vertex, subscription auth,
hosted OpenAI tools, realtime and SDK subagents are not enabled by this slice.
No packages were installed and no legal agreement was accepted during staging.
Dependency lock generation, license/SBOM review and actual version API checks remain
release work; declaring exact SDK requirements is not equivalent to a complete lock.

## Public configuration

Official SDK service creation uses the native-only, create-if-absent
`POST /api/v1/service-registrations` route. It rejects an existing service ID;
clients must never fall back to the legacy `POST /api/v1/services` upsert. Existing
service detail/update/invoke/stream routes remain in use.

These adapters require an SDK-capable native Gateway and the optional Python SDK
worker. The retained Python Gateway has neither adapter and cannot run them;
adding the worker alone does not change that. The supported candidate hybrid
composition retains Python Knowledge and the Python SDK worker with the native
Gateway, subject to their existing database/release gates. This is compatibility
guidance, not a newly implemented Python Gateway execution path or a verified
deployment. See [the operator topology](../deployment/sdk-agent-worker.md#optional-topology)
and [the source assembly handoff](../handoff/local-codex-acceptance.md).

The Web create form displays this requirement before its provider-key field.
Its unavailable/conflict/unconfirmed messages use the SDK locale namespace in
both English and Chinese, retain the attempted service ID, and prohibit automatic
replay or a legacy fallback. An HTTP 404/405/501 does not identify the running
Gateway implementation; operators must check the actual deployment and routing.

- `service_type: custom`, `connector_type: http`
- `metadata.adapter_type: openai_agents | claude_agent_sdk`
- `metadata.sdk_agent: {model, instructions, max_turns}`; turns 1 through 32
- `connector_config.sdk_provider_api_key`: write-only, server-side secret
- `session_enabled: true`, `max_retries: 0`, text inputs and invoke/stream modes

The user supplies a key and model; blank instructions normalize to the server default. Worker address and service token
are operator configuration, never browser fields. All roles, including admin, have
the SDK key removed from service detail. `metadata.sdk_provider_key_configured`
is a read projection. Updates omitting the key preserve the existing credential;
an explicitly invalid/empty replacement is rejected before persistence. No provider key
may be placed in public parameters, request context or metadata. The existing NativeProviderCipher seals a versioned, service-ID-bound credential
envelope using the host-injected provider encryption key. Missing configuration,
plaintext fallback or invalid ciphertext fails closed. Only the private adapter
decrypts it; both plaintext and ciphertext fields are absent from read projections.
The public API rejects user-supplied ciphertext.

The adapter ignores public `context` and rejects nonempty `parameters`, so a caller
cannot inject tools, environment, base URLs, SDK sessions or permissions. Authenticated
service dispatch supplies tenant/user identity and validates the Gateway session.
The existing public `session_id` never becomes a caller-chosen Claude resume ID.

## Operator/private contract

Gateway-only environment:

- Existing `GATEWAY_ENCRYPTION_KEY` is injected from host provider configuration; no new key is generated.
- `SDK_AGENT_WORKER_URL`: HTTPS origin, or literal loopback/localhost HTTP for isolated development
- `SDK_AGENT_WORKER_TOKEN`: at least 32 characters, injected from an operator secret source

Worker-only environment: same token, absolute private `SDK_AGENT_STATE_ROOT` and
mandatory finite `SDK_AGENT_RETENTION_SECONDS` (3600 through 7776000). The optional
[operator package](../deployment/sdk-agent-worker.md) uses a private durable volume
and a loopback-only sidecar; no default quickstart or public worker exposure.
Private `POST /v1/runs` requires bearer service authentication and the provider key
in sensitive `X-SDK-Provider-Key`. JSON fields are exactly `protocol`, `framework`,
`service_id`, `tenant_id`, `user_id`, `request_id`, `session_id`, `agent`, `input`,
`stream`. Protocol is `ai-platform/sdk-agent/v1`; `agent` contains the three public
configuration fields above. Do not expose this endpoint to end users. TLS/mTLS and
network policy must restrict the caller to Gateway; identity fields are only trusted
because the service-authenticated Gateway supplies them.

Blocking result: `{event:"completed",text,streamed_text,usage}`. Streaming uses SSE
`data` objects with `event:"delta",text`, followed by one `completed` object.
The final object is emitted after SDK state commit and child process exit. Failure
has `event:"error",code:"sdk_execution_failed"`; it never produces success. Gateway
maps only explicit completed to a final public chunk and keeps normalized usage.
`streamed_text` preserves all emitted text across intermediate SDK tool rounds;
`text` is the final SDK answer for blocking invoke. There is no guessed DONE sentinel.
`GET /health/ready` checks pinned packages, private state permissions and retention
sweeper health, not provider access. Private `DELETE /v1/sessions` requires the same
service bearer and canonical tenant/user/service/framework/session identity tuple;
it never accepts a path or caller-provided SDK resume ID.

## Durable scoped context and cancellation

Each session directory is SHA-256 of the JSON tuple tenant, user, service, framework,
platform session. One nonblocking filesystem lock serializes writers; busy sessions
return 409 rather than lose updates. OpenAI stores SDK items with SQLiteSession.
Claude keeps its private HOME/transcripts plus a server-written resume mapping. The
same persistent mounted volume must survive worker restarts; this is not a distributed
multi-worker session store. Session files must be 0700-parent protected, encrypted by
the deployment storage policy and included in deletion/retention implementation.

Request receipts bind request ID to canonical request bytes. Exact replay returns
the prior committed result, never a new SDK call. Changed input under the same ID
returns 409. A pending marker survives a crash, timeout or uncertain partial SDK
write. Further turns fail with `sdk_session_reconciliation_required`; this is safe
failure, not automatic crash recovery. The worker implements authenticated scoped deletion and bounded periodic finite
retention under admission/session locks. Expired/deleted scopes are durably tombstoned
before content removal; unknown runs are never silently replayed. Active scopes are
skipped by expiry and deletion returns409 until cleanup releases the session lock.
Public session deletion atomically records logical session deletion and durable
Gateway cleanup intent, delivered through the private worker deletion protocol.
This is not immediate physical erasure. See [SDK session cleanup](sdk-session-cleanup.md)
for admission barriers, retry semantics and required epoch-14 verification.
Provider execution and end-to-end deletion evidence remain rollout gates.
Operator recovery of unknown executions and backup expiry remain separate duties.

Every run is a separate process with a scrubbed environment, private stdin key and
no captured provider stderr. Execution has a five-minute budget for pure/Knowledge
runs and thirty minutes for governed runs, with bounded input/output. A strongly
owned supervisor requests process-group cleanup independently of response-task
cancellation. Cleanup observation is bounded; unresolved work keeps the session
lock/capacity and a durable refusal checked before replay, deletion and retention.
Lifespan observes owned cleanup and retention together and fails closed on expiry.
See the [shutdown contract](../deployment/sdk-agent-worker.md#execution-deadline-and-failure-ownership)
for restart refusal, legacy unknown state and commit/fsync failure ordering. These
asyncio budgets are not physical shutdown guarantees: signal delivery and leader
exit do not prove descendant or already-accepted remote provider termination.
Use a dedicated unprivileged worker container with read-only root, no host/Docker
mounts, bounded memory/CPU/processes, fixed package image and provider-only egress.
Do not run the worker on the user's workstation or install it into Gateway Python.

## Tools and remaining enterprise authority

The implemented tool profile is `pure_readonly_v1`: bounded arithmetic (AST
interpretation, no eval, imports, exponentiation or object access) and UTC time.
No Bash, file tools, network, external MCP, plugins, skills or remote executors are
available. The SDK genuinely selects tools and continues its official loop.

The [governed SDK capability authority](sdk-capability-authority.md) now has a paired
source implementation: genuine SDK runs, immutable selected snapshots, renewable
owner fences, the existing durable approvals/execution ledger, and a narrowly
supported Worker authority branch. It remains behind paired schema/native/Runtime/
Worker release gates. It never creates fake Codex lineage or issues a signing
credential from Python. Selected governed tools require streaming and the existing
authenticated exact-action approval surface. Pure and explicitly scoped Knowledge
reads remain available independently.

## Verification and release gates

External tests exercise contract validation, pure tools, official SDK invocation
shapes through doubles, session partitioning/resume options, host authentication,
request replay, no secret echo and a persistent truncation barrier. No actual SDK
package import, provider call, Rust compilation, worker launch, Docker or browser
acceptance was performed by this package.

Before enabling: compile native adapter; resolve/lock dependencies; inspect bundled
binary/licenses; run SDK-version tests; prove multi-turn memory for both real SDKs;
prove cancellation/unknown outcome behavior, tenant boundaries, credential rotation,
service revocation, quota/billing usage and session deletion/retention; inspect admin
GET and every service export for key redaction; execute end-to-end UI journey. All
are required evidence, not inferred from these unit tests.

## SDK usage preservation and existing accounting

The staged worker emits usage.sdk_usage.state as reported or unavailable. Missing,
partial, invalid or overflowing counts are never coerced to zero. Explicit measured
zero remains valid. OpenAI requires pre-normalization raw usage from every actual SDK response; neither
initialized aggregate zeroes nor a nonzero request counter establish usage. Claude retains the reported cache
components and total_cost_usd in bounded audit metadata; absent cache components
leave token totals unavailable. Cost availability is independent of token availability.

The private adapter pins model/framework and the genuine governed assistant_run_id
from its admitted lease. It never accepts a worker-selected run ID. Sync results,
stream statistics, session history and dispatcher usage metadata preserve this
observation. Cancellation records local cancellation separately from the unknown
provider outcome; it does not claim that a remote paid request was cancelled.

Only complete reported token counts with explicitly zero cache components enter
the existing dispatcher PricedUsageSink,
using the configured model and SDK's fixed provider transport (OpenAI or Anthropic).
Existing scoped pricing receipts, request-identity deduplication and quota/aggregate
updates remain the sole monetary path. Reported provider cost is audit evidence,
not an additive charge or an override of platform pricing. Cached or unknown-cache usage is preserved but withheld from this two-rate sink,
rather than charging cached tokens at ordinary input rates. Cache-aware monetary
and quota reconciliation is a remaining release gap. No prices,
provider registrations, extra charge calls, new tables or retry paths are introduced.

Unavailable usage skips token accounting rather than fabricating a zero or estimate.
The existing non-null token record schema cannot represent that absence. The
non-monetary audit journal below now records durable reconciliation scope before
provider dispatch and preserves available terminal observations independently of
session history. Pure/KB-only SDK runs still lack a genuine assistant_runs parent;
this does not create fake run lineage. Precise cache pricing/quota reconciliation,
production database guarantees and provider acceptance remain release gates.
Worker and native adapter should ship together to preserve the observation envelope;
a missing envelope is treated as unavailable accounting without losing the answer.

### Durable SDK observation journal

The adapter uses existing gateway.audit_logs with event_type
sdk_usage_observation and resource_type sdk_request. Canonical grants-policy.json
already grants the Gateway role SELECT/INSERT plus sequence USAGE; bootstrap uses
the existing Gateway data pool. No table, role grant or billing schema is added. The paired epoch-16 non-unique
partial lookup index is required before enabling these writers; see
[SDK audit index](sdk-usage-audit-index.md).
Every SELECT binds tenant, user, deterministic resource identity and phase. The
existing admin config-audit reader restricts event_type to config_changed, so these
observations are not exposed through that endpoint or to other account users.
Operators with authorized database access can inspect this existing audit store;
there is no new public audit-listing endpoint in this slice.

A successful admitted observation is committed before the private worker HTTP
request. Failure or uncertain admission commit stops dispatch. A deterministic
SHA-256 identity includes tenant, user, service, framework, session and request;
request/configuration changes under that identity are detected by a separate
canonical digest. Stored request summaries contain these identities, the configured
model/provider and genuine governed run ID when one exists. Prompts, instructions,
provider keys, grants, messages, tool arguments and tool output are never stored.

The Gateway serializes each identity using a transaction-scoped advisory lock,
checks the existing phase under that lock, and inserts only if absent. Exact
re-observation is a no-op; changed request/phase payloads or duplicate existing rows
fail closed. Existing worker request receipts and pending markers still decide
replay and recovery; the audit journal neither reruns a provider nor returns an
invented cached answer. No automatic retry follows a failed/uncertain audit commit.
All journal paths are INSERT/SELECT only, not charges or usage/quota mutations.

Completed observations preserve bounded allowlisted token/cache counts and reported
provider cost, including missing fields and platform_pricing_state. Local failure
and cancellation are separate immutable phases with unknown provider outcome.
Terminal persistence is task-owned once started, so cancelling the waiter does not
cancel the write. Process termination or database outage can prevent a terminal
write, but the admitted row remains durable for reconciliation by the original
worker request scope. A terminal write failure preserves the completed answer and
sets usage.sdk_usage.audit_persistence to unresolved_admission; success returns
durable. Neither value asserts that the separate priced sink charged successfully.
Worker receipts remain governed by existing finite retention; operators must
reconcile unresolved admissions before their evidence expires.

For operator reconciliation, filter audit_logs by event_type, resource_type,
tenant_id and user_id; use the returned audit_resource_id to group phases. An
admission without a completed phase is unresolved, as is a completed observation
whose usage_state is unavailable or platform_pricing_state requires reconciliation.
Compare compatible completed observations with the existing request-identity usage
record before any manual adjustment. Never charge the reported provider cost on
top of a platform usage record. Journal rows use status reconciliation_required as
an audit classification, not an assertion of debt or billable zero.

Offline verification includes actual SQL SELECT/INSERT strings executed against a
disposable SQLite audit schema for duplicate, collision, cancellation, restart and
failure cases. Rust unit tests cover scope hashing, sanitization and exact duplicate
comparison. SQLite transaction serialization does not prove PostgreSQL advisory
locks, deployed privileges or distributed behavior; those paired runtime/database
acceptance gates remain mandatory. No live billing, provider or database acceptance
is inferred from these checks.


## Typed saved-service SDK settings editor (2026-10-13 candidate)

The existing service configuration dialog's Basic tab now opens a typed official
SDK settings editor for OpenAI Agents SDK and Claude Agent SDK registrations.
It reads the saved service before enabling edits, keeps framework identity fixed,
and edits only model, instructions, max turns and an optional replacement provider
key. The displayed last-read model/turn limit and editable draft are separate.
Blank instructions normalize to the native default. Validation applies native
UTF-8 byte limits and the 1–32 turn range before any update is sent.

A blank replacement omits `connector_config` entirely; the existing native PUT
path retains its sealed server-side credential. A nonblank replacement sends only
`sdk_provider_api_key` and is cleared from the form immediately. Neither existing
raw keys nor ciphertext are accepted in the editor's read projection or copied
into an update. Public metadata is preserved, including `sdk_tools`; the computed
`sdk_provider_key_configured` read marker is not written back. This is the native
SDK contract, not a new SDK execution or encryption feature for retained Python.

Save performs one explicit PUT followed by a GET. Accepted settings are shown as
confirmed only if the returned framework/model/instructions/turn limit match.
A write failure is an unknown outcome, not proof that nothing changed; a failed
readback or mismatch also disables resubmission until the user explicitly reloads
saved settings. Reload discards the draft and clears a replacement key. There is
no automatic credential replay, invocation or provider test. Credential identity
cannot be read back: an accepted replacement and configured marker do not prove
provider validity or equality with a particular secret. Current runs retain their
existing admission, while subsequent admissions use the saved configuration.

Service, credential and authorization changes remount the editor; late reads and
writes cannot publish into the new view or cause a stale handler to submit again.
The server remains authorization authority. Metadata updates are whole-object
replacement with no ETag/CAS; this editor does not claim multi-admin conflict
protection. Reload before editing a service changed elsewhere.

Focused offline tests execute the production helper and form with controlled
React/HTTP substitutes. They cover both frameworks, preserved tool/knowledge
selection, omitted and explicit replacement credentials, input limits, readback,
unknown writes, duplicate submission and account/unmount boundaries. They do not
prove real React/browser scheduling, database persistence, real key rotation,
provider connectivity, SDK execution, retained-Python parity or release acceptance.


## Private child launch framing

The private worker-to-child stdin frame has one shared codec and a 512 KiB total
budget, including the complete request, provider credential, JSON envelope and
final LF. Public request-body admission remains 393216 bytes, with existing
UTF-8 field limits and credential validation unchanged. Compact UTF-8 encoding
preserves Chinese/emoji requests that previously expanded past the child's
independent 400000-byte ASCII-escaped readline limit. The entire serialized frame,
including capability schemas and metadata, is admitted before session acquisition,
profile/pending writes, authority calls or subprocess creation. Oversize returns
the existing `sdk_request_too_large` response; malformed/nonfinite/invalid Unicode
returns the existing invalid-request response. No partial launch is attempted.

The parent closes stdin after its one frame. The child reads at most budget+1
bytes through EOF, rejects oversized, missing-delimiter, malformed or additional
frames, and revalidates the Launch contract before state access or SDK execution.
Decode failures produce only the fixed execution-failed event, without input or
credential contents. The private size cap does not widen the public endpoint,
provider access, session replay semantics or output budgets. Deployment must use
a consistent server/child package version; no mixed-version handshake is added.

Controlled tests cover both language examples, full-envelope exact/max+1 limits,
metadata expansion before any session/spawn, actual child entrypoint failure,
secret-safe errors and the existing ownership/cancellation regressions. These
checks passed: 46 tests and 35 subtests, zero failures/skips. Scoped Ruff comparison
found zero new diagnostics; four pre-existing diagnostics remain (both lint commands
exit 1). The initial missing-Uvicorn attempt and test-only formatting repairs are
retained in the evidence. The shutdown fixture used the existing Uvicorn 0.52.1 and
Click 8.4.2 fallback with exact file/import bindings; optional worker uv.lock pins
Uvicorn 0.54.0, so this is not locked-worker dependency acceptance. No live SDK/provider
run or subprocess/process-group acceptance is claimed. The complete frame budget
does not bound every temporary JSON encoder allocation.
