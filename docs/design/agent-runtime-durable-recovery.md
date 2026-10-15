# Agent Runtime durable execution and restart recovery

Historical status: local implementation and required acceptance verified on the
2026-09-26 DR-01 C3/C4 release identities, not on every later Runtime/Worker build. This extends ADR-006/007 and the
R1 J03 contract; it does not introduce another agent loop or migrate platform
governance to Rust. Evidence: [local acceptance](../../reports/product/runtime-durable-recovery-2026-09-26.md), R1 runbook and DR-01 receipt.

## Authority and recovery sequence

1. Rust acquires a renewable PostgreSQL run claim before Core admission. The
   claim has an instance ID and monotonically increasing fence. Every model
   call, tool dispatch, rollout write and terminal projection verifies that
   fence. Losing a claim stops the local turn; a stale owner cannot advance it.
2. Runtime persists the admitted execution context and each dynamic invocation
   before waiting or dispatching. Context pins the original run/turn, snapshot,
   model, tool policy and platform identity. It contains no provider credential.
3. Approvals are keyed by the original run/call/action. Restart reuses the
   original approval ID, deadline and durable decision. A process waiter is a
   notification mechanism, not decision authority. No decision is changed to
   cancelled merely because its old waiter disappeared.
4. Recovery reconciles the original Worker execution before dispatching. A
   terminal result is reused; a live execution is observed; a reserved but
   undispatched execution may continue under valid authority. Dispatched write
   or unknown actions with no trustworthy receipt remain unknown and are never
   replayed. Runtime must not issue a new lease/attempt for an existing execution.
5. The full original tool response is durably paired with its original Core
   call before cold resume. Result injection is idempotent. This precedes Core
   history normalization so an unfinished call is not silently replaced by an
   aborted result. Unsupported native/descendant recovery fails closed.
6. A private in-process host bridge invokes Core `recover_turn_if_idle` with
   the original turn ID, recovered tool outputs and signed model metadata. It
   does not submit user input or use public `turn/start` to simulate recovery.
7. Gateway revalidates the original signed model lease for the current claim,
   without changing the pinned model, provider revision, limits or usage counters.
   Expired or revoked original authority stops recovery rather than extending consent.
   Recovery generations distinguish model attempts after transport/process loss;
   they never authorize replay of a tool action or reset budget consumption.

## Persistence and concurrency

Additive migration adds Runtime execution ownership/context and invocation
journal records. The existing ThreadStore, approval table and Worker execution
ledger remain authoritative. Claim/renew/takeover, approval deduplication and
Worker dispatch checks are transactional. A takeover cannot use an unexpired
claim. Cancelled/terminal runs and revoked/expired snapshots cannot be reclaimed.
Old terminal runs are not resurrected. Historical runs without durable admission
context are reported as nonrecoverable; no guessed model or authorization is used.

All phases preserve tenant/user/session binding. Worker dispatch atomically
rechecks run state, snapshot revocation, model lease and Runtime fence together
with consuming approval. Heartbeats cannot renew an expired or superseded fence.
The current product deployment remains one Runtime instance; the ownership
contract is tested with competing executors, without claiming multi-instance SSE
affinity or full platform high availability.

Worker reservation and cancellation carry the Runtime owner/fence and hold the
original run/owner locks through their mutation transaction. Cancellation reads
the latest execution under a row lock before classifying an already-dispatched
write as unknown. Dispatch and revocation use the same run-first lock order.
Lease checks use wall-clock time after lock acquisition.

SIGINT/SIGTERM stops admission and local capability waiters, then invokes the
existing Core suspension API through a private host command. The Core marker
suppresses fabricated user-cancelled tool output during suspension. Pending
approvals, decisions and Worker receipts stay durable; shutdown does not send a
Worker cancel. The owner remains fenced until its lease expires, so a replacement
process cannot overlap with shutdown. Failure to suspend does not release ownership.
The empty V2 `:recover` endpoint only wakes reconciliation of the original turn;
it cannot change the model, input, authority or execution identity.

## Failure and cancellation semantics

Pending approvals retain their original expiry. Rejection produces a single
denial response and zero Worker dispatches. Cancellation prevents new work and
reconciles already-dispatched actions; it never claims that external effects
were rolled back. Revocation blocks lease renewal, new model calls and dispatch.
Transient recovery failures retain recoverable state and diagnostics; unsupported
or unprovable action recovery emits an explicit unknown/nonrecoverable outcome.
Model transport recovery may incur another model request under the original
budget, but persisted tool calls/results cannot produce a second action.

