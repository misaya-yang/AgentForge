# Knowledge model plane integration

Source candidate, not measured service acceptance. No provider keys or endpoints
enter the typed Knowledge API. No direct Knowledge database reads/grants exist.

## Gateway composition (primary owns bootstrap/process)

The `ai-platform-service-primitives` Gateway dependency is required (primary added).
No other Cargo dependency is introduced. Mount on the existing Gateway listener:

```rust
use crate::knowledge_model_plane::{self, KnowledgeModelPlane, LeaseAuthority,
    WorkloadAuthentication, WorkloadVerifier};
let signer = Arc::new(LeaseAuthority::from_private_keyring_file(gateway_ed25519_keyring_path)?);
let workload = Arc::new(WorkloadVerifier::new(pools.data.clone(),
    WorkloadAuthentication::Ed25519 { keys: knowledge_workload_public_keyring })?);
let plane = KnowledgeModelPlane::new(
    pools.data.clone(), signer, workload, models.providers.clone(),
    core.admission.clone(), core.capacity.clone(), active_price_book_version,
    graph.history_knowledge.clone(),
)?;
router = router.merge(knowledge_model_plane::router(plane));
```

`models.providers` is the existing `ModelServices.providers` concrete service;
its existing `RuntimeProviderCredentials` implementation resolves/decrypts the
registered tenant provider. `core.admission/capacity` are the existing shared
Redis-backed controller/resolver. `graph.history_knowledge` is the existing
`KnowledgeService` using the canonical signed client; its new child scope RPC
calls the KS-owned scope-check endpoint. No alternate provider registry, quota
service, identity source, session or agent turn is constructed.

Current actor authority uses **existing AuthService account authority** adapters:
`PgAccounts::by_id` plus `current_authority` checks active account+tenant and
current grants on reserve/execute/post-call. It intentionally does not authenticate
a workload as an end-user JWT. Workload attribution and actor authority must both
succeed. Body roles do not grant privileges. The active tenant model route,
access_level and registered model kind are checked independently.

Platform lease seed stays in Gateway's existing private Ed25519 keyring file.
The workload public keyring is a **different Knowledge-owned key authority**.
Knowledge signs only its own request identity, not platform leases. Public
keyrings use `gateway-verification-keys/v2`; leases use purpose
`knowledge-model-lease`; incoming requests use `knowledge-model-workload`.
The request claims bind caller_service=knowledge-service, audience=gateway,
method=POST, exact path, SHA256 of raw body bytes, tenant_id, user_id, unique nonce.
Workload TTL <=60 seconds. Nonces persist in PostgreSQL, not process memory.

The explicit `Transition` constructor additionally accepts the existing v2
`GatewayAuth` with caller/audience reversed to knowledge-service/gateway,
allowed_path_prefixes exactly `[RESERVE_PATH]`, nonanonymous, and an absolute
accept_until_ms no more than 24 hours ahead. HMAC uses x-gateway-signature.
Verify-only mode never admits HMAC and has no HMAC fallback. KS scope RPC still
uses the existing Gateway→KS signed-v2 channel as a compatibility boundary; its
final workload-identity replacement remains the shared auth workflow's work.

## Authority admission

`database/proposals/knowledge-model-plane.sql` is unnumbered and unactivated.
Native authority must measure this as a successor; never edit frozen 025/026.
It requires the already admitted price-book-v2 publication/activation/receipt
and canonical accounting checkpoint schemas. The role receives:

- SELECT on new calls; UPDATE only state/dispatched_at/provider_request_id/
  error_code/observation; INSERT on nonce table; EXECUTE on the two exact functions.
- Existing Gateway-owned account/RBAC/model/price-book/accounting privileges.
- **No** Knowledge table access, and **no** Knowledge role access to these tables,
  functions, provider credentials or Gateway signing keys.

The privileged owner maintains expired workload nonce rows and reviews unresolved
unknown spend. New reservations atomically release this actor's expired, provably
never-dispatched reservations; unknown spend is never automatically released.

## Atomic behavior

Reserve authenticates workload, refreshes actor and tenant model rights, checks KS
scope, freezes the existing `PgPriceBookService::freeze_usage` publication and
stores its generation. The quota-row lock serializes first reservation and the
same actor/call_id. Duplicate/conflict lookup precedes capacity/quota enforcement.
A new reservation charges conservative input-byte and declared output ceilings.
A duplicate returns its original lease, including with a full quota.

Execute rechecks scope/generation, prepares the fixed registered endpoint and
acquires existing provider capacity. A single conditional committed UPDATE from
reserved to dispatched fences all processes before the sole provider POST. The
owned task survives HTTP disconnect. Capacity loss/timeout/interruption leaves
unknown; there is no HTTP-library retry, redirect, failover or redispatch.
Provider request ID and complete observation are durable. Replays of observation
retry **only settlement**, including after lease expiry, under current identity
and resource authorization. No provider I/O is repeated.

Settlement opens one transaction: the proposal function releases the original
period's reservation, writes canonical raw usage and the final result; the new
foundation helper invokes the **existing** cumulative quota/daily/hourly
accounting stages/checkpoints; then one COMMIT publishes all four together. A
stage failure rolls back and leaves the precommitted observation for reconciliation.
A lost COMMIT response is reconciled by replaying the stored result, never by
charging or dispatching twice. Accounting request IDs hash tenant+actor+call_id,
so the same client key in different actors cannot collide in existing ledgers.

