# 02 Gateway (gateway-control)

status: proposed · domain_id: platform-upgrade-2026-10 · last_verified: 2026-10-16 (HEAD `22d9ce0`)

## 1. Role

The Gateway is the public API and the control authority for identity, tenant policy,
provider/model admission, quotas and accounting, immutable launch and lease signing, and the
Gateway-owned brokers (MCP, connectors, images, artifacts, Local Node). It must not own model or
tool iteration, compaction, cancellation state, approval transitions or event ordering (platform
law L5).

## 2. Current architecture (SRC)

**Process.** `gateway --config-env` ([bin/gateway.rs](../../../rust/platform-native/crates/platform-gateway/src/bin/gateway.rs))
resolves a typed `GatewayDocument`. [`GatewayGraph::new`](../../../rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/graph.rs)
then constructs every service from explicit inputs (data and control pools, Redis, signed
Knowledge authorities), and the host serves an Axum router plus lifecycle-owned workers (eval
outbox, billing scheduler, image worker, file expiry, retention, optional usage-stream consumer).
Construction order follows the deleted Python `src/container.py`
([bootstrap README](../../../rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/README.md)).

**Crates.**

| Crate | Size | Contents |
| --- | --- | --- |
| `platform-gateway` | ~97k lines | `migration_agent` (agents, Studio, publications, model plane, approvals, schedules; 15k), `migration_compat` (Responses, transparent proxy, billing stream, MCP invocation, LangGraph, Confluence, **eval 10k**), `migration_content` (files, images, skills, artifacts, shares), `migration_host` (bootstrap, middleware, identity, metrics, Local Node channel, knowledge proxy; 25k), `migration_trace`, quiz |
| `platform-gateway-foundation` | ~98k lines, ~136 flat modules | Users/roles/API keys, tenant policies, quotas, rate limits, capacity admission, load shedding, dispatch, provider/model catalog and routing, usage/billing, MCP registry/broker, connectors, Local Node stores, tasks, sessions, assistant memory/plugins, quiz grading, Python-compat helpers |
| `platform-gateway-integrations` | ~52k lines | Provider adapters (LangGraph, Dify, SDK), **Knowledge ingest/query domain**, Local Node transport, content storage, ChatGPT-plan OAuth |
| `platform-auth` | ~2.5k lines | HS256 JWT + Redis allowlist sessions, credential revision, bcrypt, API keys, RBAC, admission |

**Request path.** Middleware order: request governance (concurrency permits, header and body-idle
deadlines) → body limits → request context → security headers → graceful drain → identity →
tracing. A single HTTP rate authority charges multiple dimensions through a Redis sorted-set
sliding-window Lua script ([rate.rs](../../../rust/platform-native/crates/platform-gateway/src/migration_host/rate.rs),
[rate_limits.rs](../../../rust/platform-native/crates/platform-gateway-foundation/src/rate_limits.rs)).
Capacity admission uses Redis leases with renewal and a local bounded queue
([capacity_admission.rs](../../../rust/platform-native/crates/platform-gateway-foundation/src/capacity_admission.rs)).
A latency-window load shedder sits alongside.

**Model plane.** The Runtime calls a private, lease-authorized single-provider plane
([migration_agent/model.rs](../../../rust/platform-native/crates/platform-gateway/src/migration_agent/model.rs)).
Leases are HMAC-SHA256 over a canonical JSON form. Wire protocols: `responses_v1` and
`chat_completions`. Provider selection and failover assemble untyped `Value` records
([provider_routing.rs](../../../rust/platform-native/crates/platform-gateway-foundation/src/provider_routing.rs),
[model_failover.rs](../../../rust/platform-native/crates/platform-gateway-foundation/src/model_failover.rs)).
The dispatcher retries only safe methods and classifies uncertain unsafe failures as
`UpstreamOutcomeUnknown` ([dispatcher.rs:140–195](../../../rust/platform-native/crates/platform-gateway-foundation/src/dispatcher.rs)).

**Accounting.** `UsageRecorder` buffers in process and batch-writes PostgreSQL with frozen
pricing receipts. Price lookup falls back to the global `model_pricing` table, keyed by model
name only (F-03).