## Verification contract

The required matrix covers persistence before approval, decision before waiter
notification, Worker reserve before dispatch, dispatch before completion, atomic
side effect/receipt before terminal, Worker terminal before Runtime result,
Runtime result before Core continuation, and continuation before recovery HTTP
response. Also cover concurrent recovery, stale owner, cancellation, revocation,
approval expiry/parameter mismatch and unknown writes. Each receipt separates
isolated automated tests, controlled crash injection and real Qwen/Worker/browser
observations. Rust commands never run on the host; candidate compilation and
scoped verification use the existing Docker builders, with hosted CI evidence
reported separately when available.

Rollback is old-binary-compatible for quiescent history; active newly durable
runs require the recovery-aware Runtime to finish or be safely cancelled before
rolling back. Existing additive tables and immutable receipts are retained.


## Final projection repairs

A recovered Core started event has the same turn identity; history uses one
original start anchor and only a different turn ends its message interval.
This preserves partial body and avoids duplicating fallback and committed Core
body. Different turns with identical prose remain distinct.

Unknown results are immutable tool lifecycle facts. Runtime publishes the
existing safe side_effect_unknown event even when reusing a journal response.
Historical projection also reads original scoped lifecycle-only unknown
receipts, so older records retain their body, diagnostic run ID and uncertainty.
Cold cancellation appends its terminal receipt in the same scoped transaction;
duplicate stop repairs a missing receipt without advancing or replaying a run.

## 2026-09-27：Gateway 当前来源权益校验

Rust保留原run/turn/模型/lease与工具回执，Gateway在新轮、显式恢复与每个恢复模型请求前核验当前账号/来源权益。原线程含有前文知识时，本轮关闭知识库不移除Core上下文；累积来源从原owner的持久snapshot解析，权限复用KB可见目录。撤权或权限无法核验时拒绝推进，不重建run，不自动执行未知动作。

历史、SSE、待审批预览、Quiz与认证下载使用同一来源规则。只读thread响应可携带restricted_source_run_ids，客户端轮询只遮蔽内容并保留公开终态；不是执行所有权或恢复引擎。匿名Quiz必须有可核验且无私有知识的原run来源，不能从dataset_ids为空推断可公开。证据见R1-RP01收据与2026-09-27收尾报告。

## 2026-09-27 terminal-turn continuation correction

A persisted thread tool ceiling is immutable. Reusing it during resume/cold startup is a read: check persistence health, member/root ownership and tombstones, and reject any requested expansion. It does not require the last terminal run's expired execution-owner fence. First ceiling binding still uses the original locked write transaction. Execution claims, sticky persistence errors, invocation admission and receipt-write fences are preserved; no expired lease is extended and no unknown action is replayed.

The corrected path and original-thread followup acceptance are recorded in [R1-RP02](../../reports/product/r1-artifacts-long-task-acceptance-2026-09-27.md). This does not enlarge the supported recovery boundary or replace the accepted DR-01 crash matrix.


## Genuine SDK capability authority (staged source design, 2026-10-12)

OpenAI Agents SDK and Claude Agent SDK runs can use the private Runtime capability
bridge without creating Codex threads or turns. The Gateway owns genuine SDK run
admission, lifecycle, selected catalog, current entitlement/grant checks and the
owner heartbeat. `assistant_sdk_runs` pins the original scoped run, engine,
immutable snapshot/revision/hash, owner/fence, lease and expiry. Runtime's SDK
handler never starts a model or waits on a Codex server request.

`POST /internal/v1/sdk-runs/{run_id}/capabilities` uses internal service authentication,
tenant/user/session headers, `x-execution-owner-id` and `x-execution-fence`. Its JSON
body contains exactly `tool_call_id`, `tool`, optional `namespace`, and object
`arguments`. Scope, selected descriptors, approval IDs and execution IDs are never
accepted from this body. Calls are bounded to 128 distinct IDs per run, 160 ASCII
identifier characters for call/tool, 64 for namespace and 64 KiB compact arguments.
Polling the same call does not consume another invocation slot.

