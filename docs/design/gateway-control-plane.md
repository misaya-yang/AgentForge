# Gateway control-plane adapters

The control plane now has source implementations for U-GW-13/15/19/22/23/24.
These are implementation boundaries, not deployment acceptance. The main upgrade
ledger remains the authority for prerequisites and measured acceptance. MCP and
A2A stay absent from the default router. No deployment feature flag is changed.

## Explicit configuration

`gateway --print-config-schema` publishes the bundled v1 JSON Schema.
`--check-config-env` and `--check-config-stdin` validate schema, projection, Cedar
policy/matrix, protocol URLs/activation bindings, and signing-key syntax without
opening services. Existing unversioned resolved documents default to schema v1;
unknown top-level, native/PDF, control-plane and known settings fields fail closed.

`GATEWAY_NATIVE_CONFIG` may point to the legacy native-only deployment JSON or a
versioned source with `schema_version`, partial `settings`, `native`, optional
`environment`, `control_plane`, and `secret_files`. The latter is a map of JSON
pointers to absolute files, for example
`"/settings/authentication/jwt/secret": "/run/secrets/gateway-jwt"` with an empty
string at that settings field. A nonempty inline value and file are a conflict.
Secret files are regular files at most 64 KiB; group/world writable files are
rejected. A single final LF/CRLF is stripped; other whitespace is retained. Errors
omit values and paths. Secret files are not searched or exported to child process
environments. Explicit secret-valued `*_FILE` environment inputs also work,
including `GATEWAY_AUTHENTICATION__JWT__SECRET_FILE`; CA paths and the separate
Valkey password-file contract retain their original interpretation.

The `environment` map remains the explicit compatibility/provider environment
escape hatch. Nested known `GATEWAY_*__*` settings are validated, including the
existing static-role and S3 operational extensions; arbitrary provider environment
variable names are not converted into a claim that every ambient key is typed.
The reference deployment's conversion from long-lived inline secrets to files is
a separate deployment-owner integration and has not been performed here.

## Cedar rollout and decision logs

`control_plane.cedar.mode` defaults to `off`. `shadow` uses Cedar's actual strict
schema validator and Authorizer, preserves the existing authorization result, and
records the independent result. Tenant, User, Dataset, Agent, Tool and Model are
six distinct typed resources. Raw current membership grants, owner and tenant,
object lifecycle, current account bindings, model access level and scope/alias
relationships are facts. The legacy decision bit is never a Cedar entity or
context attribute; it is used only for differential audit and the final
non-widening intersection.

Capability flips keep their existing explicit `enforce` list and policy/matrix
pins. Resource flips use `cedar.resources.enforce`, an array chosen from
`tenant,user,dataset,agent,tool,model`, plus that section's own
`expected_policy_sha256` and `expected_matrix_sha256`. Enforce requires at least
one explicit flip and exact current pins for each selected policy family. The
finite differential basis covers every action and resource type, configured role
maps, alias/wildcard semantics, current grants, tenant negatives and current
object/account lifecycle. Evidence also hashes the actual owning ACL source.
An immutable proof cache reuses only the same compiled basis/policy/schema/source
and role-map evidence; request/object decisions and grants are never cached.
`ai_platform_auth::cedar::authorization_evidence` returns all four secret-free
pins for config tooling and operator review.

Object hooks retain the actual existing ACL and tenant SQL. Agent membership reads
and locks are observed at their current transaction boundary; creation and list
semantics have separate profiles so a legacy listing membership is not silently
promoted into an owner grant. Account mutations re-read and lock the tenant,
actor, current role binding and protected target inside the existing supervised
operation. The request policy handle follows that supervised SQL future even if
the HTTP waiter is canceled. Tool visibility independently intersects API-key and
tenant scopes before applying the existing exact-admin user-policy bypass.
Registry capabilities and model-admin aliases preserve their owning role maps.
Loaded model access checks share the same current object helper.

Knowledge owns Dataset rows and grants. Its explicit
`KNOWLEDGE_CEDAR_DATASET_CONFIGURATION` contains `origin`, `mode` (`shadow` or
`enforce`) and expected resource pins. Its signer is the existing
knowledge-service-to-gateway HMAC v2 authority; transport reuses admitted workload
TLS (or the explicit local legacy transport). The private Gateway endpoint
The private workload listener admits only POST
`/internal/v1/authorization/dataset` and POST
`/internal/v1/authorization/dataset/batch`. Configure
`AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS` with exact comma-separated Knowledge
SPIFFE IDs, each also in `AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS`. No ID is
inferred from its name or trusted prefix. If absent, TLS peers are denied on
these two routes; all previously admitted routes keep their existing peer rules.
The explicit dual-mode legacy transport remains subject to the original signed
Knowledge-to-Gateway business proof. Required mode accepts only authenticated
Knowledge peers and still requires that proof.

