# Qdrant TLS and API/worker mutation roles

Source candidate only. No Cargo, Docker, SQL application or live Qdrant execution
was performed by this implementation writer.

## Bootstrap integration (primary owns service settings/bootstrap)

Use `migration_knowledge_query::qdrant::{QdrantStore,QdrantRole}`:

- API process: `QdrantStore::for_role(origin, read_key, QdrantRole::ApiReadOnly, ca_path)`.
- Worker process: `QdrantStore::for_role(origin, write_key, QdrantRole::WorkerWrite, ca_path)`.
- `ca_path` comes only from the controlled `QDRANT_TLS_CA_FILE` operator setting,
  converted to an absolute path. No HTTP request may select it. Custom CA requires
  HTTPS; PEM bundle parsing/read errors fail startup. Standard roots, certificate
  chain validation and hostname validation stay enabled. No TLS verification bypass.
- The pinned qdrant-client 1.19 gRPC channel only accepts native roots and exposes
  no custom CA injection. Set `prefer_grpc=false` explicitly with this custom CA;
  `set_prefer_grpc(true)` with a custom CA fails rather than ignoring that CA.
  REST retains all existing retry/deadline/circuit-breaker and wire checks.
- Set `ingestion.vector_job_actor_authority = Some(runtime_actor_client.clone())`
  on the worker instance. Its signed Gateway actor resolver refreshes tenant,
  identity and roles before every task; stored roles are never authority.
- Combined API+worker processes must construct separate read/write service
  instances. Do not share a write-key `Indexer` with API handlers.
- Legacy `QdrantStore::new` remains a worker-write compatibility constructor for
  existing explicit local test/ops clients; production must use `for_role`.

No new Cargo dependency. Existing modules wire `vector_jobs` and its dispatcher.
The worker must have enough pool capacity for its job session plus native
publication/document lease and ordinary queries (the existing default pool is
larger than that minimum).

## Durable SQL proposal

`database/proposals/knowledge-vector-mutation-jobs.sql` is unnumbered. Primary
must canonicalize through native authority, assign runtime grants and verify it.
The API needs INSERT, SELECT, and column-limited UPDATE(state,error_class,updated_at)
for explicit retry of failed tasks. Worker needs SELECT and UPDATE. Never grant API
UPDATE of tenant, requested_by, payload, expected generation or result.
The table deliberately retains rows after document/dataset deletion. No private
Qdrant key, bearer token, actor roles or server embedding credentials are stored.

## Main paths and semantics

API mutations enqueue and await their real worker result, preserving the endpoint's
result shape. A timeout returns a pending error with durable job ID, never success.
Retries reuse identity binding tenant, actor, dataset, full generation and canonical
mutation arguments. Failed jobs retry only on explicit resubmission. A cancelled
HTTP wait leaves durable work recoverable. No extra service or process is added.

Worker dispatch owns an advisory session lock per job across external I/O and
refreshes current actor before authorization. Native deletion leases, exact-target
recovery fences, Qdrant tenant/document filters, asset cleanup and database deletion
remain the original implementations. Under-lease generation checks also bind queued
operations. Only the exact deletion fence's +1 pending / +2 cleared revision may
recover, and a recreated target cannot be deleted using that cleared revision.

Migrated paths: dataset create/update; document/dataset/segment delete; segment
update/create/enable; document enable/archive; lexical cutover/rollback. Read-only
lexical verification/dry-run stay in API. Worker lexical methods retain allowlist
admission at HTTP, current operator verification at execution, kill switch, native
barrier/CAS, scan/manifest verification and emergency rollback behavior. A stable
manual-segment ID in the task prevents duplicate segments after worker restart.

## Verification primary must run

```sh
cargo test -p ai-platform-knowledge-domain --lib role_tls_tests
cargo test -p ai-platform-knowledge-domain --lib vector_jobs::tests
cargo test -p ai-platform-knowledge-domain --all-targets
cargo check -p ai-platform-knowledge-service --all-targets
```

Run with repository's external target directory, pinned toolchain and one job.
Live authority acceptance must additionally cover read-key API + write-key worker,
custom CA success/wrong CA/hostname mismatch, current ACL revocation while queued,
cross-tenant job IDs, duplicate concurrent submissions, worker death during
Qdrant write and database finalization, exact fence retry, document replacement,
lexical cutover/rollback with manifest mismatch and generation replacement.
Completed external effect followed by process death before terminal job update can
still produce a failed job if a new publication has superseded its captured
identity; it must never write into that new generation. Native lexical steady-state
operations and deletion recovery remain retryable via current-generation requests.
No quality, performance, concurrency or live tenant-isolation acceptance is claimed.

## Embedding migration completion

No DDL changes are required for these additional typed job payloads. API
embedding-migration start/cutover/rollback/abort now submit to the same vector
worker queue and return their actual original result shape. Existing asynchronous
backfill/verify/gate keep their native action-job queue and worker heartbeat/CAS.
The worker rechecks current owner permission and exact migration/dataset scope.
The embedding start collection reservation, cutover pinned gate, source/target
binding state checks, rollback retention rules and abort running-action check
remain the native implementations, with queued-generation checks under their
existing locks.

Cutover/rollback/abort atomically record this vector job ID in existing migration
`totals` JSON alongside the state transition. Restart recovery only accepts the
same recorded job and terminal state; cutover/rollback also require the correct
current serving binding, tenant, dataset and collection. Abort retries can resume
purging only this job's abandoned retired target. No forced transition or skipped
gate is introduced. Version restore also routes through this worker boundary.

The existing write router now exposes
`GET /api/v1/knowledge/datasets/{dataset_id}/vector-mutations/{job_id}` for durable
completion/failure visibility. Reads bind tenant, requesting actor and dataset,
recheck current dataset access and omit payload/configuration. A successful dataset
deletion can expose only its requesting actor's retained boolean receipt after the
dataset disappears. Pending HTTP errors contain this polling path.

Additional source regressions cover owner-only embedding operation payloads,
unknown operation rejection, migration dataset mismatch, distinct migration IDs,
and refusal to recover a different job or terminal state. These tests have not
been executed by this source writer.

Static whole-domain call-site audit: remaining vector writes in `index`,
`publication`, `special`, `legacy_images` are indexing/worker implementations;
`reclaim_expired` is an unmounted operator method with no repository caller.
Public HTTP embedding control methods and version restore are now queued;
read-only describe/verify and query paths keep their existing reads.