The handler takes a short per-run transaction lock, validates current SDK authority,
checks the immutable snapshot hash and readonly descriptor payload, and resolves the
selected descriptor using the existing resolver. The durable SDK invocation binds
name, namespace and canonical arguments before an approval or execution is reused.
These identity checks apply to reads as well as writes. Read descriptors requesting
approval are rejected because the existing Worker read-approval shape cannot safely
represent them.

Writes/unknown effects persist or reuse an ordinary `assistant_tool_approvals` row
through a transport-independent persistence helper. The Codex approval wrapper keeps
its existing process-local notification mechanism; SDK calls immediately return
`status=approval_required`, original approval ID/deadline, run/call/tool and arguments.
Only the Gateway's authenticated owner decision route can approve that row. Retrying
the original SDK call after approval observes the durable decision. Original expiry
and rejected/cancelled decisions never reset or create a replacement approval.

Both Codex and SDK wrappers use the same typed capability execution core: signed
Capability Contract V2 lease, CapabilityWorkerClient, pinned descriptor and revision,
original run/call/attempt identity, event polling, bounded cancellation and unknown
write classification. The SDK wrapper never fabricates DynamicToolCallParams or a
PlatformThreadIdentity. Worker authorization has a separate, explicit SDK snapshot
query; the existing agent_runtime/model-lease predicates remain unchanged. SDK
reservation, dispatch and recovery admission require the matching live authority.
Existing artifact, Office, knowledge and connector brokers are unchanged.

Before sending a new Worker create, Runtime durably marks `dispatch_started_at`.
A matching Worker ledger receipt is reused by exact scope/run/call/arguments and
capability identity; no new create is sent when an execution already exists. If an
SDK write was prepared but no Worker receipt can be established after a disconnect
or crash, the original call becomes `side_effect_unknown` and cannot be replayed.
This is deliberately conservative: even a crash immediately before network send can
leave an unknown outcome. Concurrent duplicates in that interval also fail closed.
The unknown barrier is evaluated before approval-expiry/rejection handling, so later
expiry cannot turn a potentially executed action into an ordinary denial.

Terminal responses include `status`, original run/call/tool, nullable `execution_id`,
raw Worker `receipt`, and the existing projected `response` (success/contentItems).
The complete response is written once and reused. A late receipt can be retained in
the invocation/Worker ledger without silently changing an earlier unknown response.
Workers may reconcile a dispatched write from its durable receipt; they never blindly
repeat a dispatched write. SDK reads and undispatched writes may be recovered only
while the original scoped SDK snapshot and owner lease remain live.

Verification is source-only until the paired Gateway/schema changes are applied and
hosted Rust checks plus isolated database/HTTP tests run. Required acceptance covers
read and write execution for both SDK engines, original approval decision/rejection,
wrong scope, stale owner, expiry/revocation, duplicate IDs, changed read arguments,
prepare-before-send crash, existing receipt recovery and unchanged Codex behavior.
No live-provider or deployment claim follows from source checks.


## Canonical recovery terminal status

The product run ledger and `assistant-turn-contract/v1` use `succeeded`, `failed`,
and `cancelled`. Core's `TurnComplete` is an execution event, not the product
status string `completed`. Recovery projects a matching Core completion without
an error to `run_finished` / `succeeded`; a matching completion carrying its
terminal error becomes `run_error` / `failed`. `TurnAborted` retains
`run_error` / `cancelled`. The same canonical status is passed to the fenced
ledger update and terminal event builder. Provider error text is not copied into
this recovery compatibility event. Ordinary V1 projection is the status authority;
recovery's unit regression compares all three outcomes against it.

Historical recovery rows/events may contain `completed`. Both native and Python
Gateway capacity watchers must recognize such a row as terminal for capacity-watch termination. It must not
restart or interrupt that run just because the legacy spelling is unfamiliar.
However, `completed` is not a general success alias: older recovery also used it
for Core `TurnComplete` records carrying terminal errors. The existing Rust and
Python completion consumers therefore remain fail-closed for noncanonical event
statuses, and their `status='running'` write guard preserves already-terminal rows.
The legacy `cancelled` event kind remains a cancellation even when its status
field is missing or different, while `run_error` / `cancelled` remains the normal
recovery cancellation envelope. No historical row, immutable event, model-call
`completed` value, Worker status,
or subagent `completed` status is relabelled by this repair. A historical data
repair needs separate operator authorization and reviewed persisted Core terminal
and error evidence, matched to the original run/turn/thread plus tenant/user/session
identity. It cannot infer success from the legacy spelling alone. Both Gateway
terminal recognition queries retain its existing engine and tenant/user/session
scope, and Python preserves its unconfirmed-dispatch guard; no broad unscoped reader or historical repair SQL is added. The shared bounded consumer matrix is
`tests/fixtures/agent-runtime/terminal-status-v1.json`.

