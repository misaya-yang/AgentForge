# Cross-service architecture evolution, 2026-10-12

Status: evolving source implementation and bounded evidence, not an accepted release. Stages A and C recovery fixes, managed/SDK correlation, Helm probe correction, SDK client recovery and native SSE transport are integrated. Stage C passed exact extracted production-storage PostgreSQL cases and Runtime all-targets type checking; the full Runtime executable remains unlinked. Native SSE has seven actual Gateway unit/loopback tests passing. Other gates and staged candidates remain explicit in loop-state.json; no deployment acceptance is implied.
Repository: AI--Platfform, HEAD 385e9d8666f2447b10cfedbf6fec30ba5b7ab6ec plus the current uncommitted enterprise implementation. The initial review was read-only; subsequent root integration and verification are recorded separately in loop-state.json and reports/architecture/. External candidate patches are not shipped until integrated. No provider or production deployment acceptance has been established.

## Recommendation

Keep the hybrid architecture and its implemented work. Upgrade the contracts at service boundaries in a deliberate order: terminal-state correctness, startup/release compatibility, execution ownership, then trace/deadline and client consistency. Do not turn this into another whole-platform language migration, a new universal execution engine, or an immediate service split.

The highest-impact problem found is real: successful recovery uses `completed` where normal managed-Agent execution uses `succeeded`. A second real problem is the crash window between recovery's terminal database update and its durable terminal event. The proposal begins with a bounded producer/consumer repair and preserves the more substantial atomic-ownership change as a separate stage.

The architecture entrypoint also materially lags implementation. Its August claims that import gates do not exist and all processes share one unrestricted database login must not be used as current facts. Conversely, the existence of new source, SQL grants, native binaries and unit evidence does not establish a runnable paired release. The native-enterprise loop-state remains authoritative for evidence and explicitly says not accepted.

## 1. What exists now

| Boundary | Implementation and owner now | Important actual limit |
| --- | --- | --- |
| Public Gateway | Retained Python `src/`; candidate native Gateway in `rust/platform-native/crates/platform-gateway`, foundation and integrations. Owns account/tenant authority, provider credentials, admission, model plane, quotas/accounting, public APIs and brokers. | Default Dockerfile and Compose still execute Python. Native source/binaries are a different deployment candidate, not an automatic replacement selected by language. Never put both behind one writer role without an explicit migration profile. |
| Managed Agent Runtime | Rust overlay embeds pinned Codex Core/App Server. Owns managed Thread/Turn execution, append-only event order, capability invocation, owner/fence recovery and managed approvals. | Latest executable remains unlinked according to the parent checkpoint. Source/all-target evidence is not executable, image or distributed-recovery acceptance. |
| Capability Worker | Rust service, with bounded Python subprocess/tool support and native Office paths. Owns execution reservation, dispatch fence, durable result and unknown-effect barrier. | A write with unknown outcome cannot be replayed; "cancelled" does not prove the remote effect never happened. Multi-replica correctness must be demonstrated independently. |
| Optional official SDK Worker | Python process embeds actual OpenAI Agents SDK or Claude Agent SDK. Native Gateway selects exactly one framework before dispatch. Each SDK run has a real separate assistant_sdk_runs identity, Gateway owner lease and scoped bridge. Runtime brokers governed capability calls; Capability Worker executes them. | Session files/flock and provenance grants are single-owner/process-local. Restart safely requires reconciliation rather than automatic paid-loop continuation. Do not pretend an SDK run has a Codex thread/turn or fail over from one framework to another. |
| Knowledge API / Worker | Retained Python service plus candidate native binary with API/worker/all roles. Owns datasets, ACLs, document generations, ingestion/publication fences, retrieval and object/vector state. Gateway uses signed HTTP. | Native Knowledge currently resides in the Gateway crate graph but constructs its own connections and authenticates actors over HTTP; this is compilation coupling, not evidence of an in-process security bypass. PDF/embedding capabilities must remain explicitly declared and probed. |
| Hosted Web / platformctl / native desktop / published client SDKs | Gateway-only clients. Native desktop/platformctl share `platform-client`; other client SDKs and Web consume versioned public APIs/events. | Managed v2 event cursor/reconnect and SDK StreamChunk lifecycle are different supported contracts. Reconnecting a read stream must never re-submit an admission or approve again. |
| Independent Agent CLI | `sdk/cli/src/cli.tsx` defaults to the lock-pinned local Rust CLI/App Server via its Node launcher. Owns local provider profile and isolated home; optional loopback Responses↔Chat adapter. | It does not call hosted Runtime/Gateway in the default mode (ADR-009). Only explicit `gateway` compatibility subcommand uses the retained remote client. Do not fold local credentials/state into hosted tenant execution. |
| Durable platform jobs | Scheduled Agent tasks, Knowledge generation work, Eval outbox, SDK cleanup each have their own domain ledger. | Generic `/api/v1/tasks` still uses an in-process destructively consumed queue and status-only cancellation. It is not the scheduled-task or SDK-run durability contract. |

