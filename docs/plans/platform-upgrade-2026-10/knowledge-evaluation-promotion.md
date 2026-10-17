# Knowledge evaluation and promotion implementation

Owns the U-KN-11 implementation in [04-knowledge.md](04-knowledge.md#48-evaluation-and-promotion-u-kn-11).
The native `knowledge-eval-gate` binary consumes captured observations and uses existing
`retrieval_eval::score_single_query` and `parsing_eval::score_document`. It makes no network,
provider, model-download or database calls and does not activate a serving profile.

## Current evidence boundary

The existing `tests/fixtures/eval/rag/golden/manifest.json` pins synthetic segment IDs and an
unreviewed machine-generated bilingual starter corpus. These are reused for **gate contract tests**,
not a real-quality baseline. `tests/fixtures/eval/knowledge/promotion/` explicitly labels every
measurement and human/judge label as fixture. A positive fixture returns `promotable: false`.
No production baseline or calibration score is invented here. Real text, scanned, table and
multilingual retrieval/parsing corpora, reviewed labels, captured baseline/candidate runs and
quality acceptance remain open. Full tree-edit TEDS is also open: the existing metric is explicitly
reported as `table_similarity_teds_lite`.

## Run commands

Primary build owner only; external Cargo target, one job, no debug/incremental output:

```sh
CARGO_TARGET_DIR=/Users/yang/.cache/agentforge-upgrade/knowledge-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo +1.99.0 test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-ops --test knowledge_promotion
CARGO_TARGET_DIR=/Users/yang/.cache/agentforge-upgrade/knowledge-target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo +1.99.0 build --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-ops --bin knowledge-eval-gate
/Users/yang/.cache/agentforge-upgrade/knowledge-target/debug/knowledge-eval-gate evaluator-sha256
/Users/yang/.cache/agentforge-upgrade/knowledge-target/debug/knowledge-eval-gate . reports/knowledge/quality/promotion-policy.json reports/knowledge/quality/candidate.json MEASURED_SOURCE_SHA
```

Fixture tests freeze the review clock at 2026-10-17 and exercise positive metrics, critical-case
regression, reversed judge calibration, fixture-to-production refusal, source/evaluator/dataset
identity, missing profiles, corrupt evidence hash and stale human review. They are gate tests,
not measured product quality. Running the CLI with `--fixture-only` can only produce a fixture
decision; its human-review clock still uses the current date.

## Evidence contract

Use the existing golden JSONL shape for retrieval (`case_id`, `track: retrieval_only`, `relevance`).
The gate selects the retrieval-only track and requires an exact case-ID join with observations;
failed/skipped, missing, duplicate and extra cases are rejected. Retrieval observations are JSONL:

```json
{"case_id":"LIVE_GOLDEN_CASE","status":"succeeded","ranked_segment_ids":["ACTUAL_SEGMENT_ID"]}
```

Parsing golden JSONL carries `case_id` and `document` (the existing serialized DocIR). Parsing
observations carry `case_id`, `status: succeeded` and the actual parser `document`. Preserve the
original captured responses separately when creating these files; do not infer ranks or DocIR from
the oracle. The gate recomputes metrics, never accepts caller-supplied aggregate quality scores.

A policy (`knowledge-promotion/v1`) fixes:

* `baseline`: `{path, sha256}` of a `knowledge-measurements/v1` receipt;
* `evaluator_sha256`: output of the binary's identity command. It covers the exact retrieval metric,
  parsing metric and promotion-rule source, including gate thresholds. A changed evaluator requires
  an explicitly established new measured baseline;
* `suites`: `{id, kind, dataset_version, golden: {path, sha256}, label_provenance, corpus_class, profiles}`.
  Kinds are `retrieval` or `parsing`. Production labels require `human_reviewed`; every kind must
  include the `text`, `scanned`, `table` and `multilingual` classes;
* `human_labels`: pinned JSON `{provenance: human_reviewed, reviewer, reviewed_at, labels: {case_id: score}}`;
* `judge_model` and `judge_prompt_sha256`. The latter must match the deployed RAGAS judge's
  `judge_prompts_sha256()` in production, rather than a caller-invented prompt version.

The baseline and candidate receipts have fields `schema`, `provenance: measured`,
`collector: knowledge-native-capture/v1`, `source_sha`, `evaluator_sha256`, `profiles` and `calibration`.
Each profile is `{suite, profile, version, configuration: {path, sha256}, dataset_version,
golden_sha256, observations: {path, sha256}}`. Configuration artifacts contain the actual full
retrieval/parser profile and dataset/index/model identity used during capture. Changing its bytes
requires a new profile version. The exact required profile set must occur once in each run.
Calibration is `{judge_model, prompt_sha256, scores: {path, sha256}}`; scores are actual judge
outputs keyed by the same human-labelled case IDs. Include provenance/model/input receipts with
the captured artifacts for independent review. Provenance strings and hashes are assertions and
integrity checks; they cannot prove that an operator genuinely ran a model or reviewed labels.

All artifact paths resolve within the supplied evidence root and all bytes must match their pins.
No credentials, raw private tenant captures or DSNs belong in tracked receipts. For confidential
corpora run the identical gate in the authorized evidence environment; repository CI requires
sanitized, reviewable evidence at the canonical reports paths to approve a repository promotion.

## Decision and CI

Every case in every configured profile must preserve Recall@20, nDCG@10 and MRR, or parsing text,
TEDS-lite, formula, reading-order and page/block coverage. CER must not increase. This deliberately
stronger per-case rule protects critical cases and prevents aggregate gains hiding losses.

Judge calibration requires at least 30 human-reviewed cases, both human outcome classes,
review within 92 days, MAE <= 0.15, binary agreement >= 0.8 and Cohen's kappa >= 0.6 at threshold
0.5. Out-of-range, nonfinite, missing, stale or misidentified scores fail. Two synthetic samples
are permitted only in fixture mode, which cannot promote.

`.github/workflows/knowledge-evaluation.yml` runs gate positive/negative tests and
`scripts/checks/knowledge-profile-promotion.sh BASE_SHA`. The shell wrapper detects Knowledge
profile/evaluator/evidence changes, takes the production policy from the trusted base Git revision,
and requires `reports/knowledge/quality/candidate.json`. It checks that the captured source commit
is an ancestor and that no Knowledge/foundation source or Cargo identities changed after capture;
receipt-only commits can follow. Then it runs the native gate without fixture mode. Missing baseline
or candidate evidence is a blocking result, not a skipped/passing quality check. Establish and review
the first real baseline in a dedicated evidence step before any serving profile promotion.

CI setup discovers the new `src/bin/knowledge-eval-gate.rs` through Cargo's existing automatic binary
convention. No workspace, dependency or lock changes are required. Production activation remains a
separate authorized step after the quality decision and original release gates.
