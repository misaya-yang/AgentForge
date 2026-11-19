# Knowledge Service Rust migration

Updated 2026-10-15. Implementation is in `rust/platform-native/crates/platform-knowledge-service` (binary `platform-knowledge-service`). No Python runtime, subprocess, RPC wrapper, or Python-generated runtime output is used. Original source remains the migration reference until the coordinator completes verified retirement.

## Implemented runtime

- Dedicated API/worker/all-role composition; real PostgreSQL/Redis/Qdrant/object-store/provider integrations reuse the native domain crates. Signed gateway admission, nonce replay protection, idempotency, tenant ACLs, readiness probes, drain/shutdown, durable worker startup and recovery are composed into this binary.
- Existing native Knowledge query, retrieval, QA, evaluation, dataset/document/segment writes, inspection, metadata, version restore, batch admission, lexical lifecycle and embedding migration route families are mounted. This is executable composition of real lower-layer implementations, not placeholder route responses.
- Health compatibility and public release identity response preserved; worker role does not expose API release route. Queue gauge reads the canonical dispatchable-document predicate and retains the last good sample on DB refresh failure.
- All twelve Confluence connection/binding/manual-sync HTTP operations: safe input parsing, owned connection visibility, cloud-domain/account verification, scoped encrypted credential envelope, insert-only binding identity, current actor/editor resolution, connection revision compare-and-swap, impact projection, pause/resume and credential replacement.
- Source catalog projects owned bindings and live setup capabilities without disclosing other owners' connections.
- Manual sync performs durable task admission, page/root traversal with complete cursor checking, current ownership/permission checks around provider and publication work, source-generation hashing, native XHTML text/Markdown conversion, attachment fingerprinting, native object-store receipts, hidden generation claims, document versions, publication into the durable worker queue and source-aware removals.
- Deletion uses exclusive lifecycle lease, exact observed source reference checks, shared-reference protection, deterministic/legacy source-document checks, durable exact-target deletion fence, Qdrant/object cleanup, atomic reference/document deletion and fence clear.
- Shutdown aborts owned provider tasks, waits for cancellation and records interruption for every owned durable task. It does not automatically replay unknown crash-era manual tasks.
- Image preparation re-discovers and compares the admitted attachment manifest, applies deterministic size/count admission, extracts surrounding text, follows bounded HTTPS attachment redirects, writes tenant/document-scoped images and produces worker-rebuildable generation receipts.

## Native PDF implementation

Owned `pdf_backend.rs` executes trusted Poppler tools directly: pdfinfo, pdftotext, pdftoppm, pdfimages, pdfseparate and pdfunite. It supports text, rasterization, embedded-image xrefs, streaming page/image extraction, resize and page-range serialization. Private scratch, input/output/page/pixel bounds, process concurrency, process-group termination, deadlines and Unix resource limits are applied. No shell interpolation or Python library is involved.

The upload route streams multipart files to a private path, probes every PDF before publication, rejects page-limit/compressed-upload violations with 413, splits above the configured threshold off the async executor, bounds cumulative derived bytes, publishes/enqueues one part at a time and cleans the complete workspace on success/error/cancellation. `KB_PDF_SPLIT_MAX_SIZE_MB`, `KB_PDF_SPLIT_PAGES_PER_PART`, `KB_PDF_MAX_PAGES` and `KB_PDF_SPLIT_MAX_OUTPUT_BYTES` retain source defaults/hard clamps.

Deployment must install the six Poppler tools. Engine-specific text, malformed-PDF tolerance, raster and serialization differences versus pypdf/PyMuPDF require fixture acceptance; this implementation does not claim bit-identical decoder output or pypdf decompressed-stream accounting.

## Verification

Real PostgreSQL source-generation acceptance passed on the canonical fixture schema, using session-local tables: initial claim, competing owner rejection, stale publication rejection, durable waiting publication, late abort no-op and upload-generation fence. Evidence: `knowledge-source-acceptance.log` in the shared verification directory. The built Rust SQLx opt-in test also passed against that fixture (`knowledge-postgres-rust-test.log`, 1/1). This validates SQL and driver binds, not the complete provider/HTTP workflow.