### Current call graph

Hosted Web / platformctl / desktop / client SDK → authenticated Gateway. Independent Agent CLI → local composed Rust App Server/CLI → locally selected provider; its explicit `gateway` subcommand alone uses the remote compatibility path.

Managed Assistant/Studio: Gateway admission and immutable launch → Rust Runtime → private Gateway model plane → provider; Runtime → Capability Worker → signed Knowledge retrieval or Gateway-owned broker → external tool/local node.

Explicit SDK service: native Gateway admission + durable SDK owner → private SDK Worker → official SDK/provider loop; SDK Worker → bounded Gateway read/capability bridge → Knowledge or Runtime capability-only endpoint → Capability Worker. No nested Codex loop and no user JWT/provider-signing secret sent to the execution worker.

Knowledge CRUD: Gateway signed proxy → Knowledge API → Knowledge-owned storage; Knowledge worker processes its own durable generation/claim records.

## 2. Preserve these successful boundaries

1. Separate core managed execution from explicitly selected official SDK services. ADR-012 is a narrow proposed exception, not authority to revive the deleted first-party Python loop.
2. Preserve signed immutable capability descriptors, exact argument hashes, one-time approvals, execution identity, dispatch fences and unknown-effect barriers. Shared UX does not make the two engines' storage identities interchangeable.
3. Preserve Knowledge's authoritative ACL and Gateway actor revalidation. Gateway role names and cached client metadata must not substitute for current tenant identity.
4. Preserve canonical immutable baseline/migration history, measured epoch17 evidence and process-specific grants. Never repair compatibility by broadening grants, editing frozen migrations or weakening the retained Python Gateway's epoch ceiling.
5. Preserve source identity receipts, package locks, synthetic-test evidence and recovery archives. A new stage adds evidence; it does not relabel prior evidence as live acceptance.
6. Preserve no-automatic-retry behavior on uncertain admissions, approvals, SDK loops and external side effects. Existing differences in replay semantics are often intentional.

## 3. Findings, ranked by consequence

### F1. Recovery success has a different semantic value from ordinary success — confirmed defect, P1

Evidence:
- Runtime `http_service/recovery.rs:51–52`: Core TurnComplete becomes `completed`, without inspecting its optional `error` in the current pinned Core protocol.
- Same file `:350–391`: terminal_projection writes that literal into assistant_runs and emits run_finished with it.
- Runtime `v1_projector.rs:127–142`: ordinary TurnCompleted emits `succeeded`.
- Native Gateway `migration_agent/control.rs:659–680` and Python `control/run_ledger.py:16–65`: anything other than `succeeded`/`cancelled` becomes failed; the database update only touches running rows.
- Native Gateway `control.rs:1357–1365` and Python `control/capacity.py:19,48–57`: terminal readers recognize succeeded, failed, cancelled, not completed.
- Web `features/chat/runtimeV2State.ts:331–337` infers success from run_finished; an old error-bearing recovery incorrectly emitted as run_finished can therefore be displayed as success. Producer correction prevents new instances; historical outcomes remain ambiguous until evidence-backed reconciliation.

Consequence: after a recovery success, the public/event/status contract is inconsistent. Gateway cannot repair the already-terminal row with its running-only update, and a capacity watcher may treat it as nonterminal. This is not a cosmetic enum rename.

First slice: map error-free recovered managed completion to `succeeded`, completion with an error to `failed`, and abort to `cancelled`; use the same fixture for Runtime/native/Python consumers. Existing managed `completed` rows may represent success or failure because the old producer discarded the error distinction. Recognize them as terminal to protect capacity watchers, but retain ambiguity pending reconciliation against original persisted Core evidence. Do not blanket-alias historical `completed` to success. Do not globally reinterpret `completed`: SDK, provider, tool, ingestion and generic-task states legitimately use it.

