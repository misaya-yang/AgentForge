# Architecture and Boundaries

> Current source contract for coding agents. Distinguish the retained deployment,
> the enterprise candidate and target invariants. Source existence, compilation,
> isolated SQL evidence and live release acceptance are different facts.

**Schema:** `harness/architecture/v3`
**Last verified:** 2026-10-17 (source review of `22d9ce0` plus U-PE-01).
This describes the native source and configuration; current image and live acceptance remain
open in the execution ledger. The Rust-native successor decision is proposed in
[P-ADR-1](../architecture/proposals/rust-native-platform-baseline.md).

The active implementation/evidence authority is
[`native-enterprise-upgrade/loop-state.json`](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json).
Completed and superseded programs retain their historical receipts; do not reopen
or discard them. [`platform-architecture.md`](platform-architecture.md) remains the
product-layer contract, subject to the explicit bounded SDK-service amendment below.

## 1. Logical ownership and implementation profiles

A process role, Rust crate or migration batch is not automatically a new bounded
context or independently deployable service.

| Context / role | Current source | Owner and boundary |
| --- | --- | --- |
| Gateway Control | `rust/platform-native/crates/platform-gateway`, `platform-gateway-foundation`, `platform-gateway-integrations`, `platform-auth` | Public APIs, identity/current tenant policy, service/provider/model admission, quotas/accounting, immutable launch, private model plane and Gateway-owned brokers |
| Managed Agent Execution | `rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime` with pinned Codex Core/App Server | Managed Thread/Turn execution, capability orchestration, owner/fence recovery and ordered durable events; not provider credentials or billing policy |
| Capability execution | `ai-platform-capability-worker` and `ai-platform-capability-contract` in the overlay | Signed scoped tool execution, single-use approval receipt, dispatch fence, durable result and unknown-side-effect barrier |
| Explicit official SDK execution | Optional `platform-sdk-agent-worker` with `ai-platform-sdk-agent-engine`, admitted by Gateway | Native SDK-protocol execution in a distinct SDK session/run; no implicit core-kernel substitution or cross-engine failover |
| Knowledge API / Worker | `platform-knowledge-service` binary and `platform-knowledge-domain` | Dataset/document lifecycle, resource ACL, generation/publication, ingestion and retrieval; separate API/worker/all process roles |
| Hosted surfaces | `web/`, published client SDKs, native `platform-client` and `platform-desktop` / platformctl | Gateway clients; render server facts, submit explicit commands, reconnect reads without re-submitting execution |
| Independent local Agent CLI | `sdk/cli` launcher and lock-pinned local Rust CLI/App Server | Local provider/profile/home and execution, under ADR-009; only explicit `gateway` subcommand uses the remote compatibility client |
| Database migration authority | `platform-migration-authority` and immutable `database/` artifacts | The only schema-change authority; applications receive scoped roles and enforce read-only compatibility preconditions |

The default Dockerfile/build overlay compiles Rust binaries; the base Compose consumes
versioned images and never builds source. Runtime/Worker use the separate pinned source
workflow and one release lock. Registry tags alone do not prove a native executable identity.
The optional SDK overlay selects an additional native worker, not a replacement managed kernel.
First-party Python services are retired; only the optional Python user-code sandbox remains.

Gateway and managed Runtime remain single-instance release assumptions until
specific multi-instance ownership/recovery/affinity gates pass. Capability and
Knowledge worker replication need their own real claim/cancel/recovery evidence.
Official SDK Worker currently owns one private state volume and filesystem session
locks; Gateway provenance grants are process-local. It is not a replicated or
cross-Gateway restart-resumable session store.

## 2. Allowed runtime edges

| Caller | Callee | Purpose and forbidden expansion |
| --- | --- | --- |
| Hosted Web / client SDK / platformctl / desktop | Gateway public API | Product contract; no direct internal Runtime/Worker/Knowledge ingress |
| Gateway | Managed Runtime | Authorized thread/turn lifecycle and event observation; no second managed Agent loop |
| Managed Runtime | Gateway private model plane | One scoped provider operation; Runtime does not hold provider/billing policy |
| Runtime | Capability Worker | Exact descriptor/arguments/lease/approval execution; Worker does not choose next model action |
| Worker | Knowledge | Resource-bound retrieval/actions after proof and current actor/ACL checks; no direct Knowledge-table ownership |
| Worker | Gateway broker | Gateway-owned connector, MCP, image, artifact and local-node operation; no public API authorization bypass |
| Gateway | Knowledge | Signed public management proxy and explicit resource authorization; no implementation import or direct write to Knowledge-owned state |
| Native Gateway | Official SDK Worker | Explicit framework-selected private launch, per-run key and bounded authority; no user bearer token or signing secret |
| Official SDK Worker | Gateway scoped bridge | Revalidated selected Knowledge reads or governed capability calls; never caller-selected run/owner/snapshot/effect |
| Gateway SDK bridge | Runtime capability-only endpoint | Existing approved capability transport and receipt semantics; no invented Codex Thread/Turn and no nested model loop |