Streams forward original chat chunks as typed delta SSE. A completed semantic
result requires the final settled receipt. Generation changes produce rejected
409 with no final output while still settling actual observed spend. A vendor
response without actual usable usage remains unknown for a token-priced model.
A registered request-priced model instead confirms exactly one invocation from a
complete successful HTTP response; score-only outputs do not require invented
tokens. Failed/interrupted outcomes retain an unsettled unknown receipt.  Existing vendor
payloads remain unchanged. Registered provider metadata may pin same-origin
`knowledge_model_endpoints` by snake-case codec (e.g. rerank=`/api/v1/reranks`);
cross-origin overrides are rejected before credentials are attached.
Model identity/codec and explicit output-token limits
are checked against the reserved ceiling. Price terms and model revision must be
explicitly activated in the existing price-book routes; no free/legacy fallback.

## Verification (not executed by subagent)

Primary single-writer native toolchain:

```sh
cargo test -p ai-platform-gateway knowledge_model_plane
cargo test -p ai-platform-gateway knowledge_model_plane::postgres_tests -- --ignored
```

The ignored test requires `AGENTFORGE_KNOWLEDGE_MODEL_TEST_DSN` pointing to a
dedicated disposable database with the authority proposal already admitted.
It races real reservations/CAS claims, checks duplicate replay at full quota,
body conflict and unknown nonredispatch. Ordinary unit tests include real Axum
unsigned/foreign-workload/verify-only HTTP rejection, Ed signature purpose and
binding negatives, tenant/resource proof substitution, usage and stream failures.
No Cargo, Docker, migrations, private env reads or real-provider calls were run.

## Explicit request meter activation

Apply both independent unnumbered proposals, with `request-meter-v1.sql` before
accepting request model traffic. Frozen 026 is unchanged. Price books append
`Component::Request` (wire `request`) with `Unit::Requests` (`requests`) and
explicit USD rate; activation validates this term. Registered model metadata
must contain `billing_meter: "requests"` and the correct model_type (e.g. rerank).
A missing request term never falls back to token prices or free invocation.
For a credential-free local registered provider, opt in with api_type
local/selfhosted/bge and provider metadata
`allow_unauthenticated_local_model: true`; this creates typed NotRequired routing,
not a fabricated resolved credential, and sends no Authorization header.

Successful full HTTP responses record requests=1 before generation/output
postchecks; a later rejected output still settles known invocation cost. Network
interruption and unpriced failure outcomes remain unknown, never automatic retry.
Token-priced models still require real token usage. Stored observations retain
request quantity across settlement failures and process restarts.

Wire compatibility: UsageReceipt.metering is optional and omitted for old
receipts. New request receipts contain units={requests:1}, metering.kind=requests,
tokens=not_applicable, and exact frozen price-book cost_nanos. Unknown requests
have tokens=unknown and no cost_nanos. Existing token component names are unchanged.
The ledger also retains request_cost_nanos; its existing `*_cost_cents` fields
remain the legacy USD half-even projection at 1e-6 USD, never cents.

DB compatibility: only request rows bind NULL input/output tokens (generated
total_tokens is NULL); generated meter_kind/measured_units/token_count_state/
request_cost_nanos expose the typed fact. Token rows keep NOT-NULL semantics via
new conditional CHECK and retain existing defaults. Canonical cumulative
checkpoints retain measured units and exact nano cost. Quota/daily/hourly stages
continue their existing rounding and request-count semantics. The reservation
proposal uses billing's true 10000 legacy microcents/cent conversion. Its
quota-row-locked admission also checks exact monthly nano facts plus pending
ceilings, so repeated sub-cent requests cannot bypass the unchanged canonical
per-observation legacy cent rounding. Expired never-dispatched ceilings release;
unknown ceilings stay reserved until evidence-based reconciliation.

Existing summary/breakdown/timeseries SUM/COALESCE queries retain all costs and
requests and sum only applicable token facts. The existing summary additionally
reports optional metering when request rows exist: reported_token_usage_only,
request_priced_invocations, tokens_not_applicable_records, request_priced_cost_nanos
and the explicit legacy cost projection. The public usage projector preserves
this additive metadata; no-request fixtures have identical fields. Cache metrics
exclude N/A rows, so their nullable token counters cannot cause decoding failures
or become invented cache misses. The general raw writer validates the new typed
request contract and frozen costs before writing, and replay remains guarded by
the original provider/model/price-snapshot identity.

Additional primary verification commands:

```sh
cargo test -p ai-platform-gateway-foundation request_meter
cargo test -p ai-platform-gateway knowledge_model_plane::provider
cargo test -p ai-platform-gateway-foundation request_meter -- --ignored
```

The ignored request-meter PostgreSQL test uses the actual existing raw writer,
replays it, invokes canonical settlement stages twice, and reads both raw and
aggregate summary plus cache metrics. It rejects invented tokens and free-cost
substitution. It requires a dedicated disposable database with the successor
applied, and intentionally retains immutable receipt/checkpoint test evidence.
Unit/local HTTP tests cover score-only 2xx, 429, truncated real responses, exact
USD unit conversion, no double rounding, wrong scope/unit/state, and legacy
wire omission. These tests have not been executed by the subagent.

Public OpenAPI source hook (primary): include
`../../platform-gateway/src/knowledge_model_plane/public_contract.rs` from
platform-openapi/src/document.rs and call its `extend_document(&mut document)`.
It adds optional UsageMeteringSummary and preserves every existing aggregate
integer schema. Do not make these aggregate token fields nullable: raw nullable
request facts are not exposed by these old endpoints. Private ModelExecution
uses the typed metering variant. Current usage traces read request_traces, a
separate unchanged table; this plane does not write request-trace token fields.
SDK snapshot regeneration and optional Web UsageSummary.metering declaration
are source-release integration work, not a frozen baseline rewrite.
