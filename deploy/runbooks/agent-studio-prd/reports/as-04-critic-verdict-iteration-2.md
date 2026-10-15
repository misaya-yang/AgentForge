# AS-04 Independent Critic Verdict — Iteration 2 (Preserved)

**Phase:** AS-04 — Skills and Knowledge Version Bindings  
**Feature:** AS-F005  
**Critic:** independent fresh-context reviewer  
**Critic Verdict:** `changes_requested`  
**Actor Report:** `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md`  
**Prior Verdict:** `docs/agent-studio-prd/reports/as-04-critic-verdict-iteration-1.md`  
**Date:** 2026-07-19

## Decision

AS-04 / AS-F005 is not approved on this frozen source. The iteration-1
same-count content-drift defect, AS04-C01, is corrected in its original scope:
an authoritative `content_revision` now advances for the covered
retrieval-content mutations, the authorized catalog emits a strict fingerprint,
missing fingerprints fail closed, and the next-run hash regression passes.
The Skill persistence, isolation, exact-version, entrypoint, permission and
revocation paths also have credible independently rerun evidence.

Three release-blocking Knowledge contract gaps remain:

1. the purported retrieval-effective Dataset fingerprint omits the Dataset
   retrieval configuration that the Knowledge service actually executes;
2. pure Document processing telemetry advances the content revision, while the
   Assistant run hash separately incorporates mutable telemetry and counts; and
3. normalized per-Dataset retrieval configurations are accepted and sealed, but
   the Gateway Snapshot silently keeps only the first binding's three scalar
   fields and the Assistant drops image retrieval configuration.

These failures affect R3, AS-KB-001/003, the AS-F005 Knowledge Oracle step and
the exact provenance acceptance gate. AS-F005 must remain `failing`, AS-04 must
remain unapproved, and AS-05 must remain locked until C02-C04 are corrected on a
new frozen source and independently reviewed.

## Inputs Reviewed

- Phase contract:
  `docs/agent-studio-prd/phase-04-skills-and-knowledge-version-bindings.md`.
- Fixed implementation plan:
  `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-plan.md`.
- Oracle item AS-F005 in `docs/agent-studio-prd/feature-oracle.json`; it remained
  `failing` with empty evidence at review time.
- Iteration-2 Actor report, iteration-1 Critic verdict and durable golden
  `reports/agent-studio/as-04-skill-kb-golden.json`.
- Product and architecture contracts for exact Skill versions, normalized
  Knowledge bindings, per-binding retrieval configuration, live-content
  provenance and non-replayability.
- Migrations 037, 071, 075 and 076, including immutable Skill versions,
  composite tenant references, normalized bindings and the new Dataset content
  revision triggers.
- Skill parser, models, artifact repository, API, scoped registry, executor,
  request-local runtime overlay, Assistant bridge and capability resolver.
- Agent save/publish/run Knowledge resolver, Gateway Snapshot construction,
  Assistant runtime mapping and streaming provenance path.
- Knowledge Dataset catalog, update path, retrieval implementation, ingestion
  status/reindex path and production database persistence methods.
- All six mandatory iteration-2 commands, plus provider-free focused
  reproductions for C02-C04.