The independent Agent CLI is a separate local product deployment under ADR-009.
Its default launcher starts the composed local Rust CLI/App Server and directly
uses its local provider profile; it does not expose or call hosted `/internal/v1`.
Its loopback provider compatibility adapter is not a hosted microservice.

Keep heterogeneous auth contracts when their trust domains differ. Knowledge uses
signed method/path/body/identity envelopes with replay protection; capability calls
use exact signed leases/proofs, owner fences and approval receipts; SDK private
transport uses operator service authentication and run-bound bridge grants. Do not
replace them with an unscoped universal bearer simply to reduce code.

## 3. Code dependency contract

Native Cargo dependencies and source imports are checked by `make architecture-boundary-gate`
and `make core-boundary-gate` (the same native checker). The gate resolves renamed/path/workspace
and target dependencies, rejects cross-context implementation edges, and records one explicit
transitional Knowledge-to-Gateway exception. The partial upstream overlay requires an
identity-verified composed workspace. Passing the graph gate does not prove runtime isolation.
`src/` and `packages/` retain static catalogs/evaluation resources, not Python services.

Native `platform-client` is a transport/client boundary shared by desktop and
platformctl. It must not acquire Gateway stores, policy or Agent execution logic.
Native Gateway foundation/integrations currently contain substantial domain
implementation; they are compilation boundaries, not pure cross-service contract
packages. Knowledge lives in that crate graph but constructs its own process,
connections and signed actor client. Avoid further arbitrary domain growth in shared
modules. Extract a domain only when ownership or measured build/change cost justifies
it, preserving existing ports and wire contracts.

Cross-language protocols require one named producer authority and shared versioned
schema/fixtures. Do not independently invent status vocabularies, field meanings,
canonical JSON/hash formats or retry semantics across Rust and TypeScript consumers.
A pure shared contract must not acquire environment, filesystem, DB, HTTP or service
configuration dependencies. ADR-011 records the existing bounded admissions.

## 4. Data, execution and cancellation authority

Every persistent object must have a migration owner, permitted mutations/readers
and tenant-isolation evidence. Some assistant tables intentionally span admission,
execution and accounting: specify engine/field/function ownership, not only schema
names. Scoped roles and canonical grants exist. The local runtime profile selects scoped DSNs,
while the public base Compose still uses bootstrap-superuser defaults (U-DATA-02 remains open).
A role never substitutes for tenant predicates, object authorization or owner fences.

File and tenant-policy administration must distinguish tenant `admin` / `file_admin`
from explicit platform administrator roles. Ordinary administrators may only act
on their authenticated, nonempty tenant; caller target fields cannot expand scope.
Policy lists and tool audit queries are tenant-filtered before database reads.
Configuration audit uses recorded actor-tenant filtering, not resource-ownership
ACL semantics. Global file statistics and legacy cleanup require an explicit
platform administrator because those backends do not support tenant-scoped work.
Targeted upload deletion remains available to same-tenant administrators.


- Gateway owns identity, policy, immutable admission snapshots/model leases and
  accounting. Runtime is the target authority for managed execution terminal facts.
- Existing managed run completion is still split: Runtime emits ordinary terminal
  events, Gateways project them into assistant_runs, while recovery/cancel can write
  terminal state in Runtime. Treat this as named convergence debt, not permission
  for new independent execution writers.
- SDK runs have separate engine-scoped parent/owner/tool-call records. Gateway's SDK
  authority, official worker receipt and Runtime capability-only broker do not create
  a second managed Thread/Turn identity.
- Worker owns exact capability dispatch/result. After dispatch uncertainty, retain
  `side_effect_unknown` and original execution identity; never auto-replay a write.
- Knowledge owns dataset/document generations, ingestion/retrieval and dataset object
  prefixes/vector collections. Gateway Agent memory is a distinct namespace/owner.
- Cancellation acceptance, local transport abort, process termination, external-effect
  cessation and final accounting are separate observations. Never infer one from another.
- Runtime event-subscription HTTP rejection is an observation failure, not execution
  terminal evidence; it must not mark the run failed or revoke its model lease by itself.
  Where run-capacity admission is configured, its watcher owns bounded reconnection and
  durable cancellation/fencing acknowledgement and quarantine. Neither observation
  failure nor cancellation acknowledgement alone proves external-effect cessation.