**Public LLM surface.** `/v1/responses` plus registered transparent-proxy services under
`/api/v1/proxy`, `/api/v1/invoke` and `/api/v1/stream` (F-29).

## 3. Assessment

Strengths: explicit construction with no hidden I/O; fail-closed configuration; strong separation
of admission, model plane and execution; body-lifetime permits; disciplined retry semantics;
tenant-scoped API keys; credential revision invalidating sessions; a single rate authority (the
dual limiter of the Python era is gone).

Gaps, in order of consequence:

1. **Trust** (F-04, F-07, F-08). Symmetric secrets shared with verifiers, a static internal
   bearer, HS256 user tokens kept in browser storage, no SSO, SCIM or MFA.
2. **Money** (F-03, F-33). Prices are keyed by model name only; Knowledge-side model spend
   bypasses Gateway accounting; usage buffers are process-local until flushed.
3. **Type safety and Python residue** (F-16). `Value` records, `python_json`, Python float and
   integer semantics, a wall-clock breaker behind a Tokio mutex, hand-written
   `Pin<Box<dyn Future>>` aliases, `std::sync::Mutex` poisoning ignored
   (`unwrap_or_else(|e| e.into_inner())`), JSON error objects returned as success values.
4. **Capability** (F-29, F-30, F-32). Narrow inbound LLM API, no guardrail stage, a hard-pinned
   MCP revision, no token-aware admission, no prompt-cache passthrough or cached-token
   accounting.
5. **Structure** (F-15, F-17). Migration-batch crate boundaries; Knowledge and quiz domains in
   Gateway crates; eval (10k lines plus LLM-judge workloads) shares the API process.
6. **Scale-out** (declared). Single instance; some process-local state (quota RPM, context-metrics
   collector, provenance grants, in-memory task queue).

## 4. Target architecture

### 4.1 Crate topology and process roles (P-ADR-4)

Extraction happens only after Ph1 correctness work. Each move is measured by compile scope and
change coupling, keeps public wire hashes, and renames a module only when it moves into its
domain crate (no stand-alone rename sweeps; vNext §0.5).

