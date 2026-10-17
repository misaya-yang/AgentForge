# Parsing v2 implementation and integration seam

Production callers are mounted through `migration_knowledge_ingest::parsing_v2`.
This implementation requires no new dependencies, SQL schema or parser daemon.
The existing native parser, PDF backend, OCR and structured cascade remain intact.

Select a versioned profile explicitly in the existing pinned `index_config`:

```json
{"parsing_v2":{"version":2,"profile_id":"native-docir-v2-candidate","atomic_chunks":true}}
```

The optional strict `layout` object requires registered `provider`, `model`,
`revision`, `mode`, `max_output_tokens`, `deadline_seconds`, and
`allow_known_failure_native_fallback`. Unknown fields (including keys/endpoints)
are rejected. The full typed config determines the parser fingerprint. Existing
schema-1 `parsing` configuration and caches remain compatible and separate.

The standard worker retains actual structured DocIR instead of discarding it
following Markdown rendering. Large PDFs retain the actual already-decoded/OCR
PageContent for this profile, with a streamed SHA-256 of the owned source spool;
there is no second parser/model pass. PDF fallback uses the current streaming
backend's real page numbers. Decoded Office/text output records logical-page,
unknown-region semantics rather than inventing physical page coordinates.

`prepare` verifies current running persisted execution, exact tenant/dataset/doc,
persisted initiator, original content and authenticated actor, refreshes the actor
through its existing authority, then prepares pages. It records an unpublished
candidate prefix in both document metadata and the existing execution manifest.
The explicit profile chooses real table/formula-atomic chunks and section/citation
metadata. Unselected profiles and re-embedding retain their original paths.

Standard publication passes prepared pages into the existing version transaction.
Only after that transaction selects/creates/restores the actual document version,
`finalize` verifies the held negative publication fence, derives its checked final
revision, serializes a bounded (16 MiB) DocV2 object with the actual version ID,
uploads to a content-addressed key under the recorded execution prefix, and
verifies stored bytes. Version metadata, final segment citations and document IR
manifest become visible only with that same transaction. Failure rolls SQL back;
the object has no active reference, and its candidate prefix remains traceable.

Scanned/vision publication uses the same finalization inside its special version
transaction. Existing image candidates and object receipts remain intact; v2 adds
atomic text structures in that same candidate generation. Figure blocks retain
real full-page rendered dimensions and owned crop keys. Restored vision candidates
use their existing pinned page texts. No crop, OCR result or vector is fabricated.

Before a version exists, vectors carry a stable typed execution publication-ref.
`resolve_reference` resolves that committed execution manifest against an existing
real version, verifies object hash and source/page/region binding, and checks
current ACL/generation before and after I/O. Execution manifests preserve prior
publication references even when an unchanged document version is re-parsed with
a new profile. SQL segment metadata also contains the fully resolved citation.

Layout now has two server-configured modes. `layout.mode="explicit"` is an
operator-selected profile: standard PDF ingestion uses the existing real native
raster backend; scanned ingestion reuses its already-rendered, digest-verified
owned page crops. The evidence carries `measurements: None`, not fabricated
coverage ratios. Only real images with verified actor/resource authority and the
profile's model/deadline/image/output budgets can dispatch. Successful route
metadata records `explicit_operator_profile`. A 32 MiB total raster budget keeps
whole-document image retention bounded; per-image bounds remain in force.

`layout.mode="thresholds"` (the default) requires all three configured policy
thresholds and real PageMeasurements. The current native PageSignals port exposes
text length and image count, not area coverage. When those measurements are
absent, threshold mode explicitly keeps native results with
`measured_layout_signals_unavailable`; image counts never become invented ratios.
An explicit profile can omit the unused threshold policy fields. For example:

```json
{"parsing_v2":{"version":2,"profile_id":"layout-explicit-candidate","atomic_chunks":true,"layout":{"mode":"explicit","provider":"registered-provider","model":"registered-layout-model","revision":"registered-immutable-revision","max_output_tokens":8192,"deadline_seconds":60,"allow_known_failure_native_fallback":false}}}
```

Missing raster support, mismatched page identities or budgets fail closed when
fallback is disabled; permitted native fallback is recorded. Default profiles and
existing native/vision paths are unchanged. No additional service, model download
or provider credential is needed. Measured threshold-signal supply and real
CER/TEDS/reading-order quality acceptance remain open.

The typed DocV2 keeps paragraphs/headings/lists/code, table HTML+Markdown, formula
LaTeX and figure captions/crop references. Native metadata and original structured
payloads are retained. Unknown native kinds remain explicit Unknown structures;
missing native coordinates remain unknown, not fabricated. Known native geometry
keeps `native_page_units`; layout coordinates are explicitly render_pixels, with
original native geometry recorded separately. Cross-page tables stay separately
citable regions with an optional continuation ID; no synthetic bounding box spans
pages. Relative crop references may be bound to existing owned assets by the
publisher; external crop URLs and traversal are rejected.

Layout calls use only the existing Gateway model plane, ChatCompletions/Vision,
explicit registered revision and max_tokens, in a verified tenant/actor scope.
No vendor keys, direct vendor HTTP, parser subprocess or external parser service
is introduced. The prompt treats document input as data. Output must be JSON of
`docir-layout/v2`, with a matching page, unique IDs/orders, typed blocks, bounded
regions, safe table HTML and both table representations, and nonempty formulas.
Only a terminal stop and settled Gateway response reaches schema validation.
Unknown/transport/authorization errors propagate directly with no retry or native
fallback. Configured native fallback applies only after a known settled invalid
layout result or missing rendered input and requires usable original native text;
route metadata records the fallback instead of claiming layout success.

Budgets cover page count, document deadline, image/input text, output JSON,
blocks/page, total text, cells/table, formula size and output tokens. Publication,
object storage and execution fencing remain the caller's existing authority:
this module has no side effects other than the explicitly selected model call.

The test children cover real native UTF-8 decoding into v2, two-page table/formula
fixtures, citation version/generation/regions, routing measurements, safe HTML,
malformed/duplicate/cross-page/over-budget output, truncated completion, explicit
native fallback and missing verified actor. These are semantic regressions, not
measured parsing quality. Primary-writer verification:

```sh
cargo test -p ai-platform-knowledge-domain parsing_v2 --lib
```

CER/TEDS/reading-order evaluation on real scanned/table-heavy and born-digital
classes, promotion evidence, and measured threshold-signal supply remain
open. No quality improvement or activation is claimed by these fixtures.