This Critic did not edit source, tests, migrations, the Actor report, golden,
Oracle, loop state, handoff, progress or continuity artifacts. The only write is
this canonical verdict.

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| AS04-C02 | high, release-blocking | R3; AS-KB-003; AS-F005 step 3; current live-content/retrieval provenance | `_dataset_revision_fingerprint` claims to hash retrieval-effective configuration but omits `index_config.retrieval` and other safe retrieval-effective Dataset configuration. Changing the actual mode, threshold, candidate policy or reranker can change retrieval while producing the identical fingerprint. | Define a canonical, secret-free projection of every retrieval-effective Dataset configuration field and include it in the authorized fingerprint. Add a regression that changes only retrieval-effective configuration while content revision and other fields remain fixed and proves the fingerprint and next-run provenance hash change. |
| AS04-C03 | high, release-blocking | R3; AS-KB-003; authoritative content revision; telemetry exclusion spotlight | Migration 076 treats almost every Document column change as content. Production same-status progress, error and processing timestamps therefore advance `content_revision` even when retrieval content is unchanged. `AgentLoop` also hashes Dataset `updated_at` and document/segment counts in addition to the authoritative fingerprint. The value labelled as live-content provenance is consequently polluted by operational telemetry. | Restrict revision advancement to an explicit retrieval-effective Document/Segment projection, excluding all pure processing/derived telemetry while retaining content, enable/archive/delete and other retrieval-effective mutations. Build the run hash from bound IDs, sealed retrieval config and authoritative fingerprints rather than mutable telemetry/counts. Add production-shaped progress/error/timestamp and count regressions. |
| AS04-C04 | high, release-blocking | R3; AS-KB-001/003; normalized stable Dataset IDs/config; `knowledge-binding` gate | Draft/Version rows preserve a `retrieval_config` per Dataset, but `_build_snapshot` takes only `knowledge_rows[0]`, copies only `mode`, `top_k` and `threshold`, and discards every other Dataset's configuration. `include_images` is dropped even for one binding, and the Assistant runtime mapper has no corresponding assignment. Accepted and version-sealed configuration is therefore not necessarily executed. | Carry and apply sealed retrieval configuration per Dataset end-to-end, including image retrieval, or explicitly reject non-uniform configurations under a documented single-global-config contract. Add multi-Dataset and image-config regressions proving accepted Version configuration reaches Assistant retrieval. |

### AS04-C01 disposition

AS04-C01 from iteration 1 is closed in its original scope:

- Migration 076 adds `datasets.content_revision` and statement-level transition
  triggers. A same-count Segment text edit advances the owning Dataset once;
  bulk changes coalesce per affected Dataset; Segment `updated_at`/`hit_count`
  changes are excluded.
- The Knowledge catalog omits its fingerprint when an authoritative revision is
  missing. `AgentLoop` accepts only an exact lowercase `sha256:` plus 64 hex
  characters for every bound Dataset and otherwise emits
  `AGENT_KNOWLEDGE_UNAVAILABLE` before model-provider execution.
- The required database, catalog and Assistant regressions passed, including a
  same-count edit, a changed next-run hash and
  `historical_replayable=false`.

C02 and C03 do not reopen the original same-count defect. They show that the
new authoritative provenance boundary is incomplete in two additional
dimensions: it misses executed configuration changes and reacts to changes
that are only telemetry.

### AS04-C02 proof: executed retrieval config is absent from the fingerprint

1. `_dataset_revision_fingerprint` hashes only `dataset_id`,
   `content_revision`, embedding provider/model/dimension, `needs_reindex` and
   collection name
   (`apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py`,
   lines 30-53). It does not include `index_config` or a safe projection of
   `embedding_config`.
2. `DatasetService.update_dataset` accepts and persists both `index_config` and
   `embedding_config` without advancing `content_revision` for an index-only
   update (same file, lines 223-295).
3. The actual retrieval service consumes `index_config.retrieval`, including
   enforced/locked configuration, retrieval mode, fusion weights and method,
   vector/keyword/candidate pools, score threshold, reranker provider/model,
   MMR and related policy
   (`apps/knowledge-service/src/knowledge_service/services/knowledge/retrieval_service.py`,
   lines 230-383). These values can change returned evidence without changing
   content rows.
4. The Critic ran a provider-free reproduction against the actual fingerprint
   helper. It held revision, embedding and collection fields constant, then
   changed only `index_config.retrieval` from hybrid/threshold 0.2/rerank off to
   BM25/threshold 0.9/rerank on. Exit was 0 and the exact result was:

   ```text
   {'first': 'sha256:4f5829b30ba016ce5c97b23bbe40c81d49605ce2b3d45fe0af5371451d3c5292', 'second': 'sha256:4f5829b30ba016ce5c97b23bbe40c81d49605ce2b3d45fe0af5371451d3c5292', 'hash_equal': True}
   ```

5. `test_catalog_fingerprint_changes_with_authoritative_content_revision`
   changes only `content_revision`; it never changes retrieval-effective
   configuration. Its green result therefore does not cover C02.

The correction must never serialize API keys, credentials or secret-bearing
configuration into a fingerprint or trace. It needs an explicit canonical
allowlist of retrieval-effective, non-secret values.

### AS04-C03 proof: processing telemetry changes content provenance

