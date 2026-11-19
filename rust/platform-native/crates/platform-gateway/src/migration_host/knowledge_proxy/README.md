# Gateway knowledge forwarding

Canonical sources: `src/api/v1/knowledge.py`, `src/api/v1/_proxy_utils.py`,
`src/services/knowledge_deletion_confirmation.py`, shared `proxy/base.py`, and
`AgentRepository.get_dataset_impact`.

Register this module in `migration_host` and merge `router(Arc<KnowledgeProxy>)`
into the Gateway application. Remove the older read-only `knowledge_http` mount
from that application's foundation to avoid overlapping routes. This router owns
both `/api/v1/knowledge/…` and `/v1/knowledge/…`; do not nest it again. The six
previous document reads, all writes, multipart upload and progress/SSE now follow
the same signed HTTP boundary. Never merge Knowledge Service business routes into
the Gateway.

`KnowledgeProxy::new` accepts explicit origin/body-limit configuration, a v2
Gateway-to-Knowledge signer, current-account content access and Gateway database.
Private-network plaintext HTTP requires explicit `allow_private_http`. No client
redirects or environment proxy discovery are allowed. Constructors make no
network calls or environment/secret reads. Supply `trace_capture` with the
existing `RagTraceCapture` configuration to persist completed bounded retrieval
observations through the existing trace repository.

Authentication uses audited current DB-key/JWT admission; key-declared read and
write scopes remain independent. Guest and configured static-key fallback use the
existing endpoint identity resolver. The native content middleware supplies
trusted IP and already-counted rate dimensions. Client authority headers and
credentials are removed; resolved identity and a fresh body-bound HMAC envelope
are generated for each transport attempt. KS remains the dataset/document ACL
and mutation authority.

Upload envelopes default to 16 MiB single/32 MiB batch plus 256 KiB multipart
overhead, with two upload slots and a 128 MiB two-copy memory reservation.
JSON defaults to 4 MiB. Limits are clamped to the canonical maxima. Responses are
streamed with backpressure rather than trusting advertised small content lengths;
SSE is never buffered. Fifty upstream slots survive until response completion or
cancellation. Connect failures alone receive one replay with a fresh signature;
post-send protocol failures and timeouts are not retried, avoiding duplicate
mutations. Breakers are bounded, route-scoped, use one half-open probe, and count
mid-stream errors. Retrieval observations are dropped on cancellation/truncation
or if larger than 256 KiB. Response encoding is preserved with an identity
encoding request because this client does not enable response decompression.

Gateway SQL is limited to account password confirmation and Agent-reference
impact. Delete confirmation rejects reserved client claims, verifies the current
account password using the bounded password engine, removes that password, then
signs a sixty-second dataset/user/tenant-bound claim. Impact requires a fresh
signed KS owner check before querying tenant-scoped Agent references, returning
names only when Agent ACL permits visibility.

The initial migration slice was source-only, with formatting but no build, test,
fixture, provider or running-service checks. Later bounded verification is
reported in the dated measured contract boundary below.

## Retained Python Knowledge profile

The same proxy supports an explicit native Gateway + Python Knowledge profile
for saved-binding Confluence manual synchronization. Keep one Knowledge owner for
Sources, sync admission, exact-task readback and ingestion. Configuration, reverse
current-actor signing, controlled-contract verification and release limits are
documented in [the hybrid profile](../../../../../../../docs/design/knowledge-native-python-hybrid.md).
The private request constructor is used directly by `upstream`;
`hybrid_contract_tests` inspects those requests without sending them.

## Source-owned OpenAPI boundary

`contract.rs` supplies the actual public prefixes, explicit methods, upstream
path, read/write classification and dataset-delete/impact matchers to this
runtime and the independent native OpenAPI producer. It declares a streaming
proxy with opaque success/error bodies, conditional native auth and special
transforms; it does not claim native ownership of Python Knowledge business APIs.

The existing snapshot wrapper composes actual required Python router declarations
as `x-python-knowledge-owner` metadata and namespaced components. Neither producer
reads `sdk/openapi.json`; the wrapper reads publication only afterward for the
existing coverage/ownership check. `verify-native-openapi-contract` also validates
this source/mapping evidence before its existing full comparison. Internal routes
are excluded and undeclared Request.json/streaming/dynamic shapes remain gaps.
See [the hybrid profile](../../../../../../../docs/design/knowledge-native-python-hybrid.md#source-owned-wildcard-contract-composition)
for client recovery and verification limitations. The two wildcard aliases do not
constitute complete native or live Knowledge acceptance.

### Measured contract boundary (2026-10-13)

The source-bound combined export measures 35 native paths/53 operations, including
ten opaque wildcard methods over the two existing aliases. The retained Python
owner contributes 101 concrete operations/59 reachable schemas as supporting
metadata only; 97 success declarations remain opaque and 18 mutating operations
have no declared request body. Two producer tests, four socket-free Gateway
routing/signature tests, 150 Python contract tests and changed-file Ruff passed.

The actual full native gate remains FAIL 449 with 399 missing published operations.
Adding the wildcard declarations removes two missing-path diagnostics and exposes
ten real auth differences plus ten missing universal 422 detail guarantees. Do
not hide these by copying published schemas or reclassifying all common APIs as
Python-owned. No live service/provider/SQL/Redis or deployment acceptance follows
from these controlled results; uncertain mutation outcomes still require the
existing exact readback/recovery workflow above.
