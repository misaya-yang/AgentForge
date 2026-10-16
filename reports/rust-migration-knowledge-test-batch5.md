# Knowledge embedding regression batch 5

New `knowledge_migration_parity_embedding` integration target has five tests
against real native HTTP embedding adapters and isolated loopback fixtures.
No external endpoint, DB, credential store or model loading is involved.

## Exact covered source assertions

`test_embedding_query_cache.py`:
- Same query with512/1024dimensions returns respective lengths and one provider
  call per identity; repeat query remains cached.
- Returned query vectors are copies and caller mutation does not poison cache.
- Query/document requests keep distinct text_type and document calls do not
  accidentally consume a cached query vector.
- Two configured endpoints/keys receive their own request identity/dimension.
- Text and multimodal dimension mismatches are refused; multimodal configured
  dimension is actually forwarded to the provider.

Residual source assertions: process-global equivalent-instance cache reuse and
cache-object identity, credential/profile fingerprint representation, semantic
configuration equivalence including timeout/unused instruct, and local SDK
private-field assertions. These are not claimed as passed or whole-file parity.
The native cache is manager-owned and clones share it; separate managers do not
claim the old global object identity.

`test_embedding_batch_contract.py`:
- Both DashScope and unified multimodal production image batch functions fail
  when the second image fails, rather than returning a shortened vector list.
- The native provider error is sanitized; exact legacy private exception text
  ('image1' / 'simulated provider failure') is not claimed as equivalent.
  Whole-file source-assertion migration therefore remains partial.

## Shared endpoint wiring and warning cleanup

Both actual Knowledge process constructors now call the Foundation-owned
ChatEndpointResolver with EndpointDomain::Embedding/Ocr. Explicit nested
Knowledge embedding overrides retain downstream precedence. The helper uses
per-domain key/base before generic values and normalizes native API suffixes.
No duplicate endpoint resolver or dependencies were added. Removed only genuine
unused ingestion imports: parser Write, store Row, request-validation local
FromRequestParts. Root owns compilation and execution.

## Exact schema suite

Added three unit tests in ingestion request-validation's test-only child
`knowledge_migration_parity_schema`. They exercise the actual HTTP validators:
all original roundtrip fields (including child_chunk_overlap and segmentation),
all four unsafe field cases, and exact255/256archive-reason bounds. This covers
all original assertion groups in `test_chunking_config_schema_roundtrip.py`.
Gateway owner confirmed no overlap. Whole-file retirement eligibility awaits
root compile/pass; no production schema implementation was changed.

## Process configuration assertions

`knowledge_migration_parity_process` adds four tests through real
ProcessSettings::from_values and ProviderKeys::qa_key. It covers the original
QA mixed-case Aliyun alias, primary/empty/generic precedence, DeepSeek/Google
fallbacks, provider isolation/custom absence, per-call supplied snapshots,
LLM_MODEL including explicit empty, and both production domain endpoint
assignments with nested embedding override preservation. This partially maps
`test_settings_env_consolidation.py`; its full34variable/default matrix,
dotenv distinction, reranker/read-time wrappers and documentation assertions
remain. No whole-file retirement claim.
