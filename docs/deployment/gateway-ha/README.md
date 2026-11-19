# Gateway multi-instance source contract — U-GW-14

This change closes the six identified source-state seams through explicit HA
constructors, stores and owner routing. It does not deploy multiple Gateways or
retire the single-instance release assumption. The actual Gateway process now
constructs and supervises this opt-in profile; deployment remains singleton until
the acceptance gate passes. Root owns serialized Cargo/integration execution.
The architecture/ADR retirement gate remains in effect until real source-bound
chaos, fencing, zero duplicate execution and capacity evidence are approved.

| State | HA implementation | Failure/recovery rule |
| --- | --- | --- |
| Quota RPM | Existing Valkey GCRA, `QuotaRpmLimiter::shared`; HA gate requires its real shared backend | Server TIME; two replicas cannot double the quota. Finite quotas never fall back to local counters. |
| Context metrics | `multi_instance::context::ContextStore` and `ContextMetricsCollector::shared` | Atomic immutable event deduplication, global recent 100 observations, 7-day idle retention; read/write failure is 503/error. Session reads also filter tenant. No reconstruction from accounting rows. |
| SDK provenance grants | Owner-local JWT/grant authority with 64-hex instance/incarnation hints; shared expiring Directory; `gateway::ha::ProvenanceRouting` | Exact owner receives calls over admitted workload mTLS. Unknown/dead owner fails closed. No JWT is stored in Valkey, copied to peers, or resurrected after restart; no unsafe operation is retried. |
| Generic task queue | Existing reviewed PostgreSQL `PgTaskDeliveryLedger` via `readiness::durable_tasks` | HA rejects managers using the legacy memory/Redis-pop queue. Pending work is claimable once; claimed/unknown work requires existing reconciliation and is never reset or replayed after a crash. |
| Guest sessions | `AtomicGuestStorage` + `GuestSessionManager::shared` | Bounded opaque CAS retries, no lost additions or deleted/expired resurrection. Conversion claims a retained reconciliation marker before external effects; cancellation/crash cannot trigger automatic repeat migration. |
| Dashboard WebSockets | Global/tenant Valkey lease admission + existing local physical-socket semaphore | Established socket stays on its accepting replica; reconnect may use any replica and obtains a new lease. Lost renewal disconnects the socket; TTL reclaims killed-process capacity. No persisted socket replay is needed. |

Legacy constructors remain for explicit single-instance/parity tests. They are
not valid HA choices. `multi_instance::readiness::require_shared` checks all six
real construction choices, including matching provenance grant/Directory boot
prefixes. A boolean replica count or a release-note inventory cannot bypass it.

## Production construction and opt-in

The actual executable calls `GatewayDocument::construct`, which parses
`bootstrap::ha::Configuration`, requires the admitted `CoordConnection`, and
passes one `Arc<ha::Runtime>` into
`GatewayGraph::from_model_services_with_ha`. Before listeners can start, that
graph configures all six real authorities and calls `require_shared` with the
objects used by the business routers. There is no separate boolean readiness
substitute. Existing constructors select the established singleton profile.

No dependency or SQL change is required. HA uses the existing coordination ACL
(`gateway:*` and `guest_session:*`), canonical task-delivery admission and workload
mTLS. Context, dashboard and directory scripts use Cluster hash tags; each guest
CAS is one key. A legacy Redis/cache-only connection does not satisfy this mode.

Set `AI_PLATFORM_GATEWAY_HA` to bounded JSON in the ordinary native configuration
environment, for example:

```json
{"enabled":true,"instance_id":"gateway-api-0","origin":"https://10.42.0.7:9444","lease_seconds":60,"dashboard_global":512,"dashboard_tenant":128}
```

Absent configuration, or `{"enabled":false}`, retains the singleton profile.
The enabled profile requires workload mode `required`, a durable or
schema-admitted task delivery mode, and split coordination. Invalid input fails
configuration/startup; there is no memory or bearer fallback. Check-config also
validates the HA shape, workload mode and durable selection without claiming
that dependencies were reached.

Each stable instance ID must be unique across live API/worker/eval processes.
The origin must resolve to exactly that process's private TLS bind address and
port. A load-balanced service alias, ambiguous multi-address DNS result, public
HTTP URL, arbitrary callback path or wildcard bind cannot represent an owner.
Every replica's `AI_PLATFORM_WORKLOAD_PEERS_JSON` must contain this and all peer
origins mapped to the expected Gateway SPIFFE identity. Its inbound exact
`AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS` must admit that Gateway identity.
Existing file credential rotation and strict private-key handling apply.
The provenance client keeps only origins mapped to its own Gateway service
identity; Runtime, Knowledge and provider destinations cannot be used as relays.

The actual hooks are:

- `bootstrap/graph.rs`: coordination-backed RPM, atomic guest storage, immutable
  shared context collector, startup-frozen provenance prefix, existing PostgreSQL
  durable task manager/actor validator, and shared dashboard leases. The durable
  schema is checked even in API-only roles. The dashboard is configured before
  any router clones it.
