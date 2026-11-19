# KB retrieval baseline evidence (PRD T0-#3)

Candidate real-corpus retrieval distributions are recorded with
`make kb-baseline-record KB_BASELINE_DATASET_ID=<id>` against a live
knowledge service. Each run produces one `kb-golden-v1-<date>-runN.json`
report (hit-rate / MRR / nDCG / recall@k distributions over
`tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl`, retrieval track,
recorded via `scripts/regen_rag_observations.py`), joined with the exact
expectations/observation hashes in its `provenance` block.

Discipline (agent-kb-eval): the first round records the distribution only —
`--min-*` thresholds are passed as 0 and gate status is not the point. A
report remains a candidate until a human approves it, the report carries the
release metadata below, a hash-bound `release-pointer.json` is added, and
`make kb-release-evidence-gate` passes. Every later quality claim cites the
approved report file name.

No baseline is frozen yet: recording requires a dataset whose real corpus the
golden segment ids are bound to (the committed set still ships synthetic
segment ids — see `tests/fixtures/eval/rag/golden/manifest.json` notes).

The gate expects the reviewed report to retain the normal `scripts/eval_rag.py`
`provenance` and `retrieval.all.metrics` fields and add:

```json
{
  "release_evidence": {
    "schema_version": "kb-baseline-evidence/v1",
    "dataset_id": "the-live-corpus-dataset-id",
    "golden_version": "the-manifest-version",
    "golden_manifest_sha256": "64-lowercase-hex",
    "review": {
      "status": "approved",
      "reviewer": "stable-reviewer-id",
      "reviewed_at": "ISO-8601 timestamp"
    }
  }
}
```

`release-pointer.json` is a separate, immutable binding. It must contain
`schema_version: kb-release-evidence/v1`, `release_key`, `golden_version`,
repository-relative `golden_manifest` and `baseline_report` paths, SHA-256 for
both files, the same `dataset_id`, and an independent approved `review` object.
Golden rows require structured `metadata.provenance` with `kind` (`real` or
`synthetic`) and `source_ref`, plus an approved per-row `metadata.review`.
The release mix permits 40–60% real cases, matching the PRD's approximately
half-real/half-synthetic requirement. Do not hand-edit hashes before review;
the gate only verifies evidence and never creates or promotes it.
