# Governed capabilities in official SDK runs

Status: staged source implementation, not production-accepted. Native, Runtime,
Worker and epoch 17 must ship as a paired change. No provider, live database,
Runtime/Worker compilation or distributed recovery acceptance is claimed here.
The existing Codex Assistant remains unchanged and the default managed runtime.

## Authority and schema

The baseline CHECK requires non-`agent_runtime` assistant_runs to have NULL harness
thread, turn, snapshot, kernel and capability-revision fields. Runtime snapshots,
model leases and owners have real Codex-thread foreign keys. Epoch 15 preserves
all those CHECK/FK contracts. It adds assistant_sdk_runs with a genuine
`openai_agents` or `claude_agent_sdk` assistant_runs parent, plus immutable scoped
SDK tool-call records. SDK owner IDs are not Codex thread or turn IDs.

Authenticated Gateway invocation issues only an in-memory provenance bearer tied
to its original JWT, tenant, user, service and request. The JWT never leaves Gateway.
Every official SDK invocation, including pure and Knowledge-only profiles, now
uses that same durable parent, owner lease and terminal lifecycle. Additive epoch 17
allows an explicit empty capability snapshot without modifying immutable epoch 15.
Missing, malformed or nonempty descriptor arrays cannot masquerade as empty authority.
Empty selection skips catalog resolution and exposes no governed tools; it grants
only run admission/observation. The Gateway-only function signature and grants,
Codex foreign keys and snapshot restrictions remain unchanged.

After ordinary authorization/session admission, Rust resolves explicitly selected
capability IDs through the existing tenant/current-policy/Worker catalog, pins the
exact descriptors and a canonical sorted-serde-JSON hash, and creates a durable run.
The opaque bearer alone cannot authorize a capability. Request context, metadata or
the SDK cannot choose a run ID, owner, snapshot, effect, schema, approval or lease.

Run owner leases renew every 10 seconds for at most five minutes for pure/KB
profiles and 30 minutes for governed profiles; each lease lives 30
seconds. Renewal and calls reauthenticate the JWT and recheck service/session,
AgentInvoke, tenant catalog and selected capabilities. The SQL assertion repeats
scope, engine, owner fence, expiry, revocation and canonical session cleanup state.
Service status and SDK configuration checks hold a shared service-row lock through
Worker dispatch. Session deletion revokes the run and its pending approvals through
the existing durable cleanup barrier. Admission respects barrier → session → run →
owner lock order. SDK process-side authority checks stop the child when admission
is lost. Provider requests or already-dispatched external actions may still finish;
this is not a guarantee of downstream cancellation.

## Reused execution and approval contract

Private Runtime POST /internal/v1/sdk-runs/{run_id}/capabilities receives scoped
internal headers and the Gateway owner/fence, then typed `{tool_call_id, tool,
namespace, arguments}`. It uses the existing descriptor resolver, extracted durable
approval persistence and transport-independent signed CapabilityWorkerClient
execution path. Neither DynamicToolCallParams nor PlatformThreadIdentity is
fabricated for SDK calls. The old Codex wrappers retain their identity checks.

Every call pins name, namespace, canonical arguments/hash and original run. Writes
and unknown effects persist the original assistant_tool_approvals row before
returning approval_required. The authenticated owner GET/POST
/api/v1/assistant/approvals/{id} exposes the existing exact-action preview and
decides that same durable row. Expired or inactive pending approvals are projected
as expired/cancelled; terminal lifecycle events retain approval_status separately
from execution status. Existing scoped run-status and task-cancel routes also
recognize genuine SDK runs. Unsupported previews cannot be approved. Polling
reuses its ID/deadline; changed arguments fail. A consumed approval permits only
observation of its original Worker execution. The Worker still verifies the
signed lease, exact snapshot descriptor, scope, effect, hash, single-use approval,
owner fence and execution receipt, and existing downstream brokers are unchanged.

The Worker has a separate SDK production-authority branch. The existing Runtime
SQL engine predicate is not broadened. Its atomic dispatch guard binds the exact
SDK snapshot and durable invocation. Prepare-before-send records and immutable
responses establish an unknown-outcome barrier. A write with uncertain dispatch
is not replayed. The original execution ID is retained when known; late receipts
cannot silently overwrite an already-reported unknown result. Worker reservation
and atomic dispatch reject subsequent actions once any original unknown receipt
is visible for that run. Exact original receipt observation remains available.
This barrier cannot cancel actions already dispatched or in flight.

## Selection and SDK transport