1. Migration 076 excludes only `updated_at` from Document row comparison
   (`database/migrations/076_agent_knowledge_content_revision.sql`, lines
   52-73). Production `documents` also contains processing fields such as
   `status`, `progress`, `error`, `started_at`, `completed_at` and derived
   counts; changes to any of those currently advance the Dataset revision.
2. `DatabaseStorage.update_document_status` updates exactly those processing
   fields (`apps/knowledge-service/src/knowledge_service/persistence/database.py`,
   lines 1982-2021). Ingestion repeatedly writes the same `embedding` status
   with a changing progress percentage for every batch
   (`apps/knowledge-service/src/knowledge_service/services/knowledge/ingestion_service.py`,
   lines 659-663), independently of the Segment/vector content writes.
3. The Critic applied the actual migration in an isolated temporary PostgreSQL
   schema with a production-shaped Document table, performed a progress-only
   update, an `updated_at`-only update, a Segment insert and a `hit_count`-only
   update, and removed the schema afterward. Exit was 0 and the exact result
   was:

   ```text
   {'after_document_insert': 1, 'after_progress_only': 2, 'after_timestamp_only': 2, 'after_segment_insert': 3, 'after_hit_only': 3}
   ```

   Thus `progress=35` to `progress=45`, with status and retrieval content held
   constant, advanced the purported content revision.
4. Independently, `AgentLoop._get_streaming_dataset_context` includes catalog
   `updated_at`, embedding fields, `needs_reindex`, collection name and
   document/segment counts alongside `revision_fingerprint` when computing the
   run hash
   (`apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`,
   lines 2723-2766). Those fields permit additional non-authoritative hash
   movement even after trigger semantics are corrected.
5. The required migration fixture uses a reduced Document schema and tests
   Document `updated_at` plus Segment `hit_count`; it does not contain or vary
   the production progress/error/timing columns. The required catalog test
   changes only `content_revision`. The green suite does not cover this case.

False-positive provenance is not merely cosmetic. It makes eval/audit drift
receipts attribute a corpus/configuration change where only processing
bookkeeping changed, undermining the authoritative label and making genuine
drift harder to localize.

### AS04-C04 proof: accepted per-Dataset config is silently discarded

1. The architecture contract defines `retrieval_config` on both Draft and
   Version Knowledge binding rows and says a Version fixes Dataset IDs and
   retrieval parameters (`docs/agent-studio-prd/architecture-contract.md`,
   lines 62-65 and 258-262). AS-KB-001 explicitly supports one or more
   Datasets plus mode, Top K, threshold and image retrieval
   (`docs/agent-studio-prd/product-requirements.md`, lines 132-136).
2. The repository normalizes and seals each binding's configuration, and reads
   Version rows ordered by Dataset ID. Yet `_build_snapshot` selects only
   `knowledge_rows[0]` and emits a single object containing only `mode`,
   `top_k` and `threshold` (`src/api/v1/agent_runtime.py`, lines 452-501).
3. `_build_agent_runtime_config` maps only that global mode/Top K/threshold to
   `AssistantConfig` and never assigns `kb_include_images`
   (`apps/assistant-service/src/assistant_service/api/routes/chat.py`, lines
   346-460).
4. The Critic built a provider-free Snapshot from two authorized bindings:
   Dataset A used `{mode: tool, top_k: 4, threshold: 0.2}` and Dataset B used
   `{mode: auto, top_k: 17, threshold: 0.85, include_images: true}`. Exit was 0
   and the material output was:

   ```text
   {'datasets': ['dataset-a', 'dataset-b'], 'runtime_retrieval': {'mode': 'tool', 'top_k': 4, 'threshold': 0.2, 'provenance': [{'dataset_id': 'dataset-a', 'content_mode': 'live_latest', 'historical_replayable': False, 'revision_source': 'assistant_run_catalog'}, {'dataset_id': 'dataset-b', 'content_mode': 'live_latest', 'historical_replayable': False, 'revision_source': 'assistant_run_catalog'}], 'replayability': 'live_content_provenance_only'}, 'dataset_b_config_present': False}
   ```

5. The required Snapshot test uses one Dataset and checks only provenance and
   replayability. The two-Dataset resolver test checks complete ACL denial, not
   successful runtime configuration. Neither proves per-binding execution or
   image retrieval propagation.