Coordinator verification `knowledge-tests4.log` passed: 20 unit tests and 5 HTTP/process contract tests, zero failures; the opt-in PostgreSQL fixture test was ignored in the default run. All five real Poppler tests passed, exercising text, raster, embedded-image xrefs, streaming image resizing and serialized page ranges. Cargo commands were run exclusively by the coordinator. Initial dependency/test Debug compile issues were corrected before this passing run.

Owned Rust tests cover credential scope/encryption, Confluence parsing, queue gauge SQL, API health/security/version/unsigned-source boundaries, real Poppler text/raster/embedded-image/streaming/split behavior, invalid PDF page ranges, an ignored opt-in real PostgreSQL generation-claim/publication/abort test, pre-publication page-count rejection, exact split ranges, cumulative output budget and private-workspace cleanup. These port focused scenarios from `test_upload_pdf_offload.py`, `test_pdf_splitter.py` and source credential/parser contracts.

## Remaining acceptance / precise limits

- PostgreSQL/Qdrant/Redis/object-store/provider integration acceptance for new Confluence execution is not yet performed. Source SQL race tests for concurrent connection revisions, source generation claims, reference detach and cancellation must be exercised against migrated Rust behavior before Python tests are retired.
- The original Knowledge test inventory is large (122 modules under `tests/services/knowledge`, plus broader service/API tests); only the focused tests listed above are newly ported here. Existing native lower-layer tests supply additional coverage, but that does not establish one-to-one migration of all Python fixtures.
- Native retrieval/pipeline Prometheus instrumentation has now been added to the approved shared modules: successful retrieval/cache latency, degraded rerank, BM25 readiness, embedding batches, durable claim/ingest and recovery counters. The dedicated scrape combines these with the queue gauge. Two standalone bounded-label/histogram tests passed; full workspace integration rerun is pending. Optional OTLP export is now wired through the shared native tracing provider; automatic SQL/client instrumentation remains unconnected.
- Qdrant gRPC configuration still requires source parity review; native CPU BGE and cross-encoder implementations are described below. Embedding document batch size now projects source configuration/default50 while retaining provider caps. Qdrant retry settings now project source defaults (5 retries, 2-second base); the declared-but-unused Python metadata-LLM enable flag remains accepted without inventing execution. The current host explicitly rejects unsupported gRPC rather than silently substituting transport.
- Active Python `build_manual_sync` does not pass a VLM service; the Rust source-image stage likewise writes extracted context and null VLM descriptions. The dormant injectable Confluence VLM extension is now available through with_image_vlm, with source context/image-type hints, a 60-second timeout, best-effort description failures and source authority checks. Normal composition leaves it disabled, as in the source entrypoint.
- Python automatic scheduler is not started by the authoritative source entrypoint; only its actively mounted manual workflow is composed here. Dormant helper coverage must be inventoried before deleting the complete reference package.
- Full migration acceptance requires successful binary build, integrated runtime checks, deployment wiring and replacement of all remaining relevant Python test/operational tooling. Source presence, route count and compilation are not treated as completion.

## Extended shared ownership

At coordinator authorization, precise telemetry hooks were added in `platform-gateway-integrations/src/migration_knowledge_query/{pipeline_metrics,service,embedding,qdrant}.rs`, module registration, and ingestion `service.rs`/`worker.rs`. Gateway owner confirmed no overlapping edits. These changes add actual internal measurements rather than approximating retrieval with HTTP response counters.

## Native local inference and utility follow-through

The default BGE provider/model now has an actual lazy CPU inference backend in `native_rerank.rs`, built on Candle's XLM-RoBERTa sequence classifier and Hugging Face's Rust tokenizer. The exact BAAI `bge-reranker-v2-m3` revision `953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e` and SHA-256 identities for config/tokenizer/safetensors are checked before loading. Pair construction follows FlagEmbedding's default 512-token window, 384-token query cap, document-only truncation and sigmoid normalization; CPU weights/activations use float32. Loading is synchronized and inference is off the async executor. An explicitly configured local HTTP backend still takes precedence.

