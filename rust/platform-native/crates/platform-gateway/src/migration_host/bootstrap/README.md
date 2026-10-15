# Durable Gateway construction

`GatewayGraph::new` is the explicit constructor boundary for the PostgreSQL + Redis + registered Agent Runtime deployment profile. It accepts resolved typed configuration, independent data/control pools and already-owned Redis connectivity. It does not read environment files, discover secrets, connect pools, start workers, contact services, bootstrap schema, or bind a listener.

Construction order follows the dependency relationships in `src/container.py` and `src/main.py`:

1. Trace/usage repositories and recorder.
2. Live model/provider services and shared quota authority, then registry with all source protocol adapters; durable sessions and tasks; in-process task queue; RBAC, rate limiting, capacity admission, priced dispatcher, health monitor and task worker.
3. Current-account JWT/API-key authority.
4. Signed Knowledge source authorities and shared content access.
5. LangGraph clients, guest sessions, object/attachment/content/image services and private capability brokers.
6. Native Agent control/model planes, local capability catalog, memory, Studio repositories and sealed-resource authorization.
7. Shared admin state, concrete Foundation APIs, compatibility endpoints and signed public Knowledge proxy.

`GatewayWorkers::new` constructs the existing billing-maintenance scheduler. `GatewayGraph::lifecycle` binds real worker operations and owned task handles. Obtain this lifecycle before consuming `GatewayGraph::into_router`, then give the router and lifecycle to `GatewayHost::assembled`. Listener binding remains the host's explicit operation. File expiry and retention only run after lifecycle startup; construction does not authorize or perform cleanup.

The process owner may supply additional configured startup/shutdown operations as actual `LifecycleStep`s; the canonical eval-outbox worker is already assembled. No success-only substitute is installed. The image worker is a single-process lifetime object: its source implementation's stop flag is terminal, so construct a fresh graph after stopping it.

## Explicit remaining boundaries

Graph construction now supplies concrete callbacks, security persistence, live model/provider preparation, Google authentication, pricing cache, object storage, artifact/history authority, timestamp codecs, and Foundation service/runtime/proxy/quota/audit adapters. Poppler is an explicit bounded PDF alternative, not strict pypdf parity. Adapter and typed REST LangGraph preparation share live model authorities and quota/RPM state.

`FoundationHostPolicies` retains actual deployment authorities: Confluence registration, MCP secret resolution, configured policy publishers, health cancellation, architecture status and Local Node channel verification. Configured Local Node device forwarding remains an explicit optional authority. The executable now constructs frozen-memory SQL/file/vector cleanup using captured deployment settings. Missing cache objects are represented as absence, never success-only invalidation implementations.

MCP credential resolution is shared between discovery and execution; the discovery broker uses the exact same repository instance as registry administration. Knowledge business operations remain signed HTTP requests to Knowledge Service.
The independent Knowledge PDF and asset-URL profile is documented in
[`../knowledge_bootstrap/README.md`](../knowledge_bootstrap/README.md).

This profile cannot represent all Python disabled/degraded startup branches yet. Existing `AgentState` requires a control plane and existing `AuthService` requires Redis plus a valid HS256 codec. The graph surfaces configuration/construction failures instead of fabricated readiness. `GatewayDocument` projects the supported durable profile and supplies its concrete deployment backends.

This writer used cached rustfmt only. Aggregate build verification belongs to the parent validation task; no service, provider or listener startup was performed here.

## Executable input and process ownership

`src/bin/gateway.rs` accepts `--config-stdin` and a JSON object with three fields:
`settings` is the resolved source settings object; `environment` is an explicitly
supplied string mapping; `native` selects deployment details that cannot be
inferred safely (Runtime kernel revision, worker-write policy, local timestamp
offset, private Knowledge HTTP opt-in, sandbox endpoint/token, Poppler paths and
attachment-alternative opt-in). Local-channel/legacy registrar modes are
`disabled` or `required`; publication modes are `absent` or `required`. Required
local-channel/legacy registrar implementations fail startup. Required model/provider publication uses the shared Redis connection; absent explicitly disables it. Google ADC values and metadata origin are
optional explicit inputs. The DTO definitions in `configuration.rs` are the
accepted schema; unknown deployment fields are rejected.

`--check-config-stdin` validates projection only. It does not open connections,
seed providers, run PDF programs, contact models, verify readiness or bind ports.
Successful projection is not a runtime-readiness claim.

The explicit process startup connects pools/Redis, initializes provider records
and the local model catalog through the mature source seeder (`discover=false`),
and passes those exact model/provider instances into the graph. Real evaluator,
image, file-expiry, billing, task and observability workers are lifecycle-owned.
Canonical watermark bytes are embedded from the existing source asset.

HTTP shutdown allows 30 seconds for graceful drain and a separate 30 seconds for
cleanup. A deadline returns an error. Dropping Axum's server future does not prove
all connection tasks stopped; the executable then ends its owned Tokio runtime.
An embedding host must not reuse resources as though deadline cancellation had
finished. No credentials, providers or listeners were exercised by this writer.

The captured `INTERNAL_AUTH_KEYS` CSV map and `INTERNAL_AUTH_ACTIVE_KEY_ID`
select outbound signing and retain all verification keys. Frozen-memory cleanup
uses `AGENT_RUNTIME_MEMORY_DIR` (absolute path required) and the canonical
Qdrant URL/API-key setting precedence. No cleanup occurs during construction.

An explicitly configured `EVENT_BUS_REDIS_URL` enables the canonical usage
Redis Streams publisher and logger consumer, including group reclaim, delivery
counts, deduplication and dead-letter handling. The consumer and transport are
lifecycle-owned; an absent URL retains the source opt-in disabled behavior.