- `bootstrap/workers.rs` and `bootstrap/ha/supervisor.rs`: Directory registration
  is the first lifecycle effect after role filtering. Each subsequent activation
  checks lease health before and after its operation. Heartbeat failure or a
  bounded heartbeat timeout makes readiness unavailable, begins the real HTTP
  drain and stops the real task worker and PostgreSQL singleton supervisors.
  Loss is terminal for that process, with no automatic Directory re-registration.
  Shutdown releases the CAS lease after worker cleanup and before connections
  close. Old owners cannot extend/delete replacement leases.
- `bootstrap/process.rs`: the assembled router receives `ProvenanceRouting`
  before the existing workload TLS gate is applied. All seven current SDK
  capability/knowledge/model callback routes retain their complete owner
  grant/JWT/session/domain checks. A wrong accepting replica relays once to the
  exact live owner over workload mTLS. Cookies, identity hints and hop headers
  are stripped; no redirect, unsafe retry or caller-selected URL is admitted.
  Worker/eval processes retain health/metrics-only public HTTP; their private
  companion exposes only these seven callbacks so background SDK grants remain
  reachable at their actual owner.
- `application.rs`, `model_guarded_stream.rs` and
  `knowledge_proxy/sdk_models.rs`: readers and actual Runtime/SDK model producers
  share the same collector. The producer measures the final provider-bound
  input/instructions/messages/tools after translation, ownership checks and
  input guardrails, before dispatch. Stable admitted model-call/lease IDs dedupe
  observations across instances. A shared write failure rejects dispatch.
  Metrics never come from usage rows, quota reservations or caller token counts.

Context `total_tokens` is explicitly a codepoint estimate of the prepared text
context and tool schemas. The metadata reports `prepared_provider_request`,
`codepoint_estimate`, the trusted context window and whether non-text modality
costs are unknown. No raw prompt, message, tool schema or provider key is stored.
Compression ratio and cache hit rate are `null` when not observed; averages use
known observations only. Older numeric metric records remain readable. This
diagnostic observation measures prepared context, not successful provider usage,
and does not substitute for accounting or claim an exact tokenizer count.

## Verification and retirement

Root uses `scripts/new/native-container.sh` and its one shared cache. Suggested
scopes (not executed by the source worker):

```
cargo test --locked -p ai-platform-gateway-foundation --lib multi_instance
cargo test --locked -p ai-platform-gateway-foundation --lib sdk_read_grants
cargo test --locked -p ai-platform-gateway --lib context_metrics
cargo test --locked -p ai-platform-gateway --lib ha::
cargo test --locked -p ai-platform-gateway --lib context_metrics::producer
cargo test --locked -p ai-platform-gateway-foundation --lib task_durable_worker_tests
cargo check --locked -p ai-platform-gateway-foundation --bin gateway-ha-chaos
```

The ignored `multi_instance` integration tests require a **disposable, isolated**
Valkey supplied through private mode-0600 `AGENTFORGE_HA_TEST_REDIS_URL_FILE`.
They use the actual Lua/storage paths across independently constructed replicas.
Run them serially (`--test-threads=1`); one test waits 16 seconds for real expiry.
The context/dashboard tests use their production coordination namespace and must
never run on application coordination data. Ordinary tests force guest CAS races,
backend failure and expiry without starting a service. Existing durable task
worker tests cover duplicate claims, fencing and unknown-effect recovery.

The `gateway-ha-chaos` native binary executes an explicit private JSON plan. It
checks executable digests, starts only declared child binaries, sends bounded
probes to exact HTTPS or literal-loopback HTTP origins, can kill-9 only those
children, compares captured JSON fields, and measures bounded read-load RPS/p95.
It has no arbitrary PID, Docker, deployment or automatic retry action. A failed
step stops the run; managed children are stopped on exit. Logs are private 0600,
output root 0700, receipt contains hashes/statuses rather than credentials or
response bodies. It always writes `single_instance_assumption_retired=false`.

Use a deterministic provider fixture and two real Gateway processes. Exercise
same authenticated scope through both peers, shared quota exhaustion, guest
updates/conversion, context observations, provenance callback routing and socket
capacity. Kill the owner/claiming process during an in-flight task/tool operation;
verify takeover only for unclaimed work, old-fence refusal, persisted unknown
outcomes and **zero duplicate provider/tool effects** using the fixture's own
execution counter. Probe the surviving peer after the kill and measure the
owning performance contract. Do not treat helper-process or mocked-store tests
as that acceptance evidence.

`RetirementEvidence::permits_retirement` still requires executed runtime checks,
at least two instances, kill-9/fencing success, zero duplicates, all state checks,
positive measured capacity with its capacity contract passed, source identity and
an approved ADR. The source change, a passing probe plan or an empty inventory
alone never changes the current single-instance release assumption.