Silently accepting configuration that runtime does not honor is an honesty and
version-binding failure. A deliberately global schema can be valid only if the
write/publish contract rejects incompatible per-Dataset values instead of
discarding them according to sort order.

## Requirement and Oracle Coverage

| Contract item | Critic assessment |
| --- | --- |
| R1 honest Skill persistence and tenant/user isolation | Locally covered. Repository and API queries are tenant/user scoped; database errors surface as failure; valid uploads receive server-owned `db://` identities; forged executable/source schemes are rejected. The required 26-test gate passed. |
| R2 exact Skill version execution | Locally covered. Full canonical content/hash and exact immutable Version IDs are loaded request-locally; old versions do not drift; global registries remain unmodified; Skill permissions remain below the AS-02 capability allowlist. The required 6-test gate passed. |
| R3 authorized Knowledge binding and live provenance | `changes_requested`. Normalized rows and save/publish/run ACL checks are covered, and C01 is fixed, but C02 makes the revision fingerprint insensitive to executed retrieval configuration, C03 makes it sensitive to telemetry, and C04 prevents sealed per-Dataset configuration from reaching runtime. |
| R4 fail-closed revocation and degradation | Locally covered for missing, deleted, disabled and revoked Skill/Dataset cases. Bound Knowledge with a missing/invalid fingerprint emits `AGENT_KNOWLEDGE_UNAVAILABLE` before provider execution; historical replay remains explicitly false. These strengths do not waive R3. |
| AS-F005 steps 1, 2 and 4 | Exact Skill content/version, same-name isolation, capability upper bounds and disable/revoke/unavailable behavior have supporting local evidence. |
| AS-F005 step 3 | Fails C02-C04. Normalization/ACL and same-count content revision are covered, but the stored configuration is not faithfully executed and the provenance value does not cleanly represent content plus retrieval-effective configuration. |
| Golden `live-knowledge-provenance` and overall `status=passed` | Not acceptable as phase completion evidence on this source. Its cited tests do not vary retrieval-effective Dataset configuration, production Document telemetry or successful heterogeneous multi-Dataset/image runtime configuration. |

## Independent Test and Regression Assessment

The Critic reran all six mandatory iteration-2 commands in the specified order.
No required gate was skipped.

| Gate | Independently rerun command | Exact result |
| --- | --- | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | exit 0; 26 collected; `26 passed, 1 warning in 0.39s`; zero skips |
| Skill runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | exit 0; 6 collected; `6 passed, 1 warning in 0.44s`; zero skips |
| Knowledge binding/streaming | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | exit 0; 37 collected; `37 passed, 1 warning in 0.52s`; zero skips |
| Exact AS-04 lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | exit 0; `All checks passed!` |
| PostgreSQL migration contract | `uv run pytest -q --no-cov tests/database/test_agent_skill_knowledge_migration.py` | exit 0; 4 collected; `4 passed, 1 warning in 0.91s`; zero skips |
| Knowledge revision fingerprint | `uv run --package knowledge-service pytest -q --no-cov tests/services/knowledge/test_dataset_revision_fingerprint.py` | exit 0; 3 collected; `3 passed, 1 warning in 0.50s`; zero skips |

Across five pytest invocations, 76 tests passed with zero skips. Each reported
the same Starlette TestClient deprecation warning; no warning was represented as
a skip or failure. These results prove only the assertions exercised. The
focused C02-C04 reproductions vary inputs that the mandatory tests do not.

### Evidence boundary

The Critic personally reproduced only the six command results above, the
provider-free C02 and C04 reproductions, and the temporary isolated PostgreSQL
C03 reproduction. The Critic did not independently reproduce Actor claims
about authenticated live Skill CRUD, repository-owned Compose 8/8 health,
approximately 730 MiB stack memory, the live rollback revision probe, migration
tracker state, hot-source identity, AHR 31/77/8/98 aggregates, the adjacent
60-test Assistant bundle, the 74-test Docker/open-source bundle or any external
provider/production Knowledge system. Those remain Actor-reported receipts and
are not used to close C02-C04.

No browser evidence was required because AS-04 must not add Studio UI. No
Docker, live provider, deployment, commit or push action was performed. No
secret, provider key or generated `.env` value was printed or written.

## Security, Privacy, and Failure Assessment