### Atomic recovery terminal transaction

The canonical spelling repair alone left a split commit: recovery updated
`assistant_runs`, then appended its V1 terminal event in another transaction.
The expired-authority closer similarly committed its run update before trying
lifecycle admission or appending a receipt. A crash or event error could strand
a terminal row that the active-run scanner could no longer repair.

`PostgresThreadStore::commit_recovery_terminal` now owns this bounded recovery
transition. It locks the original managed run, visible scoped root/member, and
exact observed execution owner in run → root → owner order. It checks original
run/thread/turn, tenant/user/session, owner/fence/context and canonical event
identity. Ordinary recovery requires current execution authority and rechecks the
wall-clock owner fence after the append; the update must affect exactly one row.
Cancellation or takeover that wins those locks prevents a recovery terminal.

Lifecycle terminal admission, any expired-authority approval cancellation, the
run's terminal fields, and the durable V1 envelope share one PostgreSQL
transaction. The shared lifecycle classifier still records an interrupted
dispatched write/unknown action as `side_effect_unknown`. It neither retries a
Worker action nor converts uncertainty into proof that no side effect occurred.
All immutable appends reuse `append_assistant_runtime_item`; no schema, grant,
security-definer function or migration changes are needed. Root-monotonic cursor
allocation rolls back with the terminal if any write fails.

The HTTP layer broadcasts only the committed saved envelope/cursor, including
its original timestamp on a duplicate reconciliation. The same owner/fence can
re-read its identical committed terminal after its lease expires. An absent
receipt or conflicting terminal on an already-terminal historical row is not
invented or relabelled. Such rows still require separately authorized,
evidence-backed reconciliation against their original persisted Core outcome.

Expired-authority discovery is read-only and paginates fairly by
`(created_at, run_id)`, advancing even past unclosable rows. Its storage operation
rechecks the exact observed owner/fence/context, elapsed owner lease, original
model/snapshot expiry and revocation under the run/root/owner locks. A heartbeat,
takeover or cancellation that won the race makes the candidate a no-op. This
closure uses existing no-reactivation authority semantics; it cannot renew or
recreate authority. A tombstoned/missing root, scope failure, or lifecycle
admission error leaves the ledger active rather than committing a terminal
without a receipt.

Verification remains layered. Pure envelope tests cover canonical status,
original run/thread identity, event kind and the restricted expired-authority
outcome. Required isolated PostgreSQL acceptance calls the exact production
transaction/helpers as the actual Runtime role against the canonical epoch17
fixture. Its matrix covers event failure after ledger update; rollback of the
ledger, lifecycle receipts, approvals and cursor; immutable duplicate replay;
wrong scope; takeover while the old writer waits; expiry during a root-lock
wait; revocation; expired owner with still-valid authority; and cancellation.
An extracted-store harness is storage evidence only: it does not establish full
Runtime linking, HTTP/broadcast delivery, process-crash restart, live Worker or
provider behavior, or multi-instance acceptance. The active program receipt
must record whether each gate actually ran. Existing ordinary completion,
Gateway accounting/projection writers and historical data repair remain outside
this bounded recovery slice.


## Native Gateway event transport budgets

Managed Runtime observation uses a separate GET-only `EventTransport` in the
native Gateway. Ordinary thread/turn/approval commands retain their 10-second
connect and 300-second whole-request budget. An event connection instead has a
10-second connect budget, a 30-second complete-response-header deadline, and a
45-second idle-read budget. It has no whole-response timer: the Runtime's existing
15-second SSE keep-alive comments refresh the read budget even while a turn waits
for approval or produces no visible event. The header deadline ends at receipt of
headers; it must never wrap body consumption. A stalled or broken connection
still fails with the existing event-stream transport error.