metadata.sdk_tools accepts knowledge_dataset_ids and capability_ids independently,
with at most 16 unique entries each. Capability selection contains identifiers only;
server descriptors are the authority. The existing assistant/tools catalog now
projects capability_kind, read_only, effect and approval_policy for selection UI.

Only selected capabilities become SDK tools. Their stable model-visible names are
platform_capability_N and map to the exact pinned ID/name/namespace/schema.
OpenAI receives the real ToolContext.tool_call_id. The pinned Claude SDK's custom
MCP handler exposes arguments without the tool-use ID, so an owned PreToolUse hook
binds the genuine SDK tool_use_id to name+canonical arguments. Ambiguous concurrent
identical Claude calls fail before dispatch; no synthetic tool-use ID is invented.
Built-in tools/settings/external MCP remain disabled. Claude's SDK-local permission
allowlist permits only these bridge wrappers and does not approve external actions.

Runtime-context tools are excluded: read_attachment, search_knowledge_base,
context_compact, update_user_memory, tool_search, tool_describe, tool_call and
spawn_subagent. Knowledge continues through the existing explicitly selected ACL
revalidated knowledge_dataset_ids route. Read descriptors requiring approval are
rejected because the existing Worker read-approval shape does not support them.

Governed services require streaming; sync invocation fails before dispatch and the
UI must not silently fall back to sync. The existing StreamChunk protocol carries
approval_required, approval_result and tool_result with metadata.sdk_lifecycle.
There is no fake runtime_thread_id. Approval preview contains exact bounded action
fields and a hash; raw arguments and private grants are not emitted. approval_result
status is the actual terminal execution status, not an assertion of approval or
success. Only a committed official SDK completion ends the stream successfully.