### F2. Recovery terminal state and terminal event are two commits — confirmed crash window, P1

Evidence:
- Runtime recovery terminal_projection updates assistant_runs at `recovery.rs:366–367`, then resolves identity, appends the event and broadcasts at `:368–396`.
- The recovery scanner claims only active running/awaiting_approval work; event persistence is a separate transaction in `postgres_store/events.rs:335–408`.
- Ordinary completion principally appends the event; Gateway consumes it and updates the run ledger. Recovery/cancel can update the run ledger inside Runtime instead. Both language Gateways retain terminal writers.

Consequence: process death or an event write failure after the run update can leave a terminal row without a replayable terminal event. Independently, readers/writers need to know whether assistant_runs is execution authority or a projection. A status-only fix does not close this window.

Target: Runtime is the managed-execution terminal authority. Commit its owner-fenced terminal fact and corresponding durable event together; notify subscribers only after commit. Gateway owns admission and accounting fields, not a second execution state machine. Keep the existing shared tables initially, but specify permitted mutations/columns or stored procedures rather than claiming a schema name has one universal owner. Do not rewrite the schema or introduce an event broker merely to make this atomic.

Migration: first canonical status; then transaction primitive + fault tests; then switch managed producers and Gateway readers; finally remove legacy execution-state mutations after mixed-version checks. Preserve capability-result pairing before a terminal can be admitted.

### F3. Native process admission does not enforce the existing database compatibility boundary — confirmed source gap, P1 release gate

Evidence:
- `ai_gateway_contracts/database_revision.py:3–11` deliberately supports Python Gateway epochs2–10 and Python Knowledge epochs5–17.
- Python Gateway and Knowledge explicitly call read-only schema compatibility checks.
- No query of platform_schema_baselines/platform_schema_changes occurs in the reviewed Rust tree.
- Native Knowledge `knowledge_bootstrap/lifecycle.rs:113–121,171–203` probes SELECT1 before starting workers. Native Gateway worker setup probes selected table names, not an authoritative compatible epoch.
- SDK epoch17 changes behavior without requiring a new column. Table existence cannot distinguish older/incompatible function bodies.

Consequence: connectivity/SELECT1 can be green while an old/future schema is operationally incompatible. This is not evidence of a production incident, but it is an actual missing startup safeguard.

Target: one small, read-only per-process compatibility reader against the canonical authority ledger, with owner-reviewed baseline and minimum/maximum window from the release candidate. Run it before public readiness and worker startup. Missing ledger, unexpected baseline, holes/incomplete migration state or unsupported epoch must fail closed. Use actual application-role negative tests; do not grant migration rights to application code. The full native-enterprise candidate can conservatively require its reviewed epoch17 until a wider window is proven.

### F4. Deployment and readiness metadata do not describe the candidate being built — confirmed configuration drift, P1/P2

Evidence:
- `Dockerfile:144`, `docker-compose.yml:514,548` and dev overlay still launch Python Gateway/Knowledge.
- `docker-compose.sdk-agent-worker.yml:1–3` explicitly requires native Gateway and does not replace its image; this safeguard is correct.
- Helm Gateway `values.yaml:32–40` uses `/health` for both liveness and readiness. Native `/health` returns a static healthy response (`migration_host/readiness.rs:238–248`); `/health/ready` actually probes dependencies. Compose uses `/health/ready`.
- Parent has already integrated the missing native readiness router mount; do not duplicate that fix. Its native34 verification is a separate pending checkpoint.
- Compatibility manifest is intentionally draft with absent image/contract identities and omits the optional SDK Worker. Topology snapshots still describe the older service set.

Target: an explicit implementation/profile matrix and one reviewed readiness contract shared by deployment generators/checks. Keep live=process and ready=admission dependencies; optional worker unavailability should affect its capabilities, not necessarily plain model traffic. Reject "ready for production" while manifest remains draft or required paired runtime gates are absent. Add the optional worker to release/topology identity only when the profile includes it.

### F5. Native Runtime client bypasses the existing correlation contract — confirmed source parity regression, P2