Provision immutable official artifacts under `KNOWLEDGE_NATIVE__BGE_MODEL_DIRECTORY`, or the default `data/models/bge-reranker-v2-m3/<revision>` under the process working directory. Requests do not fetch weights or execute remote model code. The implementation adds pure truncation/hash/normalization tests, a tiny real Candle classifier graph test and an ignored pre-provisioned-model inference test. At the user's instruction no model weights were downloaded and no live model inference was attempted. Compilation and targeted tests are pending coordinator execution. BGE Gemma and nondefault sentence-transformer cross-encoders remain explicit unsupported model paths; default model implementations are not evidence of those models working. The source local embedding implementation is the separately ported deterministic LocalHashEmbedding.

`ocr_utils.rs` ports the independent best-effort OCR helpers: source language validation and DPI/timeout clamps, English fallback, VLM/Tesseract/hybrid dispatch, bounded concurrent whole-PDF OCR with deterministic page ordering and native decoder cleanup. It reuses the real native Tesseract and Poppler paths and does not enable the source-disabled whole-PDF OCR method in the bounded text-only parser. Missing optional engines retain empty-text utility semantics. Two focused utility tests are added; they await coordinator execution.

The same native XLM-R classifier also handles `BAAI/bge-reranker-large` at revision `55611d7bca2a7133960a6d3b71e083071bbfc312`, with independent exact artifact hashes and position-embedding shape. Configure `KNOWLEDGE_NATIVE__BGE_LARGE_MODEL_DIRECTORY` or provision `data/models/bge-reranker-large/<revision>`. The default `local` provider model, `cross-encoder/ms-marco-MiniLM-L-12-v2`, is implemented in `native_cross_encoder.rs` at revision `7b0235231ca2674cb8ca8f022859a6eba2b1c968`: actual BERT encoder, CLS pooling dense/tanh, classifier, official tokenizer pair truncation, and Identity activation/raw logits as required by that model's config. Configure `KNOWLEDGE_NATIVE__CROSS_ENCODER_MODEL_DIRECTORY` or provision `data/models/ms-marco-MiniLM-L-12-v2/<revision>`. Both are lazy, verified-artifact CPU implementations; neither weights nor live inference were used during this work.

The first coordinator Candle compilation reached owned source and reported two non-model errors: an unexported metrics facade and OCR JoinSet inference. Both were fixed; the gateway facade's exact one-line reexport was authorized by its owner. Subsequent check/test result is pending.

## Knowledge ops and schema declarations

Separate `platform-knowledge-ops` now implements all six assigned operator commands: golden-case import/store, fixed bilingual bakeoff case construction, actual reranker bakeoff, Chinese lexical shadow evaluation, read-only retired sparse migration inspection, and deterministic BM25 v2 shadow backfill. Operator-only eval/JSON parser/Jieba dependencies do not enter the service dependency graph. See the crate README for exact command mapping and write/live-call guards. No operator commands were run against live data/providers.

Backfill preserves the source authority boundary: repeatable-read PostgreSQL scope and vector/text manifest, immutable collection scope, lexical fingerprints and keyword tenant indexes, exact four-way Qdrant scope counts, bounded paginated scans, canonical signed manifest, explicit hash-confirmed apply, prewrite receipt invalidation, updates only to the named BM25 vector and reserved marker, and repeated exact postwrite/postreceipt verification. It does not enable active BM25 serving. Source SQL is compiled into the native binary as SQL, with no Python dependency. Golden store includes import/list/count/release pin/read and explicit split promotion transactions.

The native Chinese evaluation uses real `jieba-rs` segmentation; dictionary-version differences versus optional Python Jieba remain an output-parity consideration. Learned-sparse's offline skip is preserved, not reported as working inference. Native Markdown reports preserve evidence content rather than exact old formatting.

`native_contract.rs` independently declares the 12 mounted Confluence operations and upload override, native request validation bounds and source operation IDs. It hashes native handler/DTO sources and explicitly labels dynamic DB response projections opaque. It can be included by the lightweight OpenAPI producer without linking service dependencies. This is 13 actual owned operations, not a claim that all reused lower-layer Knowledge DTOs have source-owned schema declarations.

Coordinator `workspace-check8.log` successfully checked both Knowledge service and operator crates; targeted tests of the newest additions are pending. Original source contains only `LocalHashEmbedding`, not a `local_semantic` embedding implementation; the native local-semantic HTTP profile is a previous extension. The Gemma model advertised in source's generic BGE class is a causal-LM artifact, while that class loads sequence classification: correct Gemma LLM prompting/scoring would be a separate behavioral change and remains unimplemented pending coordinator direction.

