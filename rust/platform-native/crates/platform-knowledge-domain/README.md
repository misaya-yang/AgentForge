# Knowledge domain

Owns dataset/document ingestion, immutable versions and generation-fenced publication,
retrieval/ACL enforcement, content-storage/image ports, signed Knowledge HTTP ingress and
capability proof verification. Constructors retain the existing explicit I/O ports.

`platform-knowledge-service` composes these modules and process helpers without importing
`ai-platform-gateway`. Gateway compatibility module paths reexport this canonical implementation
through integrations. Gateway current-actor and Agent authority adapters remain Gateway-owned.

The `host` subtree contains only CORS, idempotency, request/drain middleware and release identity.
The adapter subtree contains bounded response framing and DNS-pinned safe fetch, with its original
framing tests. No Gateway host/bootstrap, runtime loop, provider policy or billing assembly lives here.

Knowledge source now uses `ai-platform-service-primitives` for canonical JSON, the stateless
cipher/error contract, bounded HTTP client errors, DNS policy, redacted PostgreSQL operation
failure, endpoint resolution, and process telemetry/deadline helpers. That infrastructure crate
imports no Gateway/auth/Knowledge implementation. Its HTTP/telemetry adapters do I/O, so it is
not labelled a pure wire-schema crate. Shared Cargo and PostgreSQL error reexports must be
integrated by the primary before compilation.

Legacy JWT helpers now consume the neutral explicit `JwtActorVerifier` port; AuthService stays
in its own authority crate and implements the adapter there. Primary manifest/adapter integration
is required before compilation. Integrations/Gateway still import domain implementation via
compatibility aliases; that boundary must be replaced by existing signed transport and pure wire
types, not granted exceptions. The four-crate decomposition and complete no-hosted-implementation boundary remain
unaccepted pending those changes and actual gate execution.

The capability wire integration tests use a frozen Python-computed Unicode JSON/HMAC envelope,
and reject altered tenant, request, path, method, expiry and secret. Run domain/service parity suites,
Gateway compatibility tests and the architecture boundary gate after shared workspace integration.
Compilation and live ACL/publication acceptance are separate verification layers.

## Cache v2

Result caching keeps its existing default TTL of zero. When enabled it uses a concurrent Moka
future cache with a 64 MiB weighted payload budget, rather than a global async mutex and linear
scan. Keys include tenant/dataset, full publication generation, a hash of the verified actor's
user/tenant/roles/tier/type/authentication scope, normalized whitespace query and a fingerprint of
the complete dataset profile plus all request options/filters. Current dataset ACL, collection
readability, active result IDs and generation are still revalidated before returning a cached hit.
A publication changes the generation key, making previous results inaccessible; obsolete entries
are reclaimed by weighted/idle eviction. Entry TTL is checked on every hit and cannot be extended
by later increasing the public TTL setting. Returned results/metadata are detached clones.

Query embeddings use a separate 32 MiB weighted cache with the existing 30-minute TTL. The key
includes resolved provider/model/version/dimension/endpoint, credential digest, tenant/dataset,
publication revision/collection, embedding profile and exact provider input text. No query or
credential is retained in plaintext keys. Exact text is intentional: whitespace/case/Unicode
rewrites may change provider outputs. Unversioned remote model aliases bypass the cache;
the built-in local hash encoder has a fixed implementation version. This avoids reusing vectors
across an unidentifiable model replacement. Document embedding paths are unchanged.

Weights account for payload bytes and container overhead; these are bounded payload budgets,
not a claim of exact process RSS. Oversized results/vectors and empty/nonfinite vectors are not
admitted. Moka admission/eviction does not promise strict LRU order.

Required dependency (primary manifest/lock owner):
`moka = { version = "=0.12.11", default-features = false, features = ["future"] }`.
Run domain tests matching `result_cache::tests` and `query_embedding_cache_v2_tests`, then the full
Knowledge parity suites and real tenant/generation negative journeys. Cache-hit p95 <20 ms and
memory behavior require measured integration evidence; syntax checks alone do not accept U-KN-07.
