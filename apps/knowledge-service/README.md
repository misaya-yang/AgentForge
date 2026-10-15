# Knowledge Base Microservice

Independent KB service extracted from AI Gateway. Provides document ingestion,
chunking, embedding, vector storage (Qdrant), and retrieval APIs.

## Quick Start

```bash
# Install in development mode
pip install -e ".[dev]"

# Run locally
KNOWLEDGE_QDRANT__URL=http://localhost:6333 \
KNOWLEDGE_DATABASE__DSN=postgresql://localhost:5432/gateway \
uvicorn knowledge_service.main:app --reload --port 8092
```

## Configuration

All settings use the `KNOWLEDGE_` prefix with `__` as the nested delimiter.
See `.env.example` for the full list.

## Docker

```bash
docker build -t knowledge-service .
docker run -p 8092:8092 --env-file .env knowledge-service
```


## Parent-child retrieval resource bound

When parent-child retrieval is enabled, the post-ranking fold uses a fixed
worker pool for scoped parent reads. It shares the already-resolved retrieval
query concurrency limit (`KNOWLEDGE_PROCESSING__RETRIEVAL_QUERY_MAX_CONCURRENCY`,
processing default 8), including a smaller per-request batch limit when supplied.
At most that many parent lookups and worker tasks are active per request, even
at the 2,000-candidate ceiling; no task is created for every queued parent.
Direct calls to `fold_candidates_to_parents` default to eight workers.

Each distinct parent is still read once through `get_segment_scoped`, with the
same tenant, dataset and active-document/segment predicates. This does not use
the unscoped `get_segments_by_ids` helper. Best-child positions, max-child scores,
parent/context modes and per-parent missing/error fallbacks are unchanged.
Caller cancellation or cancellation of a scoped read cancels and drains sibling
workers; query timeouts retain the existing per-parent fallback behavior.

This is a per-request resource bound, not a global database admission limit or
an N+1-query removal. It can trade single-request latency for lower pool pressure;
it does not establish live throughput, tail latency or retrieval-quality gains.
The regression suite checks high-parent-count task/query bounds, deterministic
output, duplicate-parent deduplication, scoped reads and cancellation cleanup:

```sh
uv run --all-packages --extra test pytest -q --no-cov tests/services/knowledge/test_parent_child_concurrency.py tests/services/knowledge/test_t9_parent_child_unit.py tests/services/knowledge/test_t9_parent_child_pipeline.py
```
