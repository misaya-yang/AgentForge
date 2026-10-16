# Knowledge cache regression batch 4

Root authorized minimal source-proven cache parity repair. New private child
`migration_knowledge_query/service/result_cache` implements the existing deque
operations and is called by real RetrievalService under its existing mutex.
The cache still requires the existing preflight, dataset/tenant access,
generation and active-segment checks, still defaults disabled and still caps
at2000 entries. A rewrite now replaces the same key without growing the cache;
an expired read removes its stale entry. Hit recency already worked and remains.

Five unit tests under
`migration_knowledge_query::service::result_cache::knowledge_migration_parity_cache`
exercise the production operations, with deterministic injected monotonic time:

- Exact `test_cache_manager_lru.py` four original cases: hit-refresh eviction;
  overwrite/no growth/latest value; expiry deletion; disabled-TTL bypass.
- Additional copy-isolation assertion for returned rows and metadata.

The test uses capacity2/8 exactly as the source tests, while production retains
capacity2000. No cache-authority shortcut or synthetic retrieval success path
was introduced. Whole original cache-manager suite is candidate retirement
eligible after root compiles and passes these tests. Written/formatted only by
this worker; no Cargo, live DB, Qdrant or external provider calls performed.

## Signed URL utility closure

Root also authorized the dormant exported `verify_signed_url` equivalent beside
AssetStorage's existing signer. Archived Knowledge source has no production
callers; no route was added. It preserves legacy no-key acceptance, raw encoded
path signing, first nonempty signature/expiry query parameters, expiry rejection,
source diagnostic strings and constant-time truncated HMAC verification.

The assets integration target now has five tests. Its new test includes the
last original signing/verifier compatibility assertion plus wrong-key/path,
missing/empty/invalid/expired input and duplicate-query checks. A separate
`signed_url_contract` unit test pins subsecond expiry boundary and relative/raw
paths. Scope-suite whole-file retirement is now eligible after root verifies
all five tests; original four were observed passing in root run75.

Implementation note: expiry parsing currently accepts signed 64-bit integer
strings (ordinary generated Unix expiry values), while Python int also accepts
arbitrary-precision and Unicode-decimal/underscore forms. These unusual exported
helper inputs remain a precise parity boundary, not an active route change.
