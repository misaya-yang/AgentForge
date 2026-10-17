# Knowledge model plane (U-KN-10 / F-33)

Knowledge models now use the Gateway private model plane. The Knowledge process
has no model-provider credential settings or tenant-provider decryption path.
Embeddings (including multimodal/local semantic), rerank (including self-hosted
pair scores), OCR/VLM, summary, QA generation/streaming and QA/RAGAS judges use
`migration_knowledge_query::model_plane`. Model payloads and output parsers retain
their previous provider codec; endpoints and keys are resolved only by Gateway.
The explicit legacy `local/hash-384` algorithm remains a deterministic feature
hashing identity; it is never substituted for semantic model failure.

## Configuration and trust

Configure these operator-owned settings:

- `KNOWLEDGE_MODEL_PLANE__URL`: private Gateway origin (defaults to `GATEWAY_URL`).
- `KNOWLEDGE_MODEL_PLANE__WORKLOAD_KEY_FILE`: absolute, regular, non-symlink JSON
  file with owner-only Unix permissions. Fields: `key_id`, `seed_hex`,
  `public_keys` (existing `gateway-verification-keys/v2` keyring wire). This seed
  belongs only to the Knowledge workload, never Gateway's lease signer.
- `KNOWLEDGE_MODEL_PLANE__GATEWAY_PUBLIC_KEYS_FILE`: absolute regular public
  keyring JSON; contains Gateway verification keys only, no seed/private key.
- `KNOWLEDGE_MODEL_PLANE__MODEL_REVISIONS`: JSON mapping `provider/model` to the
  actual registered immutable model revision. Explicit dataset revisions take
  precedence; aliases or missing revisions cannot stand in for a pinned model.

The workload proof uses existing lease-v3 Ed25519 envelope wire with purpose
`knowledge-model-workload`, TTL 45 seconds, nonce, exact POST path/body hash and
current tenant/user. Incoming model leases require purpose
`knowledge-model-lease`, a valid admitted Gateway public key, matching envelope
expiry, and exact schema/lease/call/hash/tenant/user/model/revision claims.
`ModelPlaneClient::new` is the explicit legacy signed-v2 transport adapter; normal
process construction always uses `for_workload` with independent workload signing.
The public-key lease verifier is mandatory in both constructors.

Former provider-key and direct model-endpoint configuration is rejected by the
explicit process parser, and excluded from ambient environment collection. Do
not mount old provider variables into the Knowledge container. Register cloud and
self-hosted model endpoints/credentials in Gateway. Connector encryption uses the
separate `KNOWLEDGE_CONNECTOR_ENCRYPTION_KEY`; it is not an embedding/provider key.

## Authorization and lifecycle

JWT/signed-user verification establishes the actor task-local scope only after
successful verification. Child embedding/QA/OCR/VLM/summary/parser tasks explicitly
inherit it. The scope restores on completion/cancellation. Worker operations
refresh the persisted initiator through `RuntimeActorClient`; no anonymous or
fixed-admin fallback exists. Backfill action jobs also refresh the initiator and
current owner authorization.

`POST /internal/v2/knowledge/model-operation-scope` is internal signed Gateway-only
RPC, not a public permission endpoint. Knowledge refreshes current actor, checks
its own dataset ACL, document/execution ownership and publication generation under
the existing shared lease. Gateway consumes this authority over signed transport;
it receives no Knowledge schema privileges. Reserve pins the returned generation;
execution checks it before dispatch and again before delivering output. A changed
generation cannot be published as successful model output, while actual provider
spend still has a receipt.

RAGAS carries an `actor_scope` in the signed request body; missing scope fails
rather than creating an anonymous billing identity. The Gateway live/durable eval
callers provide the actual admitted user or current fenced run initiator. Unscoped
internal metric/parser tests are not paid-call authorization.

## Lease, receipts and failure behavior

`knowledge_model.rs` owns the typed reserve/execute/SSE DTOs and exact typed-JSON
SHA256. No key or endpoint is permitted in model-call payload control fields.
Gateway owns admission, credentials, reservation/dispatch CAS, pricebook and
settlement. Knowledge validates the resulting receipt's actor, call, lease, model,
pricebook and settled state. Unknown dispatch, dropped/truncated streams, missing
settlement or mismatched receipts fail closed. There is no direct provider
fallback or blind transport retry. Existing known provider rejections remain
visible with their status. Streaming QA requires both semantic provider `stop`
and the Gateway terminal settled receipt before `done`.

Request/pricebook authority proposals are owned by Gateway and must be activated
through the native authority before use. Source preparation and protocol fixtures
are not paid-provider, billing, quality or service acceptance.

## Verification

This source writer ran Rust syntax parsing and scoped whitespace/source audits;
no Cargo, Docker, SQL application or provider calls. Primary runs the pinned
single-job native checks and all Knowledge/Gateway targets. Added regressions
cover actor task isolation, body/hash identity, Ed25519 purpose/signature/binding,
receipt mismatches/unknown state, signed-only scope authority and generation
replacement. Existing embedding/rerank protocol assertions remain, routed through
an explicitly synthetic loopback Gateway fixture; fixture receipts and constant
vectors are never quality or accounting evidence.

Native rerank implementations remain as explicit library/test implementations;
the process graph mounts only Gateway-backed rerank. Old operator tools that
constructed credential-bearing compatibility clients must establish the same
`run_scoped` model-plane context before real measurements; unscoped calls fail
rather than using hidden credentials or reporting fabricated quality.