Transport liveness is not execution authority. This change does not extend or
renew the original signed model lease, alter owner/fence or tenant/user/session
checks, change provider/capability limits, or change either official SDK's deadline.
The existing capacity watcher retains its bounded execution window, lease-loss
signal and read-only cursor reconnect; the existing interrupt request retains its
five-second budget. HTTP observation can outlive an execution lease without
allowing another model/tool call. A disconnect, idle timeout or aborted local
receiver does not itself prove remote cancellation, success or absence of effects.

Only the existing event GET path uses this client. The watcher reconnects using
its last monotonic durable sequence, and public observers retain their existing
cursor-based recovery contract. No command POST, approval decision, provider call
or capability mutation is replayed or moved onto the event transport. The existing
bounded request-ID/active-carrier capture, independently admitted scope headers,
source-access projection, terminal processing and drop/lease cleanup are preserved.

Verification is layered: exact source/preimage checks distinguish this GET-only
routing change from unchanged signed admission, payloads, mutation paths, watcher
and cancellation code. Small real-reqwest loopback tests cover periodic heartbeat
reads beyond the scaled header/idle durations, header stalls, body stalls,
header/cursor forwarding and disabled redirects. These are transport evidence only
when actually run; full native Gateway route authentication, actual Runtime
heartbeat/reconnect, owner/lease expiry and paired Runtime/Worker/provider
acceptance remain separate release gates. No production-acceptance claim follows
from a staged patch or a source check.

### Gateway terminal usage provenance

Runtime-terminal rows no longer depend on a second execution-status transition
for the Gateway to attach model-call accounting evidence. Both Gateways run the
same scoped metadata-only projection after their existing completion handling.
The projection requires a canonical terminal run, its exact original snapshot,
and a matching durable root terminal receipt including tenant/user/session,
run/thread and event/status identity. It never changes terminal status, error,
finished time, or existing numeric usage/pricing fields. A compare-and-set on the
prior usage document prevents an older concurrent aggregate from overwriting a
newer projection; an identical replay is a no-op.

The current model-call schema does not distinguish provider-reported tokens from
fallback token estimates. Both chat-completion adapters persist either into the
same completed-call fields. Therefore the repair writes recorded totals only
under `usage.metadata.runtime_model_usage`, with `provider_usage_verified=false`
and `settlement=not_established`. These are persisted observations, not a new
charge or proof of actual provider usage. Pending/unknown calls and incomplete
records remain explicit; no completed record yields null recorded totals rather
than fabricated zero usage. Existing canonical status and accounting values are
preserved. Historical ambiguous `completed` rows and rows without a scoped
durable terminal event are not reclassified or settled.

The shared SQL fixture is `tests/fixtures/agent-runtime/terminal-usage-projection.sql`.
Language-level tests check identical SQL and preserved completion behavior;
the JSON operations, scope filters, idempotency and compare-and-set require
isolated PostgreSQL execution as the actual Gateway role. A future
provider-actual settlement change must first persist measurement provenance at
the model-plane producer; it cannot infer that provenance from `completed` or
from a provider request ID alone.


## Atomic managed dynamic approval decisions (2026-10-13)

The Runtime approval broker records a managed dynamic decision and its immutable
`approval_result` envelope/cursor in one PostgreSQL transaction. Only after a
successful commit may it notify the local waiter. A receipt failure rolls back
the decision; losing the process after commit leaves both facts available to
the existing durable poller and restart recovery. This does not replace the
Core loop, change the Python governance boundary, or dispatch/replay an action.

The transaction locks the scoped original run, visible root, then approval.
That ordering agrees with run-first claim, cancellation, revocation and Worker
dispatch. Approval records user intent and does not require the HTTP-serving
process to hold the execution claim; Worker independently enforces its live
owner/fence and exact call/tool/arguments at dispatch. Approve requires the
original live model/snapshot authority. Reject does not renew it. New decisions
require an active original run and pending approval, with the original deadline
checked using wall-clock time after blocking locks; nullable deadlines keep
the previous decision contract. No expiry is extended.

Cold resolution requires the managed execution context and original dynamic
`approval_required` receipt, bound to run/root/session/call and canonical
arguments with write/unknown effect. A lost native command/file ServerRequest
is not fabricated as a dynamic destination. Historical managed approvals without
the matching required receipt remain unsupported/unknown to this decision path:
they return the existing approval-not-eligible error envelope, are not granted,
and are not re-created. Normal recovery may read only its original durable
invocation and produce its already-authorized required receipt; this endpoint
does not perform that reconstruction. The legacy live ServerRequest
resolution path remains separate and is not covered by this atomic guarantee.