- Tenant Skill uploads remain instruction-only, receive server-owned
  database identities, and cannot create executable/bundled artifacts or
  authorize tools beyond the immutable Agent capability upper bound.
- Exact Skill and Dataset authorization/revocation is rechecked at the reviewed
  save, publish and run boundaries. Missing catalog fingerprints fail closed
  before provider execution, and accessible but unbound Datasets are filtered.
- C02 is a provenance-integrity defect. The fix must use a secret-free
  configuration projection so credentials never enter hashes or traces.
- C03 does not expose data, but it creates misleading audit/eval evidence by
  labelling operational progress as content drift.
- C04 can silently change effective behavior according to Dataset sort order
  and can disable an explicitly sealed image-retrieval choice. Runtime must
  reject ambiguity or preserve the accepted configuration exactly.

## Required Remediation and Re-review

1. Extend the authorized Dataset fingerprint with a canonical allowlist of all
   retrieval-effective, non-secret configuration. Prove a config-only change
   changes the catalog fingerprint and the next-run provenance hash; retain
   missing-revision fail-closed and `historical_replayable=false`.
2. Replace broad row-JSON trigger comparisons with explicit
   retrieval-effective field projections. Add production-shaped regression
   columns and prove progress, error, processing timestamps, hit counters and
   derived counts do not advance content revision, while content, enablement,
   archive/delete and successful retrieval-effective reindex changes do.
   Remove mutable telemetry/counts from the Assistant revision hash.
3. Preserve and execute retrieval configuration per bound Dataset, including
   images, or validate one explicit global configuration at save/publish and
   reject heterogeneous rows. Add successful multi-Dataset and single-Dataset
   image cases through Snapshot and Assistant configuration.
4. Rerun all six commands above plus the new focused regressions. Amend the
   Actor report and golden only with results from the corrected frozen source,
   then request a fresh independent Critic iteration.

These corrections are narrow contract work. They do not authorize a general
Knowledge indexing/retrieval rewrite, Studio UI, AS-05 implementation,
deployment or production migration.

## Minimal-Change Assessment

The checkout contains extensive inherited modifications across earlier phases
and unrelated work, so a clean phase-only Git diff cannot be reconstructed from
the current worktree. The reviewed AS-04 implementation stays largely within
the intended persistence, resolver, Knowledge contract and Assistant runtime
surfaces. No AS-05 UI or publication work is required to correct C02-C04.

The minimal viable correction is a safe canonical fingerprint projection,
precise trigger/hash semantics, and an honest per-Dataset-or-explicitly-global
runtime configuration contract with focused regressions. It must not become a
general Knowledge algorithm refactor.

## Rollback and Handoff Assessment

Immutable Skill and Knowledge references remain preserved while current
disable/revoke/delete state overrides execution. The reviewed failure path
stops before provider use when a bound Skill or Dataset is unavailable, and
historical Skill content is retained. Those are sound rollback properties but
do not repair C02-C04.

The canonical transition remains:

- AS04-C01: closed in its original iteration-1 scope.
- AS04-C02, AS04-C03 and AS04-C04: open, release-blocking.
- AS-F005: keep `failing`; do not write passing evidence.
- AS-04: keep active/pending remediation; do not record Critic approval.
- AS-05: keep locked.
- Preserve the iteration-1 verdict and this verdict as immutable review
  history when a later Critic iteration is produced.

## Whole-Demand Regression Assessment

Terminal same-build whole-demand regression belongs to AS-09 and remains
pending. The explicitly obsolete whole-Harness strict migration diagnostic was
not run, consistent with the request's waiver; no required AS-04 test,
security, provenance or independent-Critic gate was skipped. Actor-reported
aggregate or live receipts cannot waive the three current AS-04 acceptance
failures.

## Verdict Rationale

Iteration 2 materially repairs the original silent same-count content drift and
retains strong Skill isolation, exact-version and fail-closed behavior. But the
new fingerprint is unchanged by executed Dataset retrieval configuration,
changes on pure processing telemetry, and is attached to a runtime Snapshot
that silently discards accepted per-Dataset/image configuration. Those
behaviors prevent the recorded value from being an authoritative receipt of
the live content and configuration actually used. Because exact Knowledge
configuration/provenance is non-waivable and release-blocking, the only
evidence-supported verdict is `changes_requested`.