Evidence:
- Native middleware creates RequestContext(request_id,trace_id,span_id,traceparent), but these response/log correlation fields do not prove an active local tracing span.
- `migration_host/context.rs` deliberately forwards traceparent/tracestate only from the in-process ActiveTraceCarrier extension. Raw incoming carrier headers are not promoted to active trace context. No production adapter installing that extension was found in this source review; absent outbound trace headers alone are therefore not evidence of a broken active-span propagation path.
- Native RuntimeControlPlane.headers (`migration_agent/control.rs`) bypasses the existing internal_http_headers helper, so even the safe request ID and an installed active carrier would be lost. Python `control/http_headers.py` uses its equivalent helper and OpenTelemetry injects only the active context.
- Runtime and Capability Worker already have bounded InternalTraceContext readers/forwarders. Detached Gateway capacity watching, cancellation cleanup and trace replay require explicit correlation capture because Tokio task-local state does not automatically cross spawn boundaries.

Bounded repair: route Runtime headers through the existing helper, preserve its active-carrier trust boundary, and capture only a bounded request ID plus the traceparent/tracestate allowlist for the same request's detached work. Reuse the Runtime/Worker trace validation rules without adding an execution-crate dependency. Configured internal auth and admitted tenant/user/session/run scope remain independently owned; correlation must never alter a nonce, signed lease, request body, authorization or capability proof. No arbitrary baggage, cookies or caller identity headers cross this boundary.

Verification: fixture the actual Gateway header builder, Runtime reader/forwarder, model metadata and Worker reader/forwarder; cover malformed/oversized/zero IDs, cross-request isolation, response-body polling, detached capture and absent-adapter behavior. Source wiring checks are separate from real route/transport execution. This slice does not install an OpenTelemetry adapter or claim complete tracing in a deployment without one; managed route/auth/Runtime↔Worker executable acceptance remains a release gate.

### F6. Native managed SSE uses distinct connection and idle budgets — bounded source repair, P2 acceptance pending

The reviewed native RuntimeControlPlane reused its `.timeout(300s)` command client for `open_events`, forcing even a healthy long SSE response to expire. This was a transport-policy mismatch, not proof of data loss: Runtime's durable log and the existing GET cursor watcher already supported reconnection.

The bounded repair routes only event GETs through `event_transport::EventTransport`: 10-second connect, 30-second complete-header deadline, 45-second per-read idle budget and no whole-response timer. The Runtime's existing 15-second keep-alive comments refresh idle liveness. The original 300-second command budget, signed finite model authority, capacity-watch execution window and five-second interrupt-request budget are unchanged; a successful heartbeat cannot renew execution authority. Existing monotonic GET cursor recovery and event processing remain unchanged. No mutation retry or SDK deadline expansion is introduced.

The owning transport contract is in `agent-runtime-durable-recovery.md`. Exact source checks and provided scaled loopback tests are distinct from executed native-route, real Runtime heartbeat/reconnect and paired Runtime/Worker acceptance. The active program ledger must state which gates actually ran; this source repair does not establish release acceptance.

### F7. Generic async task durability differs from scheduled/runtime/Knowledge durability — documented limitation with real consequence, P2 if exposed as durable

Evidence: native bootstrap uses MemoryTaskQueue (`bootstrap/core.rs:93–98`). `task_worker.rs:4–15` explicitly says no claims/recovery and destructive pop can lose work. `task_manager.rs:6–14` says save-before-enqueue and status-only cancellation. Scheduled execution, SDK runs and Knowledge all have separate durable contracts.

Target: document the difference immediately. If generic task durability is a required enterprise feature, migrate that queue to a database outbox/claim+fence within its current owner, with explicit unknown-dispatch reconciliation and worker cancellation semantics. Do not put all domain jobs into a universal workflow engine. Do not call a generic queue task an SDK run or a managed turn.

### F8. Native crate boundaries follow migration batches, not all domain boundaries — structural debt, P3

Evidence: `platform-gateway-integrations/src/lib.rs` includes provider adapters, local node, content, Knowledge query and ingest. Both Gateway and Knowledge binaries live in platform-gateway; foundation exports numerous domain HTTP/store modules. In contrast platform-client depends only on transport/serde and desktop delegates to it, which is a good boundary.

Consequence: code ownership and compilation blast radius are harder to isolate. No measured latency or security defect follows from this fact alone. Defer extraction until terminal/startup contracts are correct, then extract one high-churn domain behind existing ports if dependency/build measurements justify it. Do not split modules merely because line counts are large; quiz_unicode's generated data length is not an architecture defect.

## 4. Target responsibility and protocol rules

