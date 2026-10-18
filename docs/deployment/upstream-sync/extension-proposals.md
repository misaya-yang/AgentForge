# Local upstream extension-point proposals

These are concrete API proposals for upstream discussion. No upstream file is
edited, no patch is applied or published, and no proposal is recorded as adopted.
The current local adapters keep the upstream kernel as the sole model/tool loop.
The read-only [39-file observation](current-patch-observation.json) is the measured
starting point; the five proposals below do not themselves establish reduction.

## 1. A fallible asynchronous durability fence before a model send

Existing seams: `codex_extension_api::RequireDurableThreadStore`,
`HostThreadStartupData`, `ModelRequestContributor` and
`AppServerHostRuntime::with_thread_store`. The current
`ModelRequestContributor::request` is synchronous and documented not to delay
inference. Its response interceptor is appropriate for observation, not for
making an authoritative persistence barrier succeed before a provider effect.

Propose a separate host opt-in trait in extension-api, with upstream-owned
request identity and opaque acknowledgment:

```rust,ignore
pub struct BeforeModelSend<'a> {
    pub thread_id: ThreadId,
    pub turn_id: &'a str,
    pub request_id: &'a str,
    pub kind: ModelRequestKind,
    pub cancellation: &'a CancellationToken,
}
pub trait ModelSendAdmission: Send + Sync {
    fn admit<'a>(&'a self, input: BeforeModelSend<'a>)
        -> ExtensionFuture<'a, Result<ModelSendPermit, ModelSendRejection>>;
}
```

Upstream calls it after the exact input/history is frozen and appended, before
acquiring/sending the provider request. A permit is single-request, opaque and
non-serializable; it confers no tenant/provider credential. Dropping the future
or rejecting cannot send. A write/flush failure is surfaced through the existing
turn failure path; no retry or new terminal vocabulary is introduced. Warmup has
an explicit kind and cannot impersonate a committed generation request. A failed
or unknown provider send must not re-enter admission as a fresh safe retry.

Platform migration: keep the PostgreSQL role/fence/lease check and durable marker
inside `PostgresThreadStore`; install one contributor from
`AiPlatformExtensionRegistry`, leaving `telemetry_model::ModelTelemetry` as
observation. Remove bespoke dispatch checks only after upstream has the ordering
contract and kill-at-every-boundary tests. A source grep is not equivalent to a
failure-before-send witness.

Relevant currently modified upstream paths: `core/src/model_request.rs`,
`core/src/session/mod.rs`, `core/src/session/turn.rs`, `core/src/compact.rs`,
`core/src/compact_remote_v2.rs`, and the extension-api model-request/marker files.
The upstream patch should centralize ordinary and compaction sends at the same
admission seam, rather than expose platform SQL, signed metadata or a platform
loop upstream.

Required tests: rejected/failed flush yields zero provider requests; cancellation
while admission is pending yields zero sends; successful marker precedes one
send; reused permit rejects; compaction and retry use correct request identity;
standalone sessions without the host opt-in preserve current behavior.

## 2. A host-supplied remote thread store without public protocol changes

Existing seams: `AgentKernel::start`,
`InProcessAppServerClient::start_with_host`,
`AppServerHostRuntime::with_thread_store`, reserved
`AppServerThreadStartOptions`/`AppServerTurnStartOptions`, and the platform
`PostgresThreadStore`. Propose upstreaming this dependency-injection boundary as
an optional in-process host feature with local file storage as the default.

Keep typed `ThreadStore` async persistence, ordered append IDs, immutable payload
hashes and reserved thread/turn identity in a small storage crate. Host startup
resolves ownership before creating the native session. Store failures propagate;
there is no fallback to local files or a second SQLite authority. A resume reads
the same history and original root/parent relationships. Public thread/start,
turn/start, events and schema remain unchanged; host-only options are never
accepted from an untrusted RPC payload.

Platform migration: retain tenant/user/session membership resolution and native
PostgreSQL ACLs in the platform adapter. Return upstream-neutral native rollout
items after validating/stripping the existing private writer envelope. Keep
source/schema provenance, N-1 decoding and stop-write/restore policy outside the
storage API. The upstream contract must not know a tenant table name, role,
capability grant, model-plane lease, Redis or the Gateway.

Candidate upstream migration group: `app-server/src/in_process.rs`,
`app-server/src/message_processor.rs`, request_processors/thread_processor.rs,
request_processors/turn_processor.rs, `app-server-client/src/lib.rs`,
`core/src/thread_manager.rs`, `core/src/codex_thread.rs`, and
app-server/tests/suite/v2/remote_thread_store.rs. Existing one-line import/test
adjustments can disappear only after the exact dependency-injection API is
upstream, demonstrated by a new target-vs-overlay measurement.

Required tests: remote append/read identity and order; duplicate append equality
versus conflict; missing membership rejects before native thread allocation;
partial append failure never starts a model; child-root inheritance; cold resume
using actual N-1 history; upstream standalone file backend regression.

## 3. Suspension and recovery keep the same native turn identity

Existing seams: upstream turn suspension, `AppServerTurnStartOptions::recovery`,
`TurnLifecycleContributor`, `PlatformLifecycleContributor::recover_unclosed`,
and the durable platform approval/cancellation records.

