# Execution state, workload transport and SDK model-plane contracts

This document owns the execution-side source contracts for U-RT-02/03,
U-CW-03/07 and U-SW-01/02. It does not activate a migration, change the single
Runtime deployment guard, admit an isolation runtime or establish acceptance.

## Managed Runtime state and takeover

| State | Owner | Takeover rule |
| --- | --- | --- |
| Native rollout, V1 event sequence and thread membership | PostgreSQL | Read with the original tenant/user/session membership; never reset history. |
| Run owner/fence and original start request | PostgreSQL execution owner | Existing run claim function increments the fence after lease expiry. Every model/tool/store boundary retains its original fence checks. |
| Read-only binding payload | Immutable scoped runtime snapshot | Reload under the active model lease and original snapshot/revision. The process cache is an optimization, never authority. |
| Transport budget | Immutable execution-context/v2 in the original owner context | Persist the admitted absolute deadline and hop ceiling before kernel admission. Recovery intersects the saved triplet with live DB authority; it cannot grant another 300 seconds. Legacy raw start-request contexts decode under the original database lease. |
| Cancellation intent and terminal cancellation receipt | PostgreSQL run/lease/event transaction | Any instance commits cancellation first. A commit-scoped, payload-free control NOTIFY wakes owner authority checks. NOTIFY loss retains the existing durable recovery backstop. |
| Dynamic approval decision and original required/result receipts | PostgreSQL | Original run/call/arguments/expiry are immutable. A missing waiter reads the same durable decision. Network acknowledgement loss may fall back to the idempotent durable decision/readback path. Native kernel approval destinations retain their destination-kind rejection. |
| Cancellation tokens, kernel approval waiters | Current owner process | Recreate tokens on recovery; notify or interrupt the live local kernel. Never infer an effect outcome from an absent waiter. |
| Thread gates and recovery set | Current owner process | Gates prune only when their map holds the sole Arc; active/waiting locks survive pruning. A recovered run reacquires its owner fence before resuming. |
| Event subscribers and hub | Subscriber process | One channel per subscribed thread. Subscribe before durable replay; notification hints do not grant membership. Last drop removes the channel. |
| Trace correlation and kernel text projection cache | Current process | Optional correlation may disappear on restart. Persisted history/events determine business state. No trace attribute grants scope or execution authority. |

The optional routing configuration is `AI_PLATFORM_RUNTIME_OWNER_ADDRESS` plus
`AI_PLATFORM_RUNTIME_OWNER_PEERS` (a bounded comma-separated exact origin set).
Both must be present; workload mTLS must be required. Every owner address read
from PostgreSQL must match the configured set. The unnumbered thread-owner
proposal adds an address, monotonic fence and 30-second lease per root thread.
Admission locks run, root and owner in the same order as existing recovery.
Run claims remain authoritative for external effects.

`GET /internal/v1/threads/{thread_id}/owner` returns the live owner after normal
scope authorization. Commands arriving at another Runtime forward once to the
owner. A second forwarding hop rejects; redirects and automatic retries are
forbidden. A lost start acknowledgement remains unknown. SSE stays local and
reads the shared durable event log. Interrupts commit locally even when the owner
is unreachable, then wake its durable authority scan. Dynamic approval receipts
remain readable if an owner acknowledgement is lost.

Multi-instance activation still requires the existing ADR and two-instance
crash/fence/duplicate-execution acceptance. The new functions and scoped ACLs must
be admitted through the native migration authority before enabling routing.
Readiness is false when enabled routing lacks its schema. No frozen schema,
fingerprint or existing terminal authority function was changed.

The history corpus gate remains the release authority for rollout compatibility.
A new execution-context envelope is not proof that an older Runtime binary can
resume new contexts. Until rollback decoding is measured, use stop-write plus
restore; do not claim an unrestricted old-binary rollback.

## Workload transport and verify-only service startup