An exact internal Runtime retry returns the original stored envelope and cursor
without notification, mutation or new authorization, even after later consumption
or run completion. Opposite decisions, changed reasons (including null versus
empty), changed identity, missing receipts and pre-atomic random receipt IDs
remain conflicts. This is a replay of the decision fact, not a report that the
run can still execute.

Native Gateway V1/V2 approval POSTs retain current actor/session/thread checks,
source authorization and V2 pinned-snapshot admission. An approved retry whose
Runtime summary is already approved/rejected/consumed/cancelled/expired uses the
internal receipt-only operation, bound to the route's original thread. That
operation validates the same durable required/result receipts and exact decision
and reason, including null versus empty. V1 validates that a Runtime reason is
missing, null or a string before readback/decision; malformed JSON types cannot
alias null, and previously accepted long strings remain valid. Readback cannot
write a new decision, notify a waiter, or rebroadcast the result. Missing/conflicting receipts and mismatched
threads fail closed. Native ServerRequest approvals cannot use this path. Pending
or unknown summaries still require the original owner preview; a failed preview,
source check, or transport read is never permission to approve or to fall back to
mutation. Current authorization failure may still prevent public readback even
when Runtime retains the decision. Old Runtime versions reject the new optional
internal receipt-only field; normal decision request bodies are unchanged.

Receipt-only verification update (2026-10-13): pinned offline Runtime product
compilation and all 15 selected Runtime library tests passed against the composed
source. The rebased native Gateway product build and all three selected Gateway
library tests also passed, including malformed-reason rejection and nonpending
readback selection. Four source-contract checks and the bounded runner helper
checks passed. The actual product/test ELF and compiler-input bindings were
recorded externally; products were not launched. These paired gates do not claim
all-targets coverage or SQL, lost-response HTTP, race, restart, provider, image or
deployment acceptance. Those behavioral acceptance gates remain unverified.

Verification update (2026-10-13): the integrated six-file source manifest is
`8129970260432c455a45c9c54454e5c2d7f1131f48c06a0999bf89dbe7457c39`.
Pinned Rust 1.95.0 `cargo check --locked --offline --verbose -p
ai-platform-agent-runtime --all-targets` passed against the actual composed
Runtime production and test targets. The initial source check took 609.887 seconds;
after formatting only the four changed Rust files with `skip_children=true`, the
same all-targets check passed again in 10.052 seconds. Pinned rustfmt check passed;
the repository's stable-channel `imports_granularity` warning was retained.
Independent review verified the formatting transformations changed no functional
semantics. The final check receipt SHA-256 is
`0bdf50eebedc32ee5b3affd1f4c64096ddb9bde7558245a87823b556a16cd330`.

Both source checks used one Cargo job, no debug information or incremental
compilation, external artifacts, and the approved memory and dual-filesystem
guards. Source/toolchain/lock and downloaded dependency bytes were bound before
and after. The preceding exact locked fetch verified 1,283 registry archive
checksums and all eight locked Git revisions; generated code-mode protocol
output was separately bound. The original uncompiled recovery packet and the
first check remain historical evidence rather than being overwritten.

A later separately admitted, independently reviewed test-target build linked the
actual Runtime library test executable using the pinned stable deferred Core
backend. All nine focused approval-decision tests executed and passed (zero failed
or ignored; 86 unrelated tests filtered out). The guarded build/test took
1,022.475 seconds with one Cargo job, debug/incremental disabled and unchanged
memory/dual-filesystem stops; there was no stop or remaining process group.
Receipt SHA-256:
`6bcff5400954a5190edbf823b9eb17066c7e531d4fc081e8de523c3ecfce4f44`.
The test executable SHA-256 is
`875bbb70e14c38f2c9769945306f6eca542d24e47ebe12ba75226be710832827`;
its dep-info, 39 source inputs, codegen output and exact nine-name stdout are
bound separately. Source/toolchain/lock and registry/Git dependency source bytes
remained unchanged. Sixteen pure build-wrapper routing tests also passed before
this attempt; those are distinct from the nine Rust approval tests.