`/internal/v1/authorization/dataset[/batch]` verifies the exact method/path/body
proof and replay protection before parsing facts, and refreshes account/role/
permission authority before evaluating. A batch is bounded to 200 resources and
1 MiB. Receipts bind resource order/ID, legacy rank, non-widening allowed rank,
rollout mode and both pins. Enforced outages or incompatible receipts deny; a
server's enforced receipt cannot silently downgrade to a shadow client fallback.

The ingestion and query ACLs deliberately retain their different invalid-grant
fallbacks. Public reads, explicit cross-tenant user grants, owner exceptions,
archive catalog behavior, role scope and document/source ACLs remain with their
current authorities. Both primary stores, Confluence, fresh request stores and
vector workers carry the same explicitly configured policy. The application
scope contains a policy handle, never a caller identity. Dataset enforcement is a
coordinated rollout: configure **every** Knowledge API/worker replica and the
Gateway resource pins together; a Gateway-only flip cannot cover a Knowledge
replica that is still running its legacy implementation. The private workload
path must be admitted exclusively to the Knowledge service identity.

Install both the append-only capability log and
[resource decision-log proposal](proposals/gateway-cedar-resource-audit.sql)
through a new native-authority successor before either enabled mode starts. Do
not modify frozen 029. Application authority is INSERT-only. Resource audit rows
record type/action/flip, hashed principal/tenant/object/facts, correlation IDs,
policy/matrix pins and both outcomes. Raw ACLs, identity values and credentials
are not logged. Existing business audit writes are retained.

The lifecycle owns the bounded idempotent queue/writer and drains it before the
database closes. Audit health failure or queue rejection denies enforced object
operations. Shadow retains the legacy result on audit failure. Queued audit is
asynchronous: a process crash can lose queued rows, and an operation admitted
before an outage is detected may precede durable logging. Crash-durable
pre-admission logging is not claimed.

## Eval ownership and isolation

`platform-gateway-eval` owns evaluation HTTP/domain/outbox/judge execution and trace
persistence. It has no dependency on Gateway. Gateway exports compatibility
facades and supplies narrow ports backed by its actual current Knowledge and Agent
authorities. Source-version checks, current credential checks, runtime interrupts,
worker leases and supervised database effect fences are retained.

`--role eval` admits only the leased eval outbox worker plus shared per-process
recording/audit/health services. `control_plane.evaluation` defaults to
`{"placement":"shared","concurrency":2}` for compatibility. Select `dedicated`
on all replicas and run a separate eval process to prevent general workers or an
`all` process from winning the judge lease. API roles never start the judge.
Worker/eval listeners expose health and authenticated metrics only. The API p99
A/B load measurement remains required before U-GW-23 acceptance.

## Platform MCP

An explicit `control_plane.mcp` mounts `/mcp` and
`/.well-known/oauth-protected-resource/mcp`. The resource must be the deployed
HTTPS `/mcp` URL and match the configured JWT audience; configured authorization
servers must include the verified JWT issuer. Admission verifies signature,
expiry, current credentials/session and account authority before the adapter
narrows the audience. API keys cannot substitute for an OAuth bearer here.
Browser Origins require an explicit allowlist.

The adapter implements `server/discover`, `tools/list` and `tools/call` on revision
`2026-07-28`, checks per-request capabilities/version and matching HTTP metadata,
and retains no MCP session. No optional streaming/push/subscription capability is
advertised. Tool schemas and result shapes are checked against the pinned official
schema fixture. Knowledge retrieval, Agent lists/publication metadata and model
catalog reads dispatch the existing REST handlers, carrying only the verified
bearer and correlation headers. Their object policy, admission and accounting
remain the REST owners' implementations. The published-Agent send tool is exposed
only when immutable A2A publication bindings are configured; it uses that same
durable effect boundary. Configured external A2A peers are exposed as the
`external_agent_send` MCP tool; it delegates the same outbound REST boundary.
Restoring a lost response must reuse the message ID.

## A2A binding

U-RT-03 and U-GW-14 remain rollout prerequisites. An explicit A2A config additionally
pins the actual runtime revision, `capability-contract/v1`, an Ed25519 PEM signing
key and exact immutable publication/version pairs. Its schema and application
privileges must exist before graph construction. Configure explicit outbound
peers only; names cannot select arbitrary URLs.

The HTTP+JSON v1.0 binding serves authenticated signed Agent Cards, SendMessage,
GetTask, ListTasks and CancelTask below `/a2a/{publication}`; public verification
keys are at `/a2a/.well-known/jwks.json`. Cards use RFC 8785 JCS and detached Ed25519
JWS over immutable identity/version metadata. A publication rollover requires an
explicit binding update. Clients must establish trust in the configured issuer/key;
a JWK embedded in a signature is not, by itself, trust in an arbitrary card.