Additional focused Rust tests pin source LocalHashEmbedding BLAKE2b index/sign/normalization, exact Latin/CJK token alphabet, empty vectors and repeated-token invariance. These confirm the actual Python local embedding contract rather than substituting a new semantic model. Tests are awaiting coordinator execution.

## Native tracing follow-through

`tracing.rs` now supplies the source's process-once SDK initialization, parent-based sampling, optional OTLP/gRPC batch exporter with native TLS roots, W3C traceparent/tracestate extraction and current-span carrier, HTTP server spans and source status semantics. The inbound middleware preserves the source execution ordering after request ID and before CORS. Response streams are never collected. Missing collector configuration retains in-process spans; exporter construction failure is sanitized and does not prevent startup. Shutdown flushes the SDK off the async executor before runtime teardown. A local SDK propagation/sampling test needs no collector. Source automatic asyncpg/client instrumentation is not claimed by this server-span implementation. The provider now lives in the shared foundation distributed_tracing module; Knowledge retains only a request-ID/service-name adapter. No live collector was contacted.

Coordinator direction confirms the advertised Gemma source class/model mismatch remains an explicit unsupported path; implementing a different causal-LM scoring protocol would change behavior. This is not a reason to claim arbitrary advertised models work.

The Confluence public parser utility surface additionally covers complete image-reference metadata and MIME/extension embedding eligibility, heading structure, and source English-word/CJK-character reading-time estimates. These are callable native library utilities, with focused tests, rather than dormant Python dependencies. Legacy polling scheduler and the full legacy sync-service management surface remain an explicit migration gap.

`confluence/discovery.rs` adds real credential-scoped provider methods for get/list spaces, cursor-paginated space pages, page preview and complete space page-tree discovery. The pure projection preserves source orphan/root handling, stable title ordering, child flags, depth clipping and URLs. Provider pagination extracts cursor values and keeps requests on the configured trusted origin, with repeated-cursor rejection. These helpers are available to native callers; they do not add HTTP endpoints or claim that the remaining legacy authority/service/scheduler layer is migrated. Two pure tests cover tree projection and cursor isolation; no provider calls were run.

## Dormant legacy Confluence migration

The previously missing legacy path now has independent native modules, without changing active manual-only startup:

- `scheduler.rs`: explicit singleton lifecycle, binding/page task registry, priority and interval handling, due-time checks, reload/reschedule, source incremental-to-full fallback, capped exponential retry backoff, real polling/next-sync persistence and shutdown cancellation.
- `legacy_management.rs`: legacy tier/owner ACL, scoped connection CRUD and validation, encrypted-credential rotation refusal, binding/page/task listings, root configuration updates, stats/status and cleanup. The actual persistence whitelist and source list limits are preserved, including source fields that its persistence layer ignores.
- `legacy_clients.rs`: synchronized per-connection credential identity cache, TTL refresh and invalidation. Credentials are never Debug/log output.
- `legacy_sync.rs`: concrete scheduler backend, legacy task admission/stale recovery, full/partial/incremental background execution, CQL changed-page discovery, durable source-aware deletion, batch dispatch, URL import, direct source ownership and reserved-metadata checks, binding creation/deletion and webhook page synchronization.
- `legacy_publication.rs`, `legacy_images.rs`, `legacy_detach.rs`: real generation SQL, document/dataset leases, native assets and optional VLM, source receipts, version history, durable queue handoff, reference-aware document removal. Separate authority handling retains the source legacy context instead of manufacturing manual current-actor/task authority.

Coordinator `workspace-check13.log` finished successfully, checking the registered scheduler and early legacy engine. Later management/cache/import follow-through needs another compiler/test pass. These additions have not been exercised against a live database or Confluence provider, as requested. Source-level edge parity still needs review, particularly legacy provider error shapes, client-origin support, unchanged outcomes and standalone image context metadata edges. Do not interpret these modules as integrated acceptance or complete test parity.

## Final native source retirement map (2026-10-15)

This map identifies replacement implementation ownership; it is not evidence that every historical fixture has passed. Paths below are relative to `rust/platform-native/crates` unless marked otherwise.