Production Runtime and Worker bootstrap reject
`AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET` and legacy/transition trust modes.
They require the Gateway public verification keyring. Explicit legacy library
fixtures remain test/migration helpers and are not production configuration.
The Worker no longer exposes a capability lease signing-secret CLI argument.
Broker proofs use the Gateway-only Ed25519 V3 issuer at
`POST /internal/v3/capability-broker/proofs`. Worker persists the original sealed
call, descriptor hash and execution ID before reservation. Every issued proof
revalidates the original lease, live dispatch fence, worker lease, run, approval,
revocation and exact allowed body transformation, then expires within 30 seconds
and the original call deadline. Gateway and Knowledge verify the public ring on
each request. A revoked key stops authorizing without a consumer restart.
Consumers never hold the Gateway private signer or HMAC proof secret.

V1 is admitted only during an explicit `AI_PLATFORM_BROKER_PROOF_MODE=transition`
window ending at `AI_PLATFORM_BROKER_PROOF_HMAC_ACCEPT_UNTIL_MS` (at most 24 hours).
Required workload or verify-only lease/proof mode forbids this window. A failed
V3 proof never falls back to V1. Legacy deterministic library fixtures explicitly
supply their bounded window; default production authorization has no such window.

Outbound workload transport uses the canonical native primitives copied by the
controlled generator into `shared_workload_identity`, with these public inputs:

- `AI_PLATFORM_WORKLOAD_MTLS_MODE=required` (or explicit migration `dual`).
- `AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR`: file generation root containing
  `current/{ca.pem,cert.pem,key.pem}`; no private CA signer enters a service.
- `AI_PLATFORM_WORKLOAD_SPIFFE_ID`: exact local SVID identity.
- `AI_PLATFORM_WORKLOAD_PEERS_JSON`: exact HTTPS origin to expected peer SPIFFE map.
- `AI_PLATFORM_WORKLOAD_TLS_BIND` and `AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS`:
  explicit private listener address and admitted caller identities.

Each internal dispatch checks its final origin and peer identity, loads the
current credential generation and disables proxies, redirects and retries.
Required mode rejects retained shared internal-token configuration. Domain run,
model and capability grants remain independently verified. Public-web/provider
clients never receive this transport. Gateway initializes outbound policy from
its authoritative stdin environment document, including outbound-only roles.
Gateway routes bridge only the root listener's verified TLS extension into
legacy handlers; identity hints supplied as HTTP headers grant no trust.

Runtime, Worker and SDK retain their original HTTP port for credential-free
health/readiness. Required business traffic uses the separate private TLS port;
old plaintext business routes reject even a correct historical shared bearer.
The SDK-specific old TLS listener is available in disabled workload mode; an
ambiguous combination of old TLS settings and enabled workload TLS fails startup.

The unmodified upstream model client uses a platform-owned loopback bridge.
Set `AI_PLATFORM_RUNTIME_MODEL_PROXY_BASE_URL` to
`http://127.0.0.1:<runtime-port>/internal/v1/model-proxy` and configure the actual
Gateway model-plane target. Its separate `AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN`
must contain at least 32 bytes and be distinct from platform/domain credentials.
The bridge checks the actual loopback socket peer and local token, forwards the
original signed turn metadata to the fixed Gateway using workload mTLS, and owns
no model iteration. No upstream harness source is changed.

Runtime and Worker load file DSN/password and VerifyFull client TLS through
`DATA_RUNTIME_*` and `DATA_CAPABILITY_WORKER_*`. They reuse native
`ConnectionConfig` role, physical database, connection budget and non-superuser
checks. Backend-pinned direct/session modes preserve LISTEN, PID and lease
semantics; transaction pooling and inline database credentials reject. Existing
schema admission and session timeouts remain in place. The controlled generator
must copy native `data_connection/config.rs` into `shared_data_connection/config.rs`;
there is no composition-tree-external include or secret export through env/argv.

## Strong sandbox admission and pinned Python

The dedicated sandbox verifies Python 3.14 even for stdlib-only jobs. Tenant
wheel selection retains the existing hash-locked catalog and dependency-closure
Landlock roots. The control worker still dispatches only sealed jobs across the
credential-free Unix socket; it never falls back to in-process Python.

