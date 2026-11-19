# Knowledge document backend configuration

The standalone `platform-knowledge-service` crate owns this process profile. Gateway forwards signed HTTP requests and does not construct Knowledge business routers.

## PDF admission and readiness

Text, Markdown, HTML and DOCX ingestion remain available without a PDF engine.
Without an explicitly selected PDF text backend, both single and batch uploads
reject a PDF synchronously, after existing dataset authorization and before
creating its document row or storing its source. Detection uses the filename,
MIME type and `%PDF` signature, matching extraction. Rejected batch entries are
retry-safe; other files retain the existing partial-success behavior.

The existing `/health` and `/health/ready` response gains `capabilities` with
`upload_formats` and `pdf_text` (`configured`, `status`, `startup_probe_passed`).
Absent PDF is reported as `disabled` and does not make core readiness fail.
A selected backend is probed through real, bounded extraction of one tiny
built-in page at startup, before workers and readiness. An invalid executable,
extraction failure or unexpected output fails startup with the redacted stage
`configured_pdf_backend_probe`; construction may reject invalid executable
paths earlier. This is a startup check, not continuous proof of binary integrity.

To select the already-supported Poppler alternative, provision the tools through
normal operator installation and supply these existing process settings:

- `KNOWLEDGE_NATIVE__ALLOW_POPPLER_ALTERNATIVE=true`
- `KNOWLEDGE_NATIVE__PDFINFO=/absolute/path/to/pdfinfo`
- `KNOWLEDGE_NATIVE__PDFTOTEXT=/absolute/path/to/pdftotext`
- `KNOWLEDGE_NATIVE__PDFTOPPM=/absolute/path/to/pdftoppm` only when page rasterization is required
- `KNOWLEDGE_NATIVE__SCRATCH_ROOT=/absolute/private/scratch/directory`

There is no executable discovery, download or automatic opt-in. API and worker
processes must use the same selected backend profile; API-only startup also
probes its local engine so it cannot advertise an unconfigured PDF path.
Poppler remains an explicit alternative: text ordering, font decoding and
malformed-file behavior can differ, and its process memory/output bounds do not
establish pypdf's per-decompressed-stream limit. Embedded xref image extraction
and PDF page serialization remain unavailable. Strict legacy decoder parity
requires a separate accepted implementation decision and verification.

## Vision asset URLs

A configured raster backend can construct the existing vision processor without
a second URL callback. After verifying and storing the owner-scoped page object,
the processor calls that same `AssetStorage.image_url` authority asynchronously.
S3/OSS presigning, configured URL expiry and required local HMAC signing retain
one owner. A missing local signing key or signing failure fails explicitly; no
unsigned, guessed or public URL is substituted. Original-source scope checks,
object digests and candidate generation checks remain in place.

This only repairs the existing backend assembly. Public scanned/multimodal upload
and dataset-profile restrictions remain unchanged. No new mode or permission is enabled. No provider request or production
deployment was executed while preparing this change.

The proposal has not been built, tested or run against Poppler. Required targeted
acceptance: missing-engine PDF rejection creates no row/object and leaves text
uploads usable; mixed batches reject only PDF; configured-engine startup succeeds
only after the page probe; failed probe prevents readiness; local URLs verify
with the existing expiry/HMAC checks; cloud URLs use the configured backend; and
vision owner/generation/digest failures remain closed. Full migration regression
and live document/provider acceptance remain separate release gates.

### Split Valkey coordination and retrieval cache

The process parses `AI_PLATFORM_VALKEY_*` through the shared typed `SplitConfig`.
With no split variables the existing legacy coordination configuration remains.
Any partial split configuration, unexpected split variable, incorrect service ACL
username, or non-TLS endpoint is rejected; legacy Redis is never a split fallback.
`KNOWLEDGE_RUNTIME_ROLE=worker` selects `knowledge-worker`; API/all selects
`knowledge-api`. Supply both COORD and CACHE `ENDPOINT`, `USERNAME`,
`PASSWORD_FILE`, `CA_FILE` plus `AI_PLATFORM_VALKEY_MODE=split`; endpoints use
`rediss://host:port/0`. Shared validation requires controlled credential and CA
files and verifies Valkey version, policy and authenticated user.

In split mode replay/idempotency adapters receive only the coordination handle.
Coordination connection failures fail startup. The optional result tier receives
only `CacheConnection` through `KnowledgeBackends.result_cache`; cache transport,
timeout or pressure failures produce an explicit warning and permit local-only
cache operation. Cache authentication, ACL, policy and malformed-value failures
fail the operation rather than becoming ordinary misses. There is no cache-to-
coord fallback or extra daemon/pool.

The bounded weighted Moka tier remains first. A miss may read the independently
configured cache endpoint using a typed tenant/service-scoped Retrieval key. The
existing full actor/ACL, dataset generation, model/profile and request fingerprint
is retained (including model-plane revision scope). Every hit still passes current
active IDs/hierarchy and ACL/publication-generation revalidation before return.
Remote values are bounded to 1 MiB and carry schema, tenant, fingerprint, original
admission timestamp and TTL. Their maximum lifetime is 300 seconds; shorter
configured TTLs apply immediately, and promotion to Moka preserves original age
so increasing TTL cannot resurrect expired entries. Invalid bindings fail closed.

Source regression coverage includes pure API/worker split parsing without file or
network I/O, partial configuration rejection, auth/policy versus outage error
classification, remote tenant/ACL/generation binding, corrupted/oversized values,
and original-TTL-preserving local promotion. Primary-writer verification targets:
`cargo test -p ai-platform-knowledge-domain --lib` and
`cargo test -p ai-platform-knowledge-service --all-targets`. Real split-instance
ACL/availability verification remains a separate runtime acceptance step.
