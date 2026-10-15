# Knowledge BM25 v2 shadow rollout

Status: implemented for shadow writes, authority-backed backfill, and offline
evaluation only. The live default remains `lexical_v1`. BM25 v2 active cutover
and serving are hard-disabled in production until a cross-PostgreSQL/Qdrant
lifecycle protocol is implemented and certified; this document does not claim
a completed production backfill or quality win.

## Why this is a new field

The existing Qdrant sparse field named `bm25` is retained as
`lexical_v1`. It uses the repository's FNV-1a term hash, deduplicated term
presence, and Qdrant IDF. Changing that field in place would mix incompatible
encodings in one posting list.

`bm25_v2` is a separate named sparse vector. Documents and queries use Qdrant's
native `Document(model="qdrant/bm25")` contract, while the collection field uses
`SparseVectorParams(modifier=IDF)`. The frozen schema fingerprint covers the
field/model, `k`, `b`, `avg_len`, tokenizer, language, lowercase, ASCII folding,
and token-length preprocessing. A changed fingerprint requires a new version or
a complete rebuild; it must not be written into the existing v2 field.

## Dataset configuration

The rollout flag lives under `index_config.retrieval.lexical`:

```json
{
  "retrieval": {
    "lexical": {
      "active_version": "lexical_v1",
      "bm25_v2": {
        "shadow_write_enabled": true,
        "field": "bm25_v2",
        "model": "qdrant/bm25",
        "k": 1.2,
        "b": 0.75,
        "avg_len": 256,
        "tokenizer": "multilingual",
        "language": "none",
        "lowercase": true,
        "ascii_folding": false,
        "filtering": {
          "required_payload_indexes": ["tenant_id", "dataset_id"],
          "strict_unindexed_filtering": false
        }
      }
    }
  }
}
```

The service-wide gate is disabled by default. Set
`KB_BM25_V2_ENABLED=true` only on an approved shadow deployment; turning it off
immediately suppresses shadow writes while preserving dense and `lexical_v1`
writes. A separate production hard-disable rejects any requested active-v2
configuration at the dataset boundary and again in the vector store. It is not
exposed as a runtime setting. The kill switch still permits an explicit legacy
active/shadow-to-v1 rollback, so an already persisted stale active profile can
be recovered.

The tenant, dataset, content type, hierarchy level, and enabled-state payload
indexes are verified before v2 ingestion. The tenant index must be a Qdrant
tenant-partition keyword index; level and enabled use integer and boolean types.
Strict
unindexed filtering is intentionally off by default because it is
collection-wide and can reject existing ad-hoc metadata filters. Setting it to
`true` first verifies every built-in/readiness filter index, then enables Qdrant strict mode;
active reads subsequently require that profile to remain ready.
Because strict mode is collection-wide, every additional filter field used by a
deployment must also have a compatible payload index before opting in.

## Rollout and rollback

1. Baseline: omit `retrieval.lexical`. Reads and writes remain exactly on
   `lexical_v1`; no v2 schema or metadata is created.
2. Shadow: set `active_version=lexical_v1` and
   `shadow_write_enabled=true`. New writes contain both fields. Historical
   points are not automatically backfilled.
3. Backfill: export the service PostgreSQL DSN through the configured environment
   variable (default `KNOWLEDGE_DATABASE__DSN`), create a signed, read-only
   manifest, dry-run it, then explicitly apply it. PostgreSQL
   enabled `segments.vector_id` rows for L3 text segments in the dataset's base
   collection are authoritative; Qdrant cannot self-prove completeness. Same-
   dimension image vectors and L1/L2 points are outside this versioned lexical
   authority scope. The job requires an exact ID-set match
   and stable dataset content revision before any mutation. It updates only the
   `bm25_v2` vector and reserved `_lexical` marker, then rechecks the authority
   revision, exact tenant/dataset scope, sorted point-ID digest, source-text
   digest, schema/filter fingerprints, and 100% point coverage before publishing
   the collection receipt:

   ```bash
   uv run --package knowledge-service python scripts/backfill_bm25_v2.py plan \
     --collection COLLECTION --tenant-id TENANT --dataset-id DATASET \
     > bm25-v2-manifest.json
   uv run --package knowledge-service python scripts/backfill_bm25_v2.py run \
     --manifest bm25-v2-manifest.json --dry-run
   uv run --package knowledge-service python scripts/backfill_bm25_v2.py run \
     --manifest bm25-v2-manifest.json --apply \
     --confirm-manifest-sha256 'sha256:...'
   ```

   `run` is read-only unless both `--apply` and the exact manifest hash are
   supplied. Do not use `scripts/migrate_sparse_vectors.py` for v2; that legacy
   migration is destructive and intentionally rejects this contract.