Strong mode adds `--required-runtime gvisor|kata|firecracker`,
`--runtime-evidence-file`, and `--runtime-keyring-file`. All three are required.
The evidence must be signed for `sandbox-runtime-attestation/v2`, bind the
actual Linux boot ID and workload hostname, name the required runtime, and attest
no network, no credentials and one execution. Its lifetime is at most 15 minutes.
A label, unsigned marker or environment assertion cannot satisfy this contract.

The optional strong-pool deployment now includes a Rust Kubernetes controller,
a separate node attestor and a credential-free measurement init binary. The
controller creates digest-pinned gVisor pods and durably reserves each sealed job
for one fresh slot. The attestor checks the actual live CRI sandbox/handler,
container image references, exact launch policy and pinned running runsc inode
plus `--network=none` before signing. This is software measurement anchored in a
trusted node, not hardware or TEE attestation; missing observations fail closed.

Strong services accept one original job only. The pool fsyncs exact terminal or
unknown reply bytes before deleting the used pod with its UID precondition;
duplicates replay that receipt across controller restarts. A persisted cancel
intent blocks a delayed first dispatch and destroys the assigned pod without
inventing a terminal outcome. Tombstones prevent reassignment after receipt
retention expires; journal/socket/receipt budgets fail admission before execution.
Existing namespace/Landlock/seccomp/cgroup checks remain mandatory. Unsupported
runtime/kernel/CSI combinations fail closed without a weaker tier.

`deploy/sandbox/README.md` owns provisioning and packaging inputs. The optional
Helm template supplies node-pinned lifecycle control, independent attestor keys,
namespace network isolation and admission policy. Root must connect its values,
schema fragment and Worker socket mount. Real RuntimeClass support, escape tests,
credential inspection, cold/warm latency and install/upgrade/rollback remain
required acceptance gates and have not been executed by this source change.

## SDK execution and model accounting

The SDK worker rejects provider-key headers and provider-key/base-URL environment
configuration. Its private engine frame is `{request, model_plane}`. The model
plane includes an opaque run grant, fixed Gateway execution/revocation URLs,
hard expiry and a Gateway-authenticated hash of the actor/session/service/request,
framework and model profile. This binding is checked before the worker touches
local session state and again before the engine reads its home. No provider key
is copied into the frame or child environment. Import-window parity modules and
existing read-only session importers are retained.

The Gateway SDK routes are:

- `POST /internal/v1/sdk-tools/models/lease`
- `POST /internal/v1/sdk-tools/models/execute`
- `POST /internal/v1/sdk-tools/models/revocations`

The first route validates the existing real SDK run/grant and the admitted model
schema. Each call has deterministic `run_id:iteration` identity and the original
turn budget, instructions and model. The Gateway resolves a registered model and
provider, freezes price book v2, reserves canonical user quota, acquires provider
capacity and applies input/output guardrails. Unknown provider effects are never
redispatched. Output guardrails hold back complete content, including reassembled
Anthropic/Responses deltas; metered usage still settles when screened output is
blocked. Only metering facts, hashes and immutable receipts are stored by this
model plane, not an additional prompt/output capture.

Every successful model response ends with a settled `sdk.model_receipt` from the
Gateway. The engine rejects missing, duplicate or differently bound receipts and
reports their platform cost. `model_plane_settled` excludes these already charged
calls from the existing outer SDK two-rate accounting path. Provider-reported
usage remains an observation. The independent SDK call ledger uses the existing
canonical quota/daily/hourly accounting stages within the settlement transaction.
It does not fabricate a managed Runtime session or reuse Knowledge scope.

Revocation SSE uses commit-scoped PostgreSQL wake hints and revalidates the
original grant on every wake. Hard expiry, stream loss/malformed events and the
original ten-second authority poll independently stop the supervised process
through its existing process-group cleanup. New SQL triggers cover run terminal,
SDK lease/revocation and actor/session/service policy publication. The Gateway
model stream also checks live authority while waiting for provider data.

The SDK callback TLS facade must allow these three exact POST routes. The
unnumbered SDK model-plane/notification proposal needs native authority admission,
scoped grants and measured witnesses. Real per-call receipt/rollback/quota/cache,
revocation-p95, provider-wire and credential-inspection gates remain unrun until
the primary performs the serialized builds and paired-service acceptance.