Pure/KB-only runs retain their five-minute worker deadline. Governed runs use a
30-minute hard deadline, with at most ten minutes for each durable approval.
Claude custom tool execution timeout is bounded to 700 seconds and automatic MCP
backgrounding is disabled for predictable synchronous receipt ownership. These
operator-set SDK options are described in the official
[environment variable reference](https://code.claude.com/docs/en/env-vars) and
[MCP timeout guide](https://code.claude.com/docs/en/mcp). No browser-supplied SDK
options are forwarded. HTTP transport uncertainty or a side_effect_unknown receipt
poisons the run: even if an SDK converts a callback error to model-visible text,
the process cannot issue more governed calls or commit successful completion.
Execute requests are serialized and recheck poison at every boundary, including
a pending approval poll, so a waiting sibling cannot dispatch after another call
reports an unknown outcome. Durable dispatch checks enforce the same boundary
independently of this process-local flag.

Cancellation while awaiting an execute request also poisons the run before releasing
that boundary lock. Cancelling `asyncio.to_thread` does not stop its synchronous
HTTP request: the remote action can still finish and a late receipt cannot unpoison
the run. Queued and future governed calls must fail without another dispatch, and
the existing engine completion check rejects success if callback cancellation is
suppressed upstream. Cancellation while only waiting for the lock does not poison
the run because that call has not crossed the execute boundary. This does not add
remote cancellation, a total HTTP deadline or automatic replay. Focused tests in
`tests/unit/test_sdk_bridge_cancellation.py` use production bridge functions and
bounded, explicitly released in-process HTTP doubles; no provider or distributed
cancellation acceptance is implied.

## Gateway-to-Runtime correlation

The SDK capability-only bridge uses Gateway's existing bounded internal correlation
helper after current JWT/service/session/run/owner authorization. It adds the safe
request ID, an installed active traceparent/tracestate carrier, and the genuine
SDK x-ai-run-id. It does not create managed x-ai-turn-id or Worker execution identity
from an SDK tool-call ID. Raw incoming trace headers, baggage, authorization, cookies
and caller-supplied identity/owner fields are not copied through this helper.
A deployment without a native active tracing adapter forwards request correlation
without claiming a complete OpenTelemetry span chain.

The configured Runtime service token retains its sensitive-header flag; admitted
tenant/user/session, stored owner/fence, URL, exact tool-call body and 45-second
request timeout remain unchanged. Correlation never signs or mutates a capability
proof. The one POST send has no automatic replay on an uncertain outcome. Original
post-response current-authority revalidation remains mandatory.

Focused request-builder and loopback tests cover this transport seam and negative
correlation/header substitution cases. These are separate from actual Runtime auth,
JWT/service revocation, owner-fence, approval, replay and paired Worker acceptance.

## Recovery and release gates

The bounded initial recovery behavior is deliberately conservative. Duplicate
Gateway request IDs do not create another SDK run of any profile. A Gateway/SDK restart with
an unfinished worker pending marker requires operator reconciliation; it does not
silently resume a paid SDK loop or repeat approved effects. A same-run Runtime call
can reuse its durable approval and exact Worker receipt while its owner is live.
This implementation is not automated cross-Gateway restart continuation. Database
journal retention remains subject to the platform's operator data-retention policy;
worker transcript deletion/finite retention does not erase capability audit receipts.

Required release gates: measure canonical epoch15 structural/ACL/extension/reference
fingerprints after epoch14; verify distinct Gateway/Runtime/Worker grants; compile
native and hosted Runtime/Worker modules; exercise actual OpenAI/Claude hook/tool IDs,
approval waits and UI; durable tests for rejection/expiry, cross-tenant/run/name/args
substitution, single-use consumption, concurrent duplicate, owner loss, service and
session revocation, unknown outcome and restart reconciliation. Offline source and
SDK-double tests do not prove these database/distributed/provider behaviors.

### Reviewed SDK action binding

SDK approval POST accepts optional `expected_run_id`, `expected_tool_call_id`,
and `expected_arguments_hash` as an all-or-none triple. Native CLI decisions
require this triple from the inspected action and a fresh authenticated preview.
The Gateway rejects malformed/partial bindings (422) and changed identities (409),
and passes the reviewed hash into the existing transactional approval function.
SDK call identity and established approval linkage remain immutable. Legacy
requests without the triple stay compatible; non-SDK decisions carrying it are
rejected rather than silently ignoring the binding. Mutations are single-attempt;
inspect state after an uncertain response instead of automatically retrying.

The Web SDK card also requires this triple for both approval and rejection. It
compares the freshly fetched authenticated identity/hash with the action shown to
the user before sending a single decision POST. A changed identity/hash requires
review again; absent or malformed bindings cannot be submitted. When an explicit
`can_approve=false` preview omits its digest, the authenticated approval row digest
may bind rejection only. Missing previews, malformed digests and disagreeing
row/preview digests fail closed. Approval still requires the complete actionable
preview; fresh status, expiry and owner checks apply to rejection too. Runtime cards
retain their existing callbacks. Account/token/source fences, expiry and duplicate
submission protection remain in force; a failed POST only refreshes the preview.

### Common task identity and zero-tool authority

The existing private capability_bridge carries a genuine server-created run_id for
all profiles. Its tools array is empty and timeout_seconds is 300 for pure/KB runs;
only a nonempty list allows governed tools, tool lifecycle events, or the 1,800-second
deadline. Pure sync calls remain supported. Empty lifecycle bridges do not change
worker session tool profiles or silently authorize Knowledge reads. Knowledge still
requires its separate read_bridge, explicit dataset selection and current ACL checks.

The worker rechecks durable authority before starting, periodically while running,
and immediately before committing a terminal receipt. That terminal check uses
non-consuming durable execution admission, preserving the Knowledge grant quota
(server + child admission and before/after retrieval checks). It revalidates current
principal/service/session/run authority; existing per-dataset ACL checks remain at
Knowledge admission and retrieval. Existing run-status, cancel,
owner-fence, session-delete and audit relationships therefore apply to every SDK
execution. Stream content metadata and terminal usage carry the genuine
assistant_run_id; worker-provided IDs are never trusted. Failure signals failed;
body drop/disconnect signals cancelled. These are local lifecycle statuses, not
proof that a provider has stopped or incurred no cost. No automatic retry/resume
is introduced; repeated durable request identities require reconciliation.

After session/run/audit admission and successful worker HTTP response headers, the
Gateway emits one empty, nonterminal `text_delta` carrying `sdk_framework` and the
lease-owned `assistant_run_id`, before reading any worker body bytes. This applies
to pure, Knowledge-only and governed profiles. Lifecycle events carry the same
trusted top-level identity alongside `sdk_lifecycle`; its existing run/tool checks
remain required. The early chunk contains no usage and does not complete the run,
audit or provider request. Content order and the single terminal usage chunk are
preserved. Waiting for worker headers still delays identity publication; this is
not pre-dispatch acknowledgement or proof of provider progress.

Epoch 17 is additive CREATE OR REPLACE of admission only. Existing binaries remain
DB-compatible; new Gateway and worker require a paired rollout after migration.
Canonical PostgreSQL fingerprints/role tests must be measured before acceptance;
source and SDK-double tests are not distributed/provider verification.

Epoch 17 has now passed disposable PostgreSQL16 canonical prior16 verification,
upgrade/fresh/replay fingerprints and sequential real Gateway/Runtime/Worker role
checks. Pure/KB parent identity, five-minute expiry, malformed-empty rejection,
cancellation/session-delete revocation and no-tool dispatch denial were exercised.
The measured verification record preserves prior ACL/extension/reference hashes.
This is offline SQL evidence, not provider, multi-connection race or live rollout
acceptance; native compilation of this delta remains a separate gate.

### SDK run status and usage observation

SDK runs with established durable lineage use the existing owner-scoped
`GET /api/v1/assistant/runs/{run_id}` and
`POST /api/v1/assistant/tasks/{run_id}/cancel` routes. The generic `/api/v1/tasks`
queue is a different contract; its IDs and `processing` status must not be used
as SDK run IDs/statuses. SDK runs never acquire a Codex harness thread or turn ID.
`platformctl runs get ID` includes sanitized observed usage; `runs cancel ID --yes`
retains its single-attempt cancellation and unknown-outcome guard.

Run status resolves the existing existing SDK identity to the epoch16 audit resource
hash using tenant/user/service/request/session/framework, then performs four
index-bound phase lookups, each limited to two rows to detect duplicates. It does
not scan audit JSON by run ID, create a new task store, write the journal or require
epoch17. Matching admission/run/scope and exactly one terminal observation are
required for reported observations. Missing index/history, lookup failures and
conflicting observations leave status available but usage explicitly unavailable.
A missing terminal observation may become available on a later read.
Optional usage enrichment has a one-second total client deadline including pool
acquisition, plus transaction-local 250ms statement and 100ms lock timeouts.
Timeouts return unavailable usage while preserving authoritative run status.
Cancellation skips audit enrichment entirely after its lifecycle mutation, so
an audit lock cannot convert a committed cancellation into an unknown result.

The additive `run.usage` follows the existing usage envelope: available numeric
fields plus `sdk_usage`. A reported usage state requires valid nonnegative input/output token counts, a
non-overflowing sum and agreement with any supplied total. Terminal observation
phase must match its local execution status. Provider token/cache measurements and provider cost are
observations, separate from platform pricing and accounting. Missing values remain
absent, `platform_pricing_state` preserves reconciliation requirements, and
`accounting_status` is always `not_asserted`. Cancellation does not establish
provider termination or zero cost. Web run-status typing admits SDK engine and
`sdk_authority_expired`; this change does not add a new Web run browser or change the separately implemented
epoch17 pure/Knowledge-only admission and rollout requirements.

Source-level tests cover indexed query/scope and both projection allowlists. Rust
unit tests and authenticated HTTP/provider/UI acceptance remain required; source
checks alone do not establish deployment, concurrency or billing acceptance.

### Governed argument byte admission

The official SDK bridge measures the 64 KiB argument limit using compact UTF-8
JSON, consistent with Gateway/Runtime argument admission. Unicode escape expansion
and optional JSON whitespace must not reject an otherwise bounded multilingual
action before its durable approval is created. This changes local byte accounting
only; the HTTP envelope retains its existing JSON encoding. Immutable
argument/hash binding, approvals, actual transport response bounds
and unknown-outcome fencing remain unchanged. Production-import unit tests cover
approval polling, successful and rejected receipts, exact-boundary payloads and
oversize rejection; these are in-process transport-double checks, not live paired
Gateway/Runtime/Worker or official provider acceptance.


## Private bridge response ownership

Knowledge and governed capability HTTP bridges close response bodies even when
`opener.open()` raises an HTTP error before entering a response context. The
original HTTP error and status remain available to existing admission handling;
error bodies are neither read nor emitted. Forbidden 301/302/303/307/308 redirects
close the original response before rejecting with the fixed
`sdk_gateway_redirect_forbidden` error, without following a Location or draining
its body. urllib's earlier malformed/missing Location HTTP errors retain their
original status and are closed by the same HTTP-error path. Redirect URL parsing
failures also close the owned response before propagating the existing error. Successful responses retain their existing 1 MiB read limit,
35/50-second socket timeouts and context-managed close. Proxy denial, operator
origin validation and single-attempt unknown-outcome fencing are unchanged.

The focused offline response-ownership tests exercise the real urllib opener,
HTTP error processor, bridge functions and governed tool dispatch with in-memory
HTTP responses. They retain raised errors while checking exact body closure,
status preservation, no redirect dispatch, bounded successful/error reads and
no automatic replay after uncertain execution. Bounded offline validation passed
30 tests plus 64 subtests across response ownership, existing bridge cancellation
and Worker lifecycle tests. Scoped Ruff reported the same seven baseline diagnostics
in both trees (exit 1), with zero new diagnostics. Source, dependency and tool
identities were unchanged; the initial test-style lint failure is retained.
These results do not establish live TLS/socket behaviour, a total request deadline,
remote cancellation or provider/service acceptance.