| Fact or command | Authority | Compatibility rule |
| --- | --- | --- |
| Account, tenant, role, credential revision, service access | Gateway auth/admission | Current actor revalidation; never trust a client identity header or SDK-chosen owner. |
| Managed run admission, launch snapshot, model lease, billing | Gateway | Immutable signed snapshot; accounting completion is distinct from execution terminal state. |
| Managed run execution, cancellation/recovery decision, ordered terminal event | Runtime | One canonical managed status mapping; commit event and terminal fact atomically under owner/fence. |
| SDK execution owner/expiry/run lifecycle | Explicit native Gateway SDK authority + official SDK Worker receipt | Separate engine identity; same public owner-scoped observation, no fake Codex identity and no automatic cross-engine failover. |
| Capability attempt/dispatch/result | Capability Worker | Stable original execution ID, exact signed lease/snapshot, approval consume once; unknown write cannot become retryable failure. |
| Approval policy and user decision | Shared policy projection; engine's established durable transition authority | Share action binding and fixtures, not an invented universal kernel. Existing SDK SQL and managed Runtime paths must produce equivalent exact-action semantics. |
| Dataset/document generation, ACL and retrieval result | Knowledge | Signed request envelope/current actor/ACL; no cross-domain direct write. |
| Rendering, reconnect, local request abort | Clients | Server facts remain authority; abort transport does not prove remote cancellation or zero provider cost. |
| Baseline/migration ledger and role grants | Canonical database authority | Apps read compatible epoch; only migrator changes schema. |
| Profile and deployed executable/protocol identity | Release manifest | Draft source receipts never count as a live service identity or approved rollback image. |

Keep separate protocol families where semantics differ: managed `agent-event/v2`/legacy v1 projection, Capability Contract V2, SDK private protocol/StreamChunk, and signed Knowledge v2. Add a small contract inventory listing authoritative producer, consumers, schema/fixture hash, status vocabulary, auth scope, time budgets, safe retry class and rollback window for each edge. Do not replace all envelopes in one batch.

## 5. Upgrade and coexistence matrix

| Profile | Gateway | Knowledge | Runtime / Capability Worker | Official SDK Worker | Database / rollout condition |
| --- | --- | --- | --- | --- | --- |
| Retained Python profile | Python, only its supported features | Reviewed Python image | Only the exact compatible released pair | Disabled | Gateway startup range2–10 is intentional; actual release pair still needs its own evidence. Not a fallback for the current epoch17 native candidate. |
| Hybrid native enterprise candidate | Current native Gateway candidate | Retained Python Knowledge or native candidate, explicitly chosen | Same pinned source/release unit; current executable/image gates open | Optional explicit service, version/hash-pinned | Conservatively epoch17 for full feature set, after canonical migration. Retained Knowledge accepts5–17; native range must be implemented/reviewed, not inferred. |
| Native Knowledge candidate | Same authorized Gateway profile | Native API/worker roles | Unchanged unless its contracts change | Independent opt-in | Validate ACL, signed auth/replay, generation lifecycle, parsers/embeddings and schema before switching traffic. No need to migrate it solely because Gateway is native. |
| SDK-enabled candidate | Native only | Explicit ACL-compatible Knowledge implementation | Paired SDK-capability-aware Runtime/Worker when governed tools selected | Single-owner private worker/state volume | Epoch17 parent lineage, epoch15 approval authority and epoch16 audit lookup all preserved. Pure/KB and governed modes each need acceptance. |

Roll forward additively. Do not run older Python Gateway at epoch17 by relaxing its maximum. Rollback means an actually compatible code+schema+data plan; the existence of old Python files does not supply one. Do not transform SDK state into managed sessions or delete its durable volume to make rollback look clean.

## 6. Migration sequence and proof gates

### Stage A — Current facts and one terminal-vocabulary repair
- Update architecture entrypoint with retained/candidate/optional implementation distinctions and current ownership.
- Integrate the externally staged minimal recovery-terminal producer/consumer repair after exact preimage review.
- Use a shared positive/negative fixture: managed succeeded, failed, cancelled; legacy recovered completed recognized as terminal without claiming success; provider/SDK/tool completed unchanged; unknown input cannot become success.
- Gates: targeted Runtime unit test compile/run when feasible, real native/Python consumer tests, known no-build source checks separately labeled, current-source manifest. No whole Runtime rebuild required merely to stage/review the patch.
- Exit: one narrow source contract fixed and correctly labeled; executable verification remains pending if its existing build blocker remains.