| Historical source family | Native implementation |
| --- | --- |
| Knowledge main, settings, API dependencies, health/version, role startup | `platform-knowledge-service/src/{lib,main,application,lifecycle,process,settings,http}.rs` |
| Gateway signed admission, request identity, replay, idempotency, shared HTTP policy | Dedicated service composition plus `platform-gateway/src/migration_host` and native identity foundation |
| Dataset/document/segment APIs and persistence, batches, artifact ownership, restore/versioning | `platform-gateway-integrations/src/migration_knowledge_ingest/{dataset_*,document_*,batch*,artifact_sql,versions,version_reads,special*,store,sql}.rs` |
| Query, retrieval/hierarchy, index, embeddings, lexical lifecycle, embedding migration, QA and eval | `platform-gateway-integrations/src/migration_knowledge_query` |
| Ingestion worker, parser registry/cascade, structured IR/table rendering, extraction/chunking, assets and vision | `platform-gateway-integrations/src/migration_knowledge_ingest` |
| PDF splitting/decoder backend, upload budget, standalone OCR | `platform-knowledge-service/src/{pdf_backend,pdf_readiness,upload,ocr_utils}.rs` with native Poppler/Tesseract executables |
| Local BGE and default cross-encoder reranking | `platform-knowledge-service/src/{native_rerank,native_cross_encoder}.rs`, installed through the shared reranker abstraction |
| Active Confluence source connections/bindings/manual synchronization | `platform-knowledge-service/src/confluence/{connections,bindings,credentials,client,sync,publication,images,detach,sql}.rs` |
| Dormant Confluence discovery/parser/client caching/legacy management, scheduler and sync engine | `platform-knowledge-service/src/confluence/{discovery,parser,legacy_clients,legacy_management,scheduler,legacy_sync,legacy_publication,legacy_images,legacy_detach,legacy_image_segments}.rs` |
| Schema admission | `platform-migration-authority/src/application_guard.rs`, called before Knowledge readiness/worker start |
| Prometheus query/ingest instrumentation | Shared `migration_knowledge_query/pipeline_metrics.rs` and service `metrics.rs` |
| Distributed tracing | Shared `platform-gateway-foundation/src/distributed_tracing.rs` with Knowledge `tracing.rs` adapter |
| Golden import/store, reranker cases/bakeoff, Chinese lexical shadow eval, sparse-vector retirement and BM25-v2 backfill | Separate `platform-knowledge-ops` crate (six named native commands) |
| Historical live Qdrant smoke (two cases) | `platform-knowledge-service/tests/qdrant_integration_smoke.rs`, explicit ignored live target |

The final dormant image helper now performs actual asset/VLM rebuilding, legacy image segment SQL with the original conflict-update columns, dataset-configured text embedding and Qdrant publication, and document segment-count refresh. Complete attachment receipt checks preserve empty/explicitly skipped source generations. It is not newly registered as an HTTP route or process task. The old helper's best-effort cleanup behavior is retained; source-generation publication remains strict. Full sync alone updates binding page counters; single newly created page responses omit the existing-page `image_count` field.

The two native live Qdrant tests create only randomly named, immutable-scope disposable collections and clean them afterward. They exercise actual collection creation, dense/sparse upsert, native RRF, tenant refusal, segment deletion, legacy tenantless cleanup, document collision filtering, and missing collection cleanup. The previously absent `delete_document_points_by_ids` API now implements source owner/lease validation, scope checks, lexical receipt invalidation, exact-ID plus document/tenant filters and post-delete count verification. Tests are ignored by default and have not contacted a live server in this migration phase. Explicit execution fails if the requested server is unavailable rather than reporting static success.

Remaining limitations before a full-parity claim: transport gRPC is rejected; published Gemma causal-language-model weights do not implement the source's advertised classification-model API; official model weights/live inference have not been run; native PDF/Jieba engine outputs need fixture parity acceptance; automatic SQL/client tracing is not connected; Confluence cloud-origin/error-shape and standalone image context metadata edge cases need acceptance review. The complete historical test inventory is not yet ported one-for-one. Deleting historical test files does not remove those verification gaps.

### Native retrieval-tool adapters

