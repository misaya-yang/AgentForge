# U-KN-07 integration/measurement handoff

The actual standard, fused-batch and hierarchical result paths now use the same weighted Moka
cache helper. Each path performs current dataset ACL, generation and collection preflight before
lookup. Cached standard/leaf rows must still be active in SQL; hierarchy summaries additionally
reuse the existing document/summary lifecycle filter. A final require_generation rechecks current
ACL and publication state after those reads. A changed generation cannot consume a previous key,
even when an older in-flight computation inserts after publication.

Keys bind the verified actor's tenant/user/roles/tier/type/authentication hash, dataset identity,
complete IndexGeneration, normalized whitespace query, original provider-input digest and complete
dataset/request profile. Batch keys include all deduplicated request specs and filters. Direct
hierarchical calls force the hierarchy cache namespace even if the caller omitted the flag.
The exact-input guard is intentional: whitespace normalization is an index component, not a
permission to change actual provider text or reuse a vector with different provider bytes.
There is no lowercasing/Unicode rewrite or implicit retrieval-profile promotion.

Only a configured positive result TTL enables result caching; its original default stays zero.
Unversioned remote dense profiles bypass result caching and actor-serving embedding caching;
operator-pinned model revisions or deterministic built-in local hashing are cacheable. BM25 can
cache without an embedding revision because it does not call the embedding model. The complete
rerank/profile options, including supplied model revisions, remain part of the result profile hash.
Recall errors, rerank degradation and MMR failures are never memoized. Hierarchy empty/degraded
returns are not cached. Both error and unavailable paths preserve existing errors/output behavior.

Embedding main-pipeline calls now pass the verified actor to embed_query_for_actor. Its key includes
ACL scope, tenant/dataset, complete generation, provider/model/model revision/dimension, actual
endpoint and credential digest, profile, normalized text and exact provider-input digest. Concurrent
identical scoped calls share one successful load through Moka try_get_with; errors are not cached.
Original query text and query/document text_type remain unchanged. Hierarchy intentionally embeds
as document and retains that existing semantic distinction.

The public legacy embed_query API remains usable by existing operator/parity callers with its
original 30-minute configuration cache, in an isolated operator namespace. It never shares entries
with actor-serving requests. Its unversioned-provider fallback is explicitly a legacy process-local
configuration identity, not proof of an immutable remote revision. Current request pipelines do not
use that fallback. Existing provider adapters continue resolving tenant credential changes before
lookup, so credential/endpoint rotation changes cache identity.

Result weights now account for retained string/vector/JSON allocations and approximate map-node
bookkeeping, avoiding full response serialization during insertion. It remains a payload budget,
not a claim of exact allocator RSS. Expired entries are never returned and a TTL increase cannot
resurrect an entry admitted with a shorter lifetime. Returned values are detached copies. Query
vectors have a separate weighted 32 MiB / 30-minute cache; result budget remains 64 MiB.

Primary serialized checks (NOT RUN by this worker):

```sh
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-domain result_cache::tests
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-domain query_embedding_cache_v2_tests
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-service --test knowledge_migration_parity_embedding
```

Use the established external target/single-job/no-debug environment. New negative/concurrency tests
cover 32 concurrent ACL partitions, late old-generation writes, full batch/hierarchy key differences,
degraded-result refusal, retained JSON weight, 24-way single-flight loading, failed-load recovery and
actual local-hash EmbeddingManager actor/generation calls without provider I/O. These tests are
cache contracts; they are not a measured quality or live-ACL acceptance substitute.

For live acceptance, use the original signed Gateway Knowledge transport and a version-pinned real
dataset. Measure standard/batch/hierarchy hits after warmup using external wall time and confirm
returned results against the same current golden expectations. At least 1,000 eligible cache-hit
samples per supported profile should report p95, concurrency and byte budgets; p95 must be <20 ms.
Do not reuse cached cold-run timing metadata as measured latency: hit metadata now stamps actual
current request elapsed time and a fresh observation. Separately test two actors/roles/tenants,
permission revocation, document disable/archive, collection/model revision changes and fenced
publication; forbidden actors must fail current ACL checks, and old entries must not hit. Record
actual passed/failed/unrun outcomes in the primary program receipt. No latency or live acceptance
is claimed here.

Module wiring: query/mod.rs now declares cache_identity; no Cargo/features/root lib/source-lock
changes are needed. Moka future support is already present. Worker changed only domain query
cache paths and this domain-owned handoff.
