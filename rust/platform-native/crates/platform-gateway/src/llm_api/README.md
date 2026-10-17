# Public native LLM APIs — U-GW-05

This module owns `POST /v1/chat/completions`, `POST /v1/embeddings`,
`POST /v1/messages`, `GET /v1/models` and `GET /v1/models/{model}`. Existing
`/v1/responses` remains on its Runtime-owned path. Constructors do not start
listeners, migrate databases, issue credentials, or register new service policies.

`LlmApi::new` takes the host's existing `Arc<AdmissionService>`, data `PgPool`,
`Arc<DispatchPreflight>`, `Arc<dyn DispatchAdmission>`, `Arc<CapacityResolver>`,
`Arc<dyn RuntimeProviderCredentials>` and `LlmBindings`. The binding fields are
`chat_service`, `embeddings_service`, `messages_service`, and `api_key_header`.
Each service ID must already exist in the canonical registry. Bindings are
operator policy references; public callers cannot choose a service ID. Reuse the
host's existing AtomicDispatchAdmission instance. A capacity-only lease without
its real quota proof fails before provider I/O.

Minimal integration by the shared composition owner:

```rust
// lib.rs
pub mod llm_api;

// Preserve clones of the already-constructed CoreServices collaborators:
// preflight: Arc<DispatchPreflight>
// dispatch_admission: Arc<dyn DispatchAdmission>
// capacity: Arc<CapacityResolver> (already retained)
let llm = crate::llm_api::LlmApi::new(
    auth.identities.admission.clone(),
    pools.data.clone(),
    core.preflight.clone(),
    core.dispatch_admission.clone(),
    core.capacity.clone(),
    models.providers.clone(),
    configured_llm_bindings,
)?;
router = router.merge(crate::llm_api::router(llm));
```

Preserve the preflight and dispatch admission clones before moving the originals
into `GatewayDispatcher` / `DispatchExecutor`. Mount inside the existing host
request governance, trusted identity bridge and HTTP rate middleware. Do not
construct a second admission controller. The core's degraded capacity-only
fallback cannot enable these routes. Missing registered service, publication,
protocol profile or credentials fails closed. No service registration is silently
synthesized. The primary owner chooses existing service IDs under current policy.

Model IDs returned by Models are `provider/model`. An unqualified model ID is
accepted only when exactly one tenant registry row matches. Disabled rows still
participate in ambiguity. Tenant/user authority comes from current account or
bound database-key verification. Key service/model restrictions intersect tenant
and current user service policy before existing RBAC preflight. A public payload's
identity fields are opaque provider data and cannot select an account or tenant.
Models exposes enabled, visible models whose provider is enabled.

OpenAI SDK bearer keys and Anthropic SDK `x-api-key` are normalized into the same
configured database-key header. Compact JWT-shaped bearer values stay on the JWT
verification path; invalid JWTs never fall back to another key. Duplicate or
conflicting credentials are rejected. Browser cookies and client identity headers
are never forwarded to a provider.

The provider's registered `api_type` must match (`openai` / `openai-compatible`,
or `anthropic`) and the existing capability profile must advertise
`chat_completions`, `embeddings` or `anthropic_messages`. Old `responses_v1` stays
valid. There is no lossy protocol translation or name-based provider guessing.
Declared unsupported tools, strict tool schemas, native search, parallel calls,
streaming and input modalities fail before dispatch.

A matched request keeps its exact bytes, including unknown fields, number
spellings and whitespace. Top-level duplicate keys are rejected. The only body
mutations are explicit model alias resolution, a missing native output cap from
the registered model ceiling, and Chat `stream_options.include_usage=true`.
Patches replace only those top-level JSON values; existing equal values produce
no mutation. Existing caps may not exceed the registered ceiling or disagree.
Candidate counts are included in the existing admission output estimate. Provider
credentials come only from the current registry credential adapter. Only protocol
version/beta and idempotency headers are copied; client auth/cookie/identity headers
are excluded. Response and SSE bytes remain unchanged, with response hop headers
and cookies excluded. HTTP redirects, retries, proxies and auto-decompression are
disabled.

Before the sole provider POST, the existing atomic lease acknowledges its durable
`mark_dispatched` transition and owns the full deadline, backpressure and stream
lifetime. Request cancellation and body drop cancel the owned operation and mark
Unknown. The same PG hold remains; capacity cleanup never refunds unknown tokens.
Non-2xx without actual usage is also Unknown, not an invented zero-cost result.
A 2xx response without terminal observed usage produces a sanitized 502.

OpenAI stream completion needs every choice's finish reason, actual final usage,
and `[DONE]`. Anthropic needs `message_start`, final cumulative delta usage and
`message_stop`; cumulative values replace prior counts. Anthropic cached input is
added to total ledger input once, then the existing cache partition prices plain,
cached-read and cache-write components. Interrupted streams have a body error and
no success terminator. A native terminator is withheld until frozen-price raw usage
and the canonical quota/daily/hourly stages have completed. Accounting failure
retains the hold and breaks the stream. This uses `PgUsagePersistence` wrapped by
`CheckpointSettlingPersistence`; it does not treat an in-memory enqueue as a
settlement checkpoint or add a financial authority.

Protocol references read from official SDK main branches on 2026-10-17:

- [OpenAI Chat chunks](https://github.com/openai/openai-python/blob/main/src/openai/types/chat/chat_completion_chunk.py): optional final usage, empty choices usage chunk.
- [OpenAI streaming](https://github.com/openai/openai-python/blob/main/src/openai/_streaming.py): native `[DONE]` termination.
- [OpenAI embeddings](https://github.com/openai/openai-python/blob/main/src/openai/types/create_embedding_response.py): prompt and total tokens, base64-compatible payload.
- [Anthropic Messages](https://github.com/anthropics/anthropic-sdk-python/blob/main/src/anthropic/types/message.py): separate plain/cache input counts.
- [Anthropic stream accumulation](https://github.com/anthropics/anthropic-sdk-python/blob/main/src/anthropic/lib/streaming/_messages.py): cumulative delta replacement and message stop.
- [Anthropic SDK version](https://github.com/anthropics/anthropic-sdk-python/blob/main/src/anthropic/_constants.py): `2023-06-01`; caller protocol-version/beta headers pass through.

The receipt records retrieved source hashes. These references informed the
implementation; no official SDK conformance run is claimed. Handler tests invoke
the actual Axum router with deterministic transport/accounting collaborators;
codec tests exercise production raw parsing, capability/tenant/model boundaries
and native usage/terminal interpretation. Compilation, PostgreSQL/Valkey/provider
integration and official SDK acceptance belong to the primary's serialized gates.