- Generic `/api/v1/tasks`, scheduled Agent tasks, SDK runs and managed turns are different
  contracts. The default native memory task profile uses destructive in-process dequeue
  and does not survive process loss. The explicit durable-delivery mode uses atomic
  task/envelope publication and a claimed-unknown barrier. Its source guard requires the
  appropriate schema successor; isolated evidence does not prove deployed restart recovery.
  Do not transfer guarantees from scheduled Agent, SDK or managed execution ledgers to
  either generic task profile.

The native raw transparent proxy binds a private capacity owner once and fences
its admitted request lifetime and transferred streaming pump on lease loss.
The fence covers the service-slot queue, cache lookup/write, upstream send/retry
and response reads, stream accounting, and blocked downstream channel delivery.
Loss before response handoff returns the existing capacity-rejection shape with
`GATEWAY_CAPACITY_LEASE_LOST`; after SSE starts, the local pump closes without a
synthetic completion frame. Bytes already buffered for delivery may still drain.
An already-observed loss wins over work not yet polled; a response already
returned is not revoked. RAII drops local transport/slot/connection owners and
starts the existing asynchronous capacity cleanup. This is cooperative local
cancellation, not provider execution-cessation evidence or an atomic distributed
fence. Accounting may be interrupted, while recording work already independently
owned by UsageRecorder may continue. Neither cancellation nor resource release
establishes durable final billing or exactly-once charging.

Canonical migrations remain immutable. Gateway and Knowledge perform application-role,
read-only schema admission before providers, workers and readiness. For the current epoch-22
source, windows are Gateway 18–22, durable Gateway 20–22, Knowledge API 19–22 and Knowledge
Worker 21–22. Admission checks the frozen baseline marker, exact migration-prefix checksums
and unresolved attempts; full structural/ACL/extension/reference fingerprint recomputation
belongs to `database-authority verify`.

Runtime/Capability Worker are not covered by that application guard yet. Their authority CLI
startup branches retain a legacy 2–10 window and cannot certify epoch 22. U-GW-20 owns this gap,
including real wrong-baseline/missing-ledger/out-of-window rejection and process startup evidence.
The source epoch and source windows do not prove the schema of a running database.

## 5. Managed and optional SDK execution contract

The Codex-based Rust Runtime is the sole managed Assistant/Studio kernel. The deleted
first-party Python AgentLoop/Assistant executor must not return. Gateway may resolve
an immutable launch; clients may not infer or advance server execution state.

[ADR-012](../architecture/ADR-012-optional-official-sdk-execution.md) is a proposed,
bounded amendment for explicitly registered official SDK services. Each run chooses
one execution owner before dispatch. Its adoption gates remain open until proven;
source integration is not production acceptance. SDK sessions must not be projected
as Codex sessions, silently migrated between frameworks or used as a fallback kernel.

Public coexistence preserves managed v2 events and legacy v1 projection, SDK
StreamChunk lifecycle, Capability Contract V2 and signed Knowledge v2. Share exact
fixtures and semantic definitions where they overlap, not an invented universal
state machine. `completed` has valid meanings in SDK/provider/tool/task domains;
managed success is `succeeded`. Historical managed recovery rows using `completed`
need bounded reconciliation, not a global success alias.

## 6. Contracts and required evidence

| Contract | Authority / required proof |
| --- | --- |
| Public API/OpenAPI | Owning routes + `sdk/openapi.json`; native source/profile-aware contract comparison, live checks reported separately |
| Managed events | Runtime canonical producer and shared fixtures; Rust/Web/CLI/SDK projection, reconnect and terminal/cancel races |
| Capability execution | Rust Contract V2, immutable lease/approval/result; exact cross-consumer hashes and real Runtime↔Worker fault matrix |
| Official SDK private protocol | Native adapter + worker contract fixtures/locked packages; pure/KB/governed profiles, actual libraries, provider/cancel/restart/retention separately |
| Knowledge identity | Signed envelope/current actor/ACL, including replay, revoked roles and cross-tenant negative cases |
| Database | Canonical baseline/epoch, immutable history, measured fingerprints and actual application-role checks; startup window enforcement separately |
| Health | `/health/live` process liveness; `/health/ready` compatible admission dependencies; optional capability degradation explicitly classified |
| Trace and time budgets | Sanitized request/trace/run/call identity per hop; connect/header/idle/execution/cleanup timeouts and safe-retry category |
| Release profile | Actual executable/image/protocol identities of included services; draft manifest and local build receipt are not deployed identity |

A rollback requires a real compatible native code/schema/data bundle and a paired Runtime/Worker.
Deleted Python services are not an available rollback target. Preserve historical evidence
without treating old source, source locks or registry tags as an accepted running release.

## 7. Current prioritized debt

