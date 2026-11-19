# Native Knowledge operator commands

`platform-knowledge-ops` replaces six Knowledge Python entrypoints. It is a separate operator crate, so evaluation/parser libraries are not production Knowledge service dependencies.

| Former script | Native subcommand |
| --- | --- |
| `import_kb_eval_golden.py` | `import-kb-eval-golden` |
| `build_rerank_bakeoff_cases.py` | `build-rerank-bakeoff-cases` |
| `reranker_bakeoff.py` | `reranker-bakeoff` |
| `chinese_lexical_shadow_eval.py` | `chinese-lexical-shadow-eval` |
| `migrate_sparse_vectors.py` | `migrate-sparse-vectors` |
| `backfill_bm25_v2.py` | `backfill-bm25-v2` |

Command-specific argument names retain the source scripts' flags. The native command comes first; backfill then takes `plan` or `run`. Examples:

```
platform-knowledge-ops build-rerank-bakeoff-cases --out cases.jsonl
platform-knowledge-ops reranker-bakeoff --cases cases.jsonl --adapters identity --out reports/offline
platform-knowledge-ops chinese-lexical-shadow-eval --cases cases.jsonl --out reports/lexical
platform-knowledge-ops import-kb-eval-golden cases.jsonl --dry-run
platform-knowledge-ops migrate-sparse-vectors --dry-run --collection kb_default_1024
platform-knowledge-ops backfill-bm25-v2 plan --collection COLLECTION --tenant-id TENANT --dataset-id DATASET
platform-knowledge-ops backfill-bm25-v2 run --manifest manifest.json --dry-run
```

Golden import and bakeoff use `ENV_FILE` (default `.env`) with process environment precedence. Backfill follows its original environment-only DSN/API-key contract. Credentials are never embedded in backfill Qdrant URLs or output diagnostics. Default backfill `run` is a true remote dry run with read/inference probes and no mutation. Applying requires both `--apply` and an exact `--confirm-manifest-sha256`. It does not activate BM25 v2 serving.

Golden import resolves one consistent metadata version (or requires `--version`), rejects nonscalar version values, and accepts only string or null reference answers. A missing or falsy split uses the selected default; explicit split values must be `frozen` or `growth`.

The legacy sparse migration is permanently retired. Without `--dry-run`, it refuses before any remote access; with it, it only reads collection metadata.

BGE bakeoff uses the service's native verified-artifact loaders, with no automatic model downloads. Hosted DashScope/Cohere/Jina adapters require `--allow-live` and their environment keys. The identity baseline is mandatory. Every evaluation is report-only; errors disqualify an adapter. Chinese lexical comparison executes native Jieba, simple-run and bigram BM25. Learned sparse remains explicitly skipped offline, as in source. Native Jieba's dictionary/version can differ from a separately installed Python Jieba; reports must not imply bit-identical segmentation.

Reports contain JSON metrics and Markdown summaries; Markdown presentation is native and is not byte-identical to the Python formatter. No command was run against live providers or databases during this migration phase.

## Knowledge profile promotion

The additional `knowledge-eval-gate` binary recomputes retrieval/parsing metrics from captured
rankings/DocIR, pins dataset/evaluator/profile/baseline identities, rejects any case regression,
and checks quarterly human/judge calibration. It never calls providers or activates profiles.
Its fixture mode cannot authorize promotion. See the [evidence contract and exact commands](../../../../docs/plans/platform-upgrade-2026-10/knowledge-evaluation-promotion.md).
Real quality baselines remain unestablished; the checked-in synthetic fixtures test gate behavior only.