### Stage B — Read-only schema/profile admission and readiness agreement
- Add owner-reviewed compatibility windows to native process startup before workers; preserve retained Python guards.
- Fix Helm readiness default to the real readiness route, with deployment tests proving unhealthy dependencies are not admitted.
- Add profile implementation/optional-worker metadata to the existing release/topology owners, without stamping a release complete.
- Gates: wrong baseline, empty/missing ledger, unsupported low/high epoch, allowed exact epoch, application-role access, no worker before guard, config/Compose/Helm route consistency. These can be isolated from providers and the large Runtime executable.

### Stage C — Atomic managed terminal authority
- Add one owner-fenced Runtime terminal persistence primitive that commits paired terminal event and run state together.
- Cover ordinary completion, cancellation, recovery of persisted Core terminal and expiry. Keep model/accounting updates separately owned and idempotent.
- Gateway terminal consumers stop generating execution outcomes after the compatible producer lands; legacy consumers must remain safe during the rollout window.
- Gates: crashes before transaction/after row update/before event/after commit-before-broadcast; retry same event key; duplicate terminal; cancelled-vs-completed race; stale owner; event ordering and original capability receipts. Require real PostgreSQL transactions and source-matched Runtime execution before release.

### Stage D — Private transport contract parity
- Restore native Runtime request-ID and installed-active-carrier propagation first, preserving the existing no-raw-carrier trust boundary; then state per-edge timeout/retry policy.
- Capture bounded correlation explicitly for same-request detached Runtime work; do not claim an active tracing adapter exists merely because middleware emits a response traceparent. Use engine-specific scope and preserve existing Knowledge signatures and capability proofs.
- Gates: round-trip fixtures, malformed/replayed auth, tenant/run/args substitution, stream idle/disconnect/reconnect, no POST retry under uncertain outcome, provider/Worker late result classification.

### Stage E — Product-safe client convergence and durable queue gaps
- Run the same engine-aware event/result fixtures through Web, platform-client, Python/Java/Dart SDKs and independent CLI where applicable.
- Keep SDK run status/cancel/approval and managed cursor subscriptions explicit; support empty completion, early identity, refresh, late usage and unknown cancel outcomes without fabricating success.
- Decide generic async queue durability based on product requirements. If required, add its own outbox/claim/fence instead of reusing run IDs from another domain.
- Gates: cancel during approval, owner/account revocation, repeated click, refresh/back/forward, disconnect with no terminal, and read-only recovery without duplicate submission.

### Stage F — Measured structural simplification
- Only after A–E, consider extracting pure contract and Knowledge implementation crates behind established ports.
- Compare incremental compile scope, dependency graph, test runtime and release risk before/after; preserve all public wire hashes.
- Stop if extraction offers only renamed files or shifted coupling. No new service boundary, language rewrite or provider-framework abstraction is required by this proposal.

## 7. Verification limits and current checkpoints

This review established source facts by reading files and searching all relevant Rust paths. It did not execute a failure reproduction, compile tests, start services or apply data changes. Confirmed source defects are not labeled observed production incidents.

Preserve the parent checkpoint distinctions: native Gateway's readiness route fix is integrated and native34 real HTTP acceptance is a separate active task; the latest Runtime executable had three SIGKILL attempts with cause unproven. Do not attribute those kills to out-of-memory without evidence and do not repeat expensive builds merely to finish this report.

Current official-SDK synthetic loops, wheel reproducibility, sequential real-role SQL and Web/unit gates are useful completed work. They do not replace paid-provider, paired Runtime/Worker, browser, platform or production release acceptance.

Companion artifacts: source-evidence.json (exact inspected source hashes), recovery-terminal-proposal/ (bounded staged source and tests, separate review), and entrypoint-proposal/ (current architecture entrypoint draft). The existing native-enterprise loop-state remains the sole implementation/evidence ledger; this proposal creates no parallel execution status authority.

## Performance acceptance addition (2026-10-12)

Performance is a core product gate alongside correctness and recovery. The [enterprise performance contract](enterprise-performance-contract.md) defines workload identity, metric populations, latency tails, throughput/errors, CPU/RSS, streaming, queues, retrieval and overload boundaries. Its initial bounded GET harness has offline regression evidence only; it has not measured deployment capacity. The [native adapter line framer](native-stream-framing.md) separately removes quadratic suffix movement and repeated delimiter scans, with exact-helper and operation-count evidence; this is not an end-to-end latency claim. Model waiting must remain distinct from Gateway overhead, and optimizations must preserve tenant authorization, accounting, cancellation and output semantics.