| Target crate (package `ai-platform-*`) | Takes over | Depends on |
| --- | --- | --- |
| `contracts` (pure, no I/O) | Signed envelopes, canonical JSON v1/v2, event and lease schemas, problem types | serde only |
| `identity` | `platform-auth`, users/roles/API keys/sessions, SSO/SCIM/passkeys | contracts |
| `policy` | Tenant/tool/service-access policies, Cedar PDP, decision log | identity, contracts |
| `catalog` | Providers, models, capability profiles, price books | contracts |
| `routing` | Route policies, failover, provider health, circuit breaking | catalog |
| `admission` | Rate (GCRA), capacity leases, token reservations, quotas, load shedding | contracts |
| `accounting` | Usage intake, receipts, billing, budgets | catalog |
| `llm-api` | Inbound OpenAI/Anthropic codecs, Responses, transparent proxy, stream metering | routing, admission, accounting |
| `agent-control` | Agents/Studio/publications, launch resolution, model-plane leases, approvals projection, schedules | policy, routing, accounting |
| `brokers` | MCP, A2A, connectors, Local Node channel, images, artifacts | policy, contracts |
| `eval` | Eval, trace capture and scoring (with today's `platform-eval`) | contracts |
| `gateway-http` / `gateway-app` | Axum routers, middleware, OpenAPI producer; composition root, typed config, process roles | all |

Process roles (no new bounded context):

- `gateway --role api`: stateless HTTP and SSE; scales horizontally once §4.6 holds.
- `gateway --role worker`: eval outbox and judge runs, billing scheduler, scheduled agent tasks,
  batch jobs, image worker, retention, usage consumer. Each singleton job holds a PostgreSQL
  lease with a fence token.
- `gateway --role all`: the Compose default; identical behaviour to today.

### 4.2 LLM data plane

- **Canonical invocation model.** A typed `ModelInvocation` carries tenant, principal, route
  policy reference, a `ModelRef` (provider, model, version), typed input items, tools, sampling
  parameters, budget, cache hints, idempotency key and trace context. It replaces the
  `configurable.gateway_model` JSON shape.
- **Inbound codecs (P-ADR-12).** OpenAI Chat Completions, Responses, Embeddings and Models, plus
  Anthropic Messages, with streaming. Optional Gemini `generateContent`. Every surface uses the
  same admission, routing, guardrail and accounting path.
- **Passthrough first.** When the inbound and provider protocols match, forward the body with only
  authorized mutations (credentials, enforced parameters, metadata) and parse the stream only for
  usage and terminal detection. New provider features (reasoning items, cache markers, tool
  types) then work without a code change. Translate only on a protocol mismatch, and fail closed
  when the target model's capability profile (ADR-005) cannot represent the request.
- **Outbound adapters.** OpenAI Responses/Chat, Anthropic Messages, Gemini (AI Studio and
  Vertex), DashScope, Ark, DeepSeek, and OpenAI-compatible self-hosted servers (vLLM, SGLang,
  TEI) for chat, embeddings and rerank.
- **Prompt caching.** Pass provider cache controls through (Anthropic `cache_control`, OpenAI
  prompt-cache keys, Gemini cached content). Record cached-read and cache-write tokens as separate
  receipt components, and report hit ratios per tenant and model. This is the largest single cost
  lever for agent workloads with long, stable prefixes.
- **Batch.** Provider batch APIs run as durable Gateway tasks (U-GW-11) with their own price-book
  components.
- **Knowledge model calls** (F-33). Embeddings, rerank, OCR/VLM and judge calls from Knowledge go
  through the private model plane with service-scoped leases, so credentials, quotas and receipts
  have one owner.

### 4.3 Identity, authorization and service trust (P-ADR-2, P-ADR-9)

| Concern | Target |
| --- | --- |
| Console sessions | Backend-for-frontend: an opaque `__Host-` session cookie (HttpOnly, Secure, SameSite=Lax) backed by today's server-side session store. CSRF defence through Fetch Metadata (`Sec-Fetch-Site`) and Origin checks plus a synchronizer token on state-changing requests. WebSocket upgrades check cookie and Origin. |
| API clients | Hashed, scoped API keys (existing). Short-lived EdDSA access tokens with a published key set for SDK and CLI. Device authorization grant (RFC 8628) for the CLI's explicit `gateway` login. |
| Enterprise SSO | Native OIDC (Authorization Code + PKCE) per tenant with the `openidconnect` crate. SAML 2.0 through an external IdP broker or a security-reviewed Rust implementation. JIT provisioning bounded by tenant domain policy. |
| Provisioning | SCIM 2.0 `/scim/v2/Users` and `/Groups`, mapped onto tenant roles. |
| Authentication strength | Passkeys/WebAuthn (`webauthn-rs`) for local accounts and admin step-up. Argon2id password hashing, transparently rehashing bcrypt hashes on login. |
| Authorization | Cedar (`cedar-policy`, Rust-native, schema-validated, analyzable) as the decision point for RBAC/ABAC on tenants, users, datasets, agents, tools and models. Run in shadow mode against today's checks, then enforce resource type by resource type, with decision logs written to audit. |
| Service trust | The Gateway signs model leases, launch snapshots, Knowledge actor envelopes and a per-turn capability ceiling with Ed25519 keys that carry key IDs. Today the Runtime mints per-call capability leases with the shared HMAC secret (F-04). In the target it only *attenuates* the Gateway-signed ceiling per call (an offline-attenuable token such as Biscuit, narrowed to run, call, arguments hash and expiry), so it needs no signing key. Runtime, Worker and Knowledge hold public keys only. Rotation overlaps two keys. The Local Node v2 protocol already verifies pinned Ed25519 platform keys by key ID, so reuse that key format and rotation procedure. Internal hops use mTLS with workload identities (SPIFFE IDs) instead of the shared bearer. Attenuated delegation to subagents (ADR-004) uses offline-attenuable tokens such as Biscuit. |
| Secrets at rest | Provider and connector credentials use AEAD envelope encryption with versioned key-encryption keys (KMS, Vault Transit, or a file key for single-node). Fernet stays decrypt-only during migration. |

### 4.4 Admission, quotas and accounting (P-ADR-5)

- **One script, at most two round trips.** A single Valkey Lua call evaluates GCRA request-rate
  dimensions, acquires the capacity lease and reserves tokens. Keys carry `{tenant}` hash tags so
  they work on Valkey Cluster. A process-local `governor` bucket absorbs anonymous and IP floods
  before Valkey is touched.
- **Token-aware limits.** `reserve(tenant, model, estimated_input + max_output)` before dispatch;
  `settle(actual)` with refund or charge on completion; expiry reclaims unsettled reservations.
  Quota RPM moves from process memory to Valkey.
- **Price book v2.** Entries are keyed by provider, model, model version and component (input,
  cached input, cache write, output, reasoning, tool call, image, batch) with currency and an
  effective range, plus tenant overrides and discounts. Entries are immutable and receipts
  reference them. Existing `model_pricing` rows migrate with explicit provider attribution;
  ambiguous rows are quarantined, not guessed.
- **Durable usage intake.** Usage events go to a PostgreSQL outbox in the same transaction as the
  run's accounting stage (idempotency key: request ID + stage) and are folded into the ledger and
  aggregates asynchronously. Process termination then cannot lose billable usage. The Redis
  Streams publisher stays an optional fan-out, not the record.
- **Budgets.** Per tenant, project and user budgets with soft and hard limits, alerts (the
  `quota_alerts` table exists) and a cost explorer by tenant, model, route and agent.

### 4.5 Brokers and interoperability

- **MCP** (F-32). Negotiate a supported set {`2025-11-25`, `2026-07-28`}, adding older revisions
  only on demand. Implement the stateless request model (`server/discover`, per-request
  capabilities, no session pinning). Keep tool-snapshot pinning and schema-diff review. Follow the
  authorization spec (protected resource metadata, resource indicators, audience-bound tokens).
  Run stdio MCP servers in the sandbox pool, never in the Gateway process.
- **A2A v1.0** (Ph3, U-GW-15). Expose published agents as A2A servers with signed Agent Cards
  derived from immutable publications; map A2A tasks onto managed turns. Register external A2A
  agents as governed capabilities with allowlists, authentication, budgets and unknown-effect
  semantics. This is the "bring your own framework" path for OpenAI Agents SDK, Claude Agent SDK,
  LangGraph or ADK agents (see [03 §4.5](03-agent-execution.md)).
- **Platform as an MCP server** (U-GW-24). Today the Gateway is only an MCP *client* broker; no
  public path exposes platform functions over MCP. Expose knowledge retrieval, published agents
  and selected read-only administration as a stateless `2026-07-28` MCP server, acting as an
  OAuth resource server with audience-bound tokens. Every call passes the same policy,
  admission and accounting path as REST. External clients (IDEs, desktop assistants, other
  agents) can then use the platform without a custom SDK.
- **Connectors.** Envelope-encrypted OAuth tokens, refresh rotation, per-connector rate limits and
  health.

### 4.6 Reliability and scale-out

- **Process-local state inventory.** Every item is moved to Valkey or PostgreSQL, or declared
  instance-affine with a documented routing rule. Known items: quota RPM, context-metrics
  collector, provenance grants, the generic memory task queue, guest sessions, dashboard
  WebSocket sessions.
- **Singleton workers** hold PostgreSQL leases with fence tokens. Every write they make is
  conditional on the fence.
- **Circuit breaker v2.** Monotonic clock, atomic state, a half-open single probe, scoped per
  provider endpoint and model, with outlier ejection feeding routing fallbacks. Error-class
  routing: a 429 with `Retry-After` backs off or reroutes; failures before the first byte may
  fall over only for safe operations; after the first byte there is no retry.
- **Deadlines** propagate internally (an `x-request-deadline` header; gRPC deadlines for Knowledge)
  with the per-edge connect, header, idle and total budgets defined in Stage D.
- **Retiring the single-instance release assumption** ([architecture.md](../../harness/architecture.md))
  requires lease and fencing tests, kill-9 failover, zero double executions under chaos, and
  capacity measurements. This is vNext phase three work.

### 4.7 Performance engineering

Profile before optimizing (OTel profiles, `samply`, `pprof`). Expected wins, in order:

1. Fewer Valkey round trips through the combined admission script.
2. Typed structs and `Bytes` pass-through instead of `Value` cloning on the streaming path; the
   native line framer already exists.
3. `moka` caches for principals (keyed by token ID and credential revision) and policy decisions,
   with revision-based invalidation.
4. Async DNS (`hickory`), HTTP/2 pools per provider, and per-role database pool sizing.
5. An allocator trial (`mimalloc` or jemalloc), adopted only with measured RSS and tail-latency
   gains.

## 5. Technology upgrades

| Item | Current | Target | Rationale |
| --- | --- | --- | --- |
| SQL access | sqlx 0.8, runtime-checked strings | sqlx 0.9; compile-time-checked `query!` with offline metadata in CI for new code | Catches schema drift at build time |
| Tokens | jsonwebtoken 9.3, HS256 | jsonwebtoken 11 with an explicit provider; EdDSA/ES256; key IDs | Asymmetric verification, rotation |
| Passwords | bcrypt 0.17 | argon2 (Argon2id) | Current guidance; no 72-byte truncation |
| Encryption at rest | fernet 0.2.2 | AEAD envelope (`aes-gcm-siv` or `chacha20poly1305`) | Key IDs, rotation, KMS integration |
| TLS provider | rustls with `ring` pinned in places | One provider workspace-wide; aws-lc-rs where a FIPS profile is required | Consistency and compliance options |
| Telemetry | OpenTelemetry 0.31, trace only | 0.32 + `tracing-opentelemetry` + OTLP logs and metrics; GenAI semconv pinned | [08](08-platform-engineering.md) |
| YAML | `serde_yaml` 0.9 | Maintained fork | Archived crate |
| New crates | — | `cedar-policy`, `webauthn-rs`, `openidconnect`, `moka`, `governor`; `biscuit-auth` if delegated tokens are adopted | §4.3–4.4 |
| Async traits | Hand-written `Pin<Box<dyn Future>>` aliases | Native `async fn` in traits for static dispatch; boxing only at `dyn` boundaries | Readability; fewer allocations |
| Errors | `{"detail": …}` bodies, JSON error values in `Ok` | Typed `thiserror` errors at domain boundaries. HTTP errors become RFC 9457 Problem Details while keeping `detail`, so existing clients still parse them. | Backward-compatible enrichment |

## 6. Work packages

| ID | Work package | Prio | Phase | Acceptance |
| --- | --- | --- | --- | --- |
| U-GW-01 | Price book v2 and immutable receipts (F-03) | P1 | Ph1 | Composite key; migration with provider attribution and quarantine; typed `Result`; cross-provider/tenant collision tests |
| U-GW-02 | Ed25519 leases and envelopes; per-turn capability ceilings that the Runtime attenuates per call; key rotation; verify-only consumers (F-04) | P1 | Ph1 | No signing key in Runtime/Worker/Knowledge config; dual-verify window (HMAC v1 + v2); "verifier cannot mint" and "attenuation cannot widen" negative tests; rotation drill |
| U-GW-03 | BFF session cookies + CSRF, Argon2id, EdDSA API tokens (F-07, F-08) | P1 | Ph1–Ph2 | Console holds no bearer token; CSRF and Origin negative suite; bcrypt hashes rehash on login |
| U-GW-04 | OIDC SSO, SAML via broker, SCIM 2.0, passkeys and MFA | P1 | Ph2 | Entra ID/Okta/Keycloak conformance runs; SCIM create/update/deactivate propagates within one minute |
| U-GW-05 | Inbound LLM API surface with passthrough-first codecs; Knowledge model calls through the model plane (F-29, F-33) | P1 | Ph2 | Official-SDK conformance against the Gateway for each surface; byte-exact passthrough fixtures; usage receipts for every call |
| U-GW-06 | Routing policy v1: versioned, explainable, deterministic for eval | P1 | Ph2 | `x-route-decision` ID resolves to policy version and candidate trace; eval reruns pin the policy |
| U-GW-07 | Token-aware admission (reserve/settle), GCRA, ≤ 2 Valkey round trips, Valkey-backed quota RPM | P1 | Ph2 | Load test at target RPS with correct rejection shapes; reservation reclaim on crash |
| U-GW-08 | Prompt-cache passthrough and cached-token accounting | P1 | Ph2 | Cache components on receipts; per tenant/model hit-ratio metric |
| U-GW-09 | Guardrails pipeline v1 (F-30) | P2 | Ph2 | Per-tenant policy; injection, PII/secret and content-safety stages; decisions audited; added latency budget measured |
| U-GW-10 | Multi-revision MCP including `2026-07-28`; OAuth resource metadata; stdio servers sandboxed (F-32) | P1 | Ph2 | Conformance against reference servers on both revisions; downgrade and mismatch fixtures |
| U-GW-11 | Make the existing opt-in durable task path ([design](../../design/generic-task-durable-rollout.md)) the default after canonical schema acceptance; add operator reconciliation for `claimed_unknown`; provider Batch API on the same ledger (F-09) | P2 | Ph2 | Kill-9 before and after claim leaves recoverable pending work or an explicit `claimed_unknown`, never a silent loss or replay |
| U-GW-12 | Process roles and singleton worker leases | P2 | Ph2 | Two worker replicas never run the same singleton job (fence-violation counter stays 0) |
| U-GW-13 | Cedar decision point: shadow, then enforce; decision logs | P2 | Ph2–Ph3 | Differential test zero-diff on the authorization matrix before each enforcement flip |
| U-GW-14 | Gateway multi-instance | P2 | Ph3 | State inventory closed; failover and chaos suite; single-instance assumption retired by ADR |
| U-GW-15 | A2A v1.0 inbound and outbound | P3 | Ph3 | Signed Agent Cards; task-state mapping fixtures; governed external agent run end to end |
| U-GW-16 | Tenant-scoped semantic cache experiment (opt-in) | P3 | Ph3 | Paired eval shows no quality loss at the chosen threshold; cost saving measured |
| U-GW-17 | Typed catalog, routing and failover; canonical JSON v2 (F-16, P-ADR-3) | P2 | Ph2 | No `Value` in routing signatures; v1 hashes still verify; v2 hashes match the JCS test vectors |
| U-GW-18 | Quiz as a plugin-delivered capability (F-17) | P3 | Ph2–Ph3 | The `ai-quiz` plugin works with no quiz code in foundation or Worker |
| U-GW-19 | Typed, versioned configuration schema; secrets from files (F-27) | P2 | Ph1–Ph2 | Published JSON Schema; `--check-config` covers every key; no long-lived secret in env vars in the reference deployment |
| U-GW-20 | Schema-epoch admission before readiness and workers (evolution F3, Stage B) | P1 | Ph1 | Wrong baseline, missing ledger and out-of-window epochs fail closed |
| U-GW-21 | Trace propagation and per-edge budgets (evolution F5/F6, Stage D) | P1 | Ph1 | Round-trip fixtures across Gateway → Runtime → Worker → Knowledge |
| U-GW-22 | Problem Details error envelope (compatible with `detail`) | P3 | Ph2 | OpenAPI documents the problem types; SDKs map them to typed errors |
| U-GW-23 | Eval subsystem in its own crate and worker role | P2 | Ph2 | Judge load no longer affects API p99 (measured A/B) |
| U-GW-24 | Platform MCP server (knowledge retrieval, published agents, read-only admin) | P2 | Ph2–Ph3 | MCP conformance on `2026-07-28`; OAuth audience tests; same policy decisions as the REST equivalents |

## 7. Verification gates

- Contract: OpenAPI and event diffs against `sdk/openapi.json` and shared fixtures (no unversioned
  breaking change); SSE framing fixtures; passthrough byte-equality fixtures.
- Security: cross-tenant negative matrix; CSRF and Origin tests; token forgery and replay tests;
  "verifier cannot mint" tests; Cedar differential tests; fuzzing of parsers and codecs
  (`cargo-fuzz` on SSE framing, canonical JSON, inbound codecs).
- Money: receipt determinism (same inputs give the same receipt); reservation and settlement
  conservation (Σ reservations = Σ settlements + Σ expiries); price-book migration audit report.
- Performance: the enterprise performance contract harness with a deterministic mock provider,
  reporting p50/p95/p99 overhead separately from provider time.
- Resilience: Valkey outage (per-policy fail-closed or degrade), provider 429/5xx/EOF matrix,
  kill-9 of worker-role singletons.