The final retirement audit identified `services/knowledge/langgraph_tools.py` as a genuine missing adapter surface. It is now implemented in `platform-knowledge-service/src/langgraph_tools.rs` and exported by the library: typed retrieval inputs/results, authenticated `KnowledgeRetriever` (ordinary/text/metadata methods), `MultiDatasetRetriever`, Dify retrieval projection, configurable single/multiple-dataset KB tools, native executable descriptors, all four factories, and a synchronous-to-async runtime bridge. These call the real native retrieval service and preserve ordered concurrent fanout, source per-dataset exception isolation, score sorting/truncation, string/object invocation, query/top-k/threshold/text-intent bounds, Unicode text truncation and numbered score/context formatting. Source text-only intent validation is retained without inventing an image intent or forwarding an intent that the original ordinary adapter discards. Four pure contract tests cover validation, formatting, duplicate arguments and intent forwarding. No Python or LangChain subprocess/object is created.

Coordinator `workspace-check23.log` passed before this final adapter module was added; the adapter needs the next serialized compile. Configurable local cross-encoder model coverage remains narrower than the original arbitrary sentence-transformers model-name loader: the native default model is implemented, but arbitrary third-party architectures are not implicitly claimed supported. The gRPC transport follow-through is separately owned by the Gateway migration worker.

## Authorized native transport upgrade

The subsequent architecture instruction authorizes a real optional typed internal gRPC path. `rpc.rs` implements the shared `platform.knowledge.v1.KnowledgeRetrieval/Retrieve` service from `platform-rpc`. Typed dataset/query/top-k/mode/filter mirrors must agree with the exact HMAC-signed JSON bytes. Typed result hits accompany the complete original HTTP response JSON and bounded Retry-After. The adapter dispatches through the existing Axum router using Tower in-process; signed gateway authentication, nonce replay, trusted actor extraction, tenant/dataset ACL and generation checks remain the actual authorities. There is no network HTTP hop and no unauthenticated alternate retrieval entrypoint.

Direct-binary gRPC defaults disabled. Set `KNOWLEDGE_GRPC__BIND_ADDRESS` to an internal socket address (Compose target `0.0.0.0:8093`), optionally `KNOWLEDGE_GRPC__MAX_CONCURRENT_REQUESTS` (32, range 1–256) and `KNOWLEDGE_GRPC__DEADLINE_MS` (30000, range 100–120000). Worker-only processes reject a listener configuration. Complete protobuf requests are capped at 1 MiB and responses at 16 MiB, matching the shared client. Exact encoded lengths are checked, so the effective JSON capacity is lower when typed mirrors are included. A process-wide semaphore rejects excess RPC work immediately. The complete dispatch/body collection has a bounded deadline; dropped RPC futures cancel native work. HTTP and RPC listeners share signal handling, draining and bounded graceful shutdown. RPC does not retry failed authority or silently fall back to HTTP.

The shared HTTP governance middleware is wired inside existing tracing/authentication. `KNOWLEDGE_HTTP__MAX_CONCURRENT_REQUESTS` defaults 1024; `KNOWLEDGE_HTTP__RESPONSE_HEADER_TIMEOUT_MS` and `KNOWLEDGE_HTTP__RESPONSE_BODY_IDLE_TIMEOUT_MS` default 120000 and accept explicit `null` to disable a deadline. Permits and cancellation follow streaming body ownership; there is no total SSE duration limit. Health/metrics remain exempt. These limits supplement, rather than replace, domain admission and tenant quotas.

New RPC unit contracts verify the real signature/replay middleware with native in-process dispatch, missing authority, changed signed bodies, and bounded configuration. No database/model/provider calls are used. These additions await the coordinator's next compile/tests; protocol/client and Qdrant gRPC migration are owned by their respective workers.

### Verified native transport checkpoint

Coordinator `workspace-check41.log` completed successfully for all workspace targets on the final reqwest 0.13.5 / Redis 1.7.1 / tokio-tungstenite 0.30 stack. `knowledge-rpc42.log` then ran all four Knowledge `rpc::tests`: 4 passed, 0 failed. This includes the actual loopback HTTP/2 tonic listener and shared native client routed through real signed Gateway middleware, successful authenticated dispatch, replay refusal, altered-body refusal, missing-authority refusal, configuration limits and graceful shutdown. No database, remote provider, model weights or live Qdrant was contacted by these tests. The LangGraph adapter and dormant helper code are compilation-verified by the all-target check; no additional execution coverage is inferred from that fact.