4. Evaluate: compare v1/v2 retrieval on a versioned golden set. A schema-ready
   collection is not quality evidence.
5. Cut over: **not available in this release**. A request with
   `active_version=bm25_v2` is rejected before the dataset configuration is
   saved, and direct vector-store transition/query paths also reject it. The
   capability, receipt, exact-scope, and readiness logic remains exercised as
   pre-certification evidence only. Enabling active serving requires a future
   protocol that excludes every PostgreSQL and Qdrant writer across the entire
   transition lifecycle, plus real concurrent integration tests. A stable
   before/after receipt alone does not close that cross-store race.
6. Roll back reads: set `active_version=lexical_v1`. Keep shadow writes enabled
   for a reversible read rollback, or set them to false to stop v2 writes. The
   v2 field and data are retained; rollback never deletes a collection, field,
   or alias.

## Failure behavior and evidence boundary

- Default v1 behavior does not depend on native BM25 availability.
- Capability is proved against the configured endpoint with an isolated
  create/write/query/delete canary. A missing-collection response is not a
  capability receipt. Canary receipts are keyed by endpoint, client version,
  and full BM25 profile and expire.
- Shadow is two-phase: the dense/legacy-v1 base write completes first; native-v2
  vector and point marker updates follow. A v2 failure is counted and logged but
  does not roll back or fail the successful base write. Active-v2 writes remain
  fail-closed.
- Active v2 is unreachable through production configuration. A stale or
  directly supplied active profile produces an explicit retrieval error; it
  never silently reports PostgreSQL FTS or the legacy sparse field as v2
  evidence.
- Empty text in shadow mode keeps the dense/legacy base write and records a v2
  shadow failure; active mode rejects it. No fake v2 sentinel is created.
- Every configured runtime write/delete replaces a completed remote receipt with
  a fail-closed `status=invalidated` sentinel before mutation (shadow v1 retains
  base availability if only receipt invalidation fails). Active readiness always
  recomputes and compares the exact receipt point count, sorted-ID digest, and
  source-text digest on every active query; there is no positive-readiness cache
  shortcut, and point markers alone are insufficient.
- Consequently, the current release is shadow-only. Receipt/readiness output is
  evidence for shadow evaluation, not authorization to cut over. Active serving
  requires a future cross-store lifecycle mechanism and is not claimed here.
- BM25 v2 lifecycle metadata and receipts belong only to enabled L3 text points
  in the dataset's base collection. Hierarchical summary/section and dimension-
  specific image/scanned collections remain dense plus `lexical_v1`; every
  collection write and dense query carries mandatory tenant and dataset filters.
  Same-dimension image vectors may remain co-located because the PostgreSQL
  authority, Qdrant plan/apply scan, and runtime readiness scan use the same
  versioned enabled-L3-text scope. Disabled, image, and L1/L2 points are never
  mutated or counted as BM25 v2 receipt coverage.
- Unit/integration-mock coverage and the native capability mechanism are present,
  but this document does not claim that a production backfill, live quality
  evaluation, or production cutover has completed.

References:

- [Qdrant full-text search and BM25](https://qdrant.tech/documentation/search/text-search/full-text-search/)
- [Qdrant server-side BM25 inference](https://qdrant.tech/documentation/inference/inference-bm25/)