Propose a host-neutral `SuspendedTurn { turn_id, continuation_token, reason }`
notification plus `resume_suspended(turn_id, continuation_token, admission)` on
the in-process request handle. The continuation is an opaque hash-bound native
phase token, not a new user message. Native suspension yields without emitting a
completed/failed turn and without holding a database transaction. The kernel
retains responsibility for continuing its own loop.

On recovery the host must re-admit the same turn/arguments and original absolute
deadline. The adapter reacquires the actual owner fence; this is platform
permission to resume, not a license for the kernel to bypass authorization.
Cancellation is durable-first and wins over a later approval. A dispatched write
with no terminal receipt remains `side_effect_unknown`; recovery cannot replay
it merely because the process-local waiter vanished. Rejected/stale tokens do
not reset deadlines, submit user input or create another turn.

Candidate group: `core/src/session/turn_suspension.rs`, session/handlers.rs,
session/turn_input.rs, app-server turn processor/in-process client, and the
existing lifecycle contributor seam. Reuse existing terminal authority; add no
new externally visible status to solve an internal continuation problem.

Required tests: approval waiter lost then exact durable decision readback;
cancel/approve race; original budget expiry during suspension; owner death before
and after tool dispatch; stale continuation/fence rejection; no second model
send or write replay when the prior external effect is unknown.

## 4. One tool lifecycle admission/result seam across handlers

Existing seams: `ToolLifecycleContributor::{before_tool_dispatch,
on_tool_finish}` and its direct-call, post-hook, command/MCP observation
callbacks; `ToolDispatchInput`, `ToolStartInput`, `ToolFinishInput`; platform
`PlatformLifecycleContributor` and canonical receipt reconciliation.

Propose a single fallible **pre-effect** admission invocation after the final
post-hook arguments and concrete effect are known, before any handler readiness
side effect or broker post. Pair it with a finish callback owning a stable
`(thread, turn, call, attempt)` identity and a discriminated actual outcome:
completed, rejected-before-effect, cancelled-before-effect, or outcome-unknown.
Keep direct blocked-call observation distinct from effect admission. Finish
callback failure never converts an uncertain effect to a safe retry.

All concrete handlers route through the same registry seam. Preserve existing
namespace/tool schema collision checks; contributors cannot call a hidden second
handler or synthesize effect success. Canonical exactly-once/owner-fenced terminal
storage remains platform code. Post-hook argument hashes and approvals must bind
the final dispatched bytes; pre-hook text is insufficient.

Candidate group: `core/src/tools/lifecycle.rs`, tools/registry.rs,
tools/handlers/dynamic.rs, tools/handlers/mcp.rs,
`tools/src/tool_executor.rs`, tools/src/lib.rs and extension-api lifecycle types.

Required tests: each handler emits admission once; rejection sends zero broker
requests; callbacks observe final normalized args; concurrent finish/cancel cannot
overwrite a canonical terminal; dropped result stream preserves unknown; direct
blocked call is observable without inventing a dispatch receipt.

## 5. Immutable tool policy ceilings inherited by child sessions

Existing seams: `HostThreadStartupData::tool_policy`, `ToolPolicy`,
`AiPlatformExtensionRegistry::install`, `readonly_extension` contributors and
`PostgresThreadStore::resolve_host_startup` through tool_policy.rs.

Propose host-only `ToolPolicyCeiling` captured before native session construction,
containing stable tool IDs, schema hashes, allowed effects and revision. Every
later contributor can only intersect the ceiling. Deferred discovery resolves a
descriptor under the captured ceiling; it cannot expand it. Child sessions inherit
the parent ceiling before spawn. Resume compares the supplied ceiling to the
original immutable capture and rejects widening or rebinding. This is an allowlist
boundary, not a second tool-selection loop or a generic shell permission bypass.

Platform migration: keep Knowledge/MCP/attachments, tenant binding, approvals,
lease proofs and actual broker execution in platform adapters. Upstream only
normalizes/intersects the neutral descriptor set and preserves attribution.
Candidate group: `core/src/agent/control/spawn.rs`, core/src/thread_manager.rs,
`core/src/turn_metadata.rs`, extension-api/tool_policy.rs and host startup data.

Required tests: root/child equality; deferred tool cannot widen schema/effect;
resume under changed catalog revision fails; final normalized arguments retain
scope; unrelated upstream CLI configurations remain unchanged.

## Two-window migration sequence and accounting

Window 1 proposes/adapts the host store/startup and common lifecycle/ceiling
contracts. Window 2 targets centralized pre-model durability and suspension
continuation. Each proposal must first be accepted upstream and become available
in a stable tag before its equivalent overlay fragments may be retired. No
proposal acceptance, PR merge or future file-count reduction is assumed here.

Use each monthly report's complete modified-path inventory to assign every path
to one of these groups or an explicit retained patch. Count Cargo.toml/Cargo.lock,
protocol and test modifications; added platform crates do not cancel them out.
An upstream-adopted hook may remove many patch sites, but its remaining adapter
still counts if it changes a file present in that target. The measured 39 remains
above 15 until actual immutable source-unit diffs prove otherwise across the two
registered windows. Unmergeable/conflicting paths and elapsed repair time remain
visible even if the final candidate compiles.

The present repository has the extension seams above but no adopted-upstream
receipt for these proposals. Accordingly this package completes reviewable local
proposals and fail-closed measurement/admission code, while the continuous
calendar windows and real acceptance remain open in the sole program ledger.