1. Managed recovery terminal semantics and crash atomicity: canonical success/failure
   mapping, then owner-fenced terminal fact+event transaction and Gateway convergence.
2. Complete per-process schema/profile admission for Runtime/Worker and prove all process
   startup refusals; retain the implemented Gateway/Knowledge guards and deployment readiness.
   Helm and Compose use `/health/ready`; the retained Helm chart still
   needs a reviewed managed-Runtime/native deployment profile (see `DEPLOY.md`).
3. Explicit release/topology identity for retained/candidate/optional implementations,
   including SDK Worker when selected. Current release manifest is draft.
4. Native Gateway→Runtime request/trace propagation and separate long-SSE transport
   budget; preserve GET cursor reconnect without POST retry.
5. Engine-aware client fixture coverage and generic async-task durability semantics.
6. Measured native domain extraction after correctness gates, not broad file splitting.

Preserve existing work and sequence one coherent implementation slice at a time.
No Runtime executable, provider, browser or production claim follows from a source
review or a successful unrelated build.

## 8. Historical decisions and guardrails

Accepted ADRs preserve history. A successor supersedes precise clauses rather than
silently rewriting prior decisions. Key records: ADR-006/007 (managed kernel/data),
ADR-008 (bounded contexts/deployment), ADR-009 (independent local CLI), ADR-010 (tenant/Knowledge authority), ADR-011
(pure contracts), and proposed ADR-012 (optional official SDK scope).

Read [`work-packages.md`](work-packages.md) and
[`repository-quality.md`](repository-quality.md) for source ownership and hygiene.
One product-scale change has one primary writer; parallel reviewers stage externally.
Line count is a review trigger, not architecture proof. Prefer the smallest change
that fixes an observed boundary defect; keep public contracts stable unless an
explicit versioned migration is the task.


## Native HTTP request governance (2026-10-15)

### Accepted Responses observation-failure contract

For non-streaming `/v1/responses`, once Runtime accepts a turn, an observation error
must not release the request for a second execution. With an `Idempotency-Key`, retain
the original 503/502 response and its `x-run-id`/`x-session-id` in the existing 24-hour
response cache. Same-key/body retries and waiters replay that result; changed bodies
conflict. Failures before acceptance retain their existing retry behavior. Streaming
Responses continues to reject idempotency keys.

The Ph0 source repair retains stream-open/read failures and EOF after `start_turn`
through one accepted-failure cache path. Its production-route, real-store and live gates
remain open in `UP-PH0-RESPONSES`; source changes alone do not establish acceptance.
If cache persistence is unavailable, retain the claim and report storage failure with
run/session identity. An uncertain cache write may later yield the original cached error
or in-progress; it cannot guarantee original-error replay. A cached observer failure is not an execution
terminal fact, transparent resumption or an exactly-once execution guarantee. Cache
expiry/loss, ambiguous start acknowledgement and cancellation need separate evidence.

### Native middleware ownership

Gateway and Knowledge share `request_governance` middleware, separate from domain,
tenant and Redis-backed admission. The typed `request_governance` configuration
sets `max_concurrent_requests` (default 1024), `response_header_timeout_ms`
(default 120000), and `response_body_idle_timeout_ms` (default 120000).
Explicit JSON/YAML null disables an individual deadline; zero and out-of-range
values are rejected. Admission is immediate: saturation returns HTTP 503 with
`Retry-After: 1`. Exact health/live/ready and metrics probes remain available.

A permit lives through response-body completion or disconnect. The header deadline
returns HTTP 504 only before a response exists. The body deadline bounds inactivity
between frames and terminates the body with a transport error after headers; it
never rewrites an accepted response status or imposes a total SSE lifetime.
Existing authentication, tenant quota, tracing, and graceful-drain authorities are
preserved. `RequestCancellation`, using the existing admission cancellation signal,
is available as a request extension for cooperative child work; dropping a handler
future/body signals cancellation and releases local admission. Unrelated detached
jobs and durable executions are not silently cancelled by a disconnected observer.
WebSocket upgrade transfers to its own connection lifecycle after the HTTP response.

Dashboard WebSockets have a separate per-API active-session semaphore because HTTP
admission ends at upgrade. Defaults are 256 sessions, a 10-second snapshot/send
deadline and a 60-second inbound-idle deadline; native construction can supply a
validated `DashboardWebSocketConfig`. Authorization still precedes upgrade and
session admission. Saturation returns HTTP 503 with Retry-After, while authentication
and metrics-permission denial remain HTTP 403 before upgrade. Periodic protocol
pings allow ordinary browser peers to stay live automatically. Text `ping` retains
the existing JSON `pong` response. Every socket-task exit releases its owned permit.