A separately admitted Runtime main-bin compilation then passed in 477.53 seconds
using the same pinned source/toolchain and reviewed stable backend. The guarded
Cargo receipt is
`5732eac41dc1f9f2ad3f3dd6a33b57e6f286d8dfa69b7396bc6ac14b1bf856ad`;
the resulting 318,003,320-byte ELF has SHA-256
`e1fc8d5a2a75bb5e4e6716d4cec00cf6781b5880d5f156327eecb81a8dc210a4`.
The original post-build proof parser exited 1 because Cargo dep-info includes
build-script directory prerequisites. That failure is retained separately from
the successful compilation. A separately reviewed read-only verifier, without a
rebuild or product execution, bound the exact ELF target, 2,274 file prerequisites
and 23 source directories. Source file membership/content/modes and original test
ELF/dep-info were preserved; directory metadata is recorded for the final
snapshot, without claiming historical directory-mode comparison.

This is a host Runtime main executable, not a complete deployable package.
`deploy/agent-runtime-source/Dockerfile.runtime` also installs the container
entrypoint and source evidence. The canonical Worker image builds and installs
four targets: `ai-platform-capability-worker`,
`ai-platform-capability-worker-health`, `ai-platform-python-supervisor` and
`ai-platform-plugin-packs` (`Dockerfile.capability-worker`). The health binary is
used by Compose/image healthchecks, and Python capability execution calls the
supervisor. The Runtime-only build did not verify those Worker targets or image
assembly.

A later separately admitted four-target Worker build completed in 293.389 seconds
with the same pinned Rust 1.95.0 source/lock, one Cargo job, debug/incremental
disabled and unchanged resource guards. Its receipt SHA-256 is
`9b700c104272a2c6722d0ce957ca8551e8e21d4735559171ce3b9a723abe9951`;
the four-artifact binding SHA-256 is
`b050f6add1e28a262a84988bc687802a768b6c5811bc141cd984c434b48bd043`.
Worker main, health, Python supervisor and plugin-packs each produced an actual
ELF64 x86_64 executable, bound to its exact main source and dep-info; all four
were compiled in this attempt. Source and registry/Git dependency bytes, plus
the preceding Runtime/test ELFs and dep-info, remained unchanged. The guarded
attempt exited 0 without a stop or remaining process group; none of the four
products was executed. Ten pure routing and eight inert-file binding tests
passed before this build. This completes the selected host-binary compilation
scope, not OCI image/package assembly, process acceptance or deployment.

Eight existing root Worker `MemoryStore` tests later ran by exact full-name
filters and passed: zero failed/ignored, 73 unrelated tests filtered out. The
11.554-second guarded build/test bound the actual executed library-test ELF and
30 dep-info inputs; receipt SHA-256 is
`7cde18192f891e1a38ab4ada5b5b49f7ddfc88da58926ed0bc87d5fc162eb253`.
They cover reserve/scope idempotency, Runtime binding, event cursor progression,
active/expired lease fences and side-effect-unknown recovery in the in-memory
reference store. No PostgreSQL, socket, broker or product process is invoked by
these selected test bodies. All six preceding product/test ELF and dep-info
pairs, source and registry/Git dependency bytes remained unchanged.

This narrow cached test used a separately reviewed 8 GiB launch reserve with
exact cache identity and unchanged memory/disk stop guards. Earlier 12 GiB
product-build admission contracts were not changed. Sixteen pure wrapper and
eight inert-file binder preparation tests passed first. This is in-memory
reference-model verification; it does not establish production SQL atomicity,
HTTP authorization, process restart or external side-effect reconciliation.

The nine Runtime approval tests cover pure receipt identity, argument binding, replay comparison
and public envelope helpers. They do not exercise PostgreSQL transactions or a
live process restart. Current Runtime plus four-target Worker host compilation does not establish
OCI image or deployment completion. PostgreSQL transaction/concurrency, restart, provider,
Worker and browser acceptance remain unverified.
Required transaction checks remain append-failure rollback, commit-before-wakeup
crash, exact retry after consumed/cancelled/terminal, opposite decision/reason,
expired deadlines under lock contention, cancellation/revocation races, missing
or native required receipt, and cross-scope denial. Historical Sep26 evidence
remains limited to the identities in the linked DR-01 report.
