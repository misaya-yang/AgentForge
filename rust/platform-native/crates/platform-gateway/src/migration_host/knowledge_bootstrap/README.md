# Knowledge document backend configuration

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