## Final regression-retirement review (2026-10-15)

The native test counterpart review was sent to the retirement-map owner without
editing the shared JSON. Whole-file assertion equivalence is **not** claimed.
The capability-plane five-test native target, Unicode chunking four-test target,
QA provider shared fixture target, and two explicitly ignored live Qdrant tests
are concrete counterparts. PDF split/upload and Confluence receipt/hostname tests
are partial counterparts only.

Added one focused high-risk regression:
`confluence::credentials::tests::envelope_rejects_every_scope_change_key_loss_and_corrupt_marker`
ports the ten key/scope/marker corruption cases and unchanged legacy read assertion
from `test_confluence_connection_onboarding.py::test_envelope_fail_closed_and_legacy_unchanged`.
This new test was formatted but has not been run at this checkpoint. The original
monkeypatched cipher-noop assertion is not represented by this test.

Confluence DB workflow regression obligations remain: pause/resume exact CAS,
lost-acknowledgement metadata readback, active-task invalidation after asset work,
reconnect fresh-authority CAS and atomic replacement, provider-read-to-binding
revocation races, and deletion/publication races. Native signed RPC tests and the
source-generation SQL test do not establish all these assertions.

All three owned Confluence reqwest builders now explicitly disable protocol-level
retries with `retry(reqwest::retry::never())`. Existing application-level provider
retry loops remain intact. This prevents reqwest 0.13 implicit retry behavior from
adding attempts beneath the explicit source retry contract. No live calls or
Cargo commands were run in this final review.

### Runnable Confluence CAS/revocation regression batch

Added `src/confluence/knowledge_migration_parity.rs`, included as
`confluence::connections::migration_parity`. Four explicit ignored PostgreSQL
tests invoke production `Sources::control`, `change_connection`,
`connection_status`, `guard`, and `guard_on`. The fixture requires
`KNOWLEDGE_TEST_DATABASE_URL`, uses one retained connection with session-local
minimal typed tables and `search_path=pg_temp,pg_catalog`, and a loopback actor
resolver. Persistent fixture tables are neither read nor modified. The real
service graph is constructed but lifecycle workers, model loading, Qdrant and
external provider calls are not started.

Run explicitly with `-p ai-platform-knowledge-service --lib
confluence::connections::migration_parity -- --ignored --test-threads=1`.
This batch was formatted, but **not compiled or executed by this worker**.

- `connection_control_exact_cas_preserves_credentials_and_readback`: ports the
  plain/encrypted credential branches of source controls
  `test_pause_resume_exact_cas_preserves_credentials_and_never_calls_provider`:
  pause, stale 409, resume, advancing revision, unchanged credentials/other
  columns, credential-free metadata and no extra sync task. Also exercises real
  metadata readback after discarding a successful acknowledgement. It does not
  reproduce source `test_lost_write_ack_readback_returns_current_state_without_retry`'s
  injected COMMIT acknowledgement failure or HTTP 500.
- `pause_fast_resume_and_replacement_invalidate_admitted_task`: ports source
  `test_pause_and_fast_resume_invalidate_active_task_next_check` for paused and
  fast-resumed states, through both regular and publication-connection guards;
  task stays processing. Also ports the post-replacement invalidation/assertions
  from reconnect `test_replacement_invalidates_old_task_authority` using the real
  atomic replacement function, without executing provider credential verification.
- `controls_and_publication_guards_recheck_revocation_and_monotonic_clock`:
  covers source `test_controls_require_current_exact_authority_and_advancing_revision`
  actor/editor/tenant/owner/clock branches and archived/deleted dataset refusals;
  publication fence does not prevent control or metadata reads. The source
  `revoke_at_lock`/`change_at_lock` interleavings are not simulated.
- `guards_reject_binding_task_and_editor_grant_revocation`: checks binding owner,
  dataset and connection changes, task owner/status changes, and removal of a
  previously effective explicit editor grant, through both production guards;
  control and metadata readback also refuse the revoked grant.

Residuals: genuine COMMIT/network acknowledgement fault; concurrent mutation
between preflight and transaction lock; provider-verification-to-CAS race;
interleaving revocation with actual asset publication. These are not claimed
closed by the sequential real-function regressions.