Text messages map to publication-pinned managed turns, not another model/tool
loop. Current viewer access, AgentInvoke capability, shared rate limits,
publication governance, model admission and current source ACLs run before
execution/output projection. Context/task lookups always include verified tenant,
user and publication. Message reservation and a dispatch marker commit before the
managed effect. Lost responses and ambiguous failures retain an unknown effect;
retries do not launch a new turn. An interrupt acknowledgement is not reported as
confirmed cancellation. Optional files, streaming, push notifications, arbitrary
extensions and task-ID continuation are rejected; none are advertised.

Outbound calls require a configured HTTPS `/message:send` endpoint, public IP
pin, peer-owned bearer credential, `agent_card_key_id` and a raw 32-byte
base64url `agent_card_public_key`. Before a new send, Gateway fetches the same
origin/base's `.well-known/agent-card.json`, verifies detached Ed25519 JWS with
that explicit key (never a header-supplied key), and checks the exact HTTP+JSON
1.0 interface and text profile. The signed card must advertise task-authority and
by-message reconciliation extensions; its name is the configured peer identifier.
Card canonical SHA/key identity are fixed in the durable effect reservation.
Later polls cannot silently retarget a changed card or remote task ID.

The registered tool identifier is `external_agent_<name>`, category
`external_agents`. Tenant name allowlists, denials and categories apply even to
administrators before any framework effect. The daily per-tenant call debit and
unknown marker commit before the only SendMessage. HTTPS/IP pinning, no redirects,
no proxy, no retries, bounded bytes/deadlines and current capability/rate admission
remain mandatory. No ambiguous failure refunds the debit or resubmits a message.

Every send/get/cancel/reconcile request carries `x-agentforge-a2a-task`, a compact
Ed25519 JWS using the existing Gateway signing key. Its fixed contract is
`agentforge-a2a-task/v1`: issuer and peer origins, verified tenant/user, peer name,
namespaced message UUID, exact method/path/raw-body SHA-256, issue/expiry (at most
120 seconds) and a fresh UUID nonce. POST bodies use the shared RFC 8785 encoder;
GET signs the empty body. Query strings are rejected. The peer bearer is never
used as a caller identity, and incoming user credentials/identity headers are
never forwarded. The external reference adapter verifies this proof and binds
its durable task and nonce records to the same tenant/user/message owner.

Gateway exposes GET `/a2a/outbound/{name}/tasks/{original_message_id}`, and POST
that path with `:cancel` or `:reconcile`. It maps the local owner/message record to
the remote task ID; callers cannot provide an arbitrary remote task ID. Unknown
sends reconcile only through signed GET
`{peer_base}/tasks:byMessage/{namespaced_message_id}`. A 404, timeout or invalid
receipt leaves the effect unknown. Cancellation has its own durable unknown
marker before the one cancel POST; a lost acknowledgement requires GET polling,
not a repeated cancel. Row locking retains terminal task receipts against stale
concurrent polls. A confirmed interrupt is reported only after a canceled task
state is observed. MCP exposes the same four send/task/cancel/reconcile REST
operations, with configured peer names published in their input schemas.

Install the [task/effect proposal](proposals/gateway-a2a-task-effects.sql), then
[the outbound authority successor](proposals/gateway-a2a-outbound-authority.sql)
through native authority. Opt-in graph admission checks all table privileges and
new columns. Older rows without authority bindings fail closed for the new task
operations; startup never fills them by replaying a framework effect. The
checked-in `a2a-task-authority-v1.json` is a test-only independently signed Node
fixture consumed by both native and external-adapter unit tests.

## Problem Details and verification

Gateway-owned bounded JSON errors gain type/title/status/instance/trace metadata.
String `detail` uses `application/problem+json`; structured legacy `detail` remains
lossless `application/json`. Provider wire, internal APIs, OAuth/JSON-RPC/A2A,
streams and successes retain their own contracts. The live and offline OpenAPI
producer share declarations; Rust/CLI clients expose typed problem errors while
retaining HTTP status and legacy fields.

Rust unit suites cover the real Cedar engine/matrix and audit failure, strict
configuration and file secrets, protocol shapes, signed-card tampering, managed
state mapping, public-IP negatives, REST dispatch/denial parity, no effect on
invalid tool arguments, exact OAuth audience/signature negatives, and role
selection. Existing eval/trace tests move with the independent crate.
Compilation/unit tests, real PostgreSQL multi-replica effect races, peer A2A E2E,
MCP reference-client conformance, file-secret reference deployment and eval API p99
are separate evidence layers; the owning upgrade records must report each.
