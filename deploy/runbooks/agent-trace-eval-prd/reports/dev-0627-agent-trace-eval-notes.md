# dev_0627 Agent Trace & Eval Notes

Date: 2026-06-27
Branch: `dev_0627`
Base: `main@0977093`

## Scope

This branch keeps the Agent Trace & Eval module additive. It does not remove Eval endpoints, change existing table meanings, deploy services, or introduce external LangSmith/Langfuse/Phoenix/MLflow dependencies.

## Implemented

- Added `make verify-eval-dev` as the single local gate for targeted ruff, root-package Eval pytest, `assistant-service` package pytest, and web lint/type-check with `corepack pnpm@10.33.0`.
- Aligned trace contracts across Python schema, repository decode, and Web TypeScript types:
  - `trace_family=assistant|langgraph_proxy|rag`
  - trace `thread_id`, `metrics`, `privacy`, `source_adapter`
  - score `target_type`, `target_id`, `evaluator_id`, `evaluator_name`, `score_source`, `confidence`
  - export `redaction_policy`
  - experiment `runs: EvalExperimentRun[]`
- Added OpenAPI/Web type regression coverage for the shared Eval trace contract.
- Added defensive export redaction for OpenInference, OTEL, and LangSmith JSONL payloads.
- Collapsed non-stream assistant chat trace initialization so the history-aware locator updates subsequent writes without a second `start_trace`.
- Exposed assistant trace writer `pending_writes`, `dropped_writes`, `failed_writes`, and `timed_out_writes` through a telemetry snapshot and terminal trace metadata.
- Upgraded Eval Console workbench from fixed template buttons to configurable forms with existing dataset/evaluator/experiment selection.
- Added LangGraph proxy route matrix coverage for assistants, threads, runs, stream, wait, and cancel path shapes.
- Marked `source_kind` as a historical planning alias in the runbook; current public naming is `trace_family`.

## LangGraph/RAG Coverage Matrix

| Family | Path | Current status |
| --- | --- | --- |
| `langgraph_proxy` | `/assistants/{assistant_id}/runs/stream` | trace payload route identity covered; runtime passthrough verification still required |
| `langgraph_proxy` | `/threads/{thread_id}/runs/stream` | trace payload route identity covered; runtime passthrough verification still required |
| `langgraph_proxy` | `/threads/{thread_id}/runs/{run_id}/stream` | trace payload route identity covered; runtime passthrough verification still required |
| `langgraph_proxy` | `/threads/{thread_id}/runs/{run_id}/wait` | trace payload route identity covered; runtime passthrough verification still required |
| `langgraph_proxy` | `/threads/{thread_id}/runs/{run_id}/cancel` | trace payload route identity covered; runtime passthrough verification still required |
| `rag` | gateway knowledge proxy `{dataset_id}/retrieve` | helper and ingest scheduling covered |
| `rag` | assistant-service internal RAG spans | helper-level coverage exists; full runtime roundtrip still required |

## Remote Mac Validation

Run:

```bash
git checkout dev_0627
make verify-eval-dev
corepack pnpm@10.33.0 -C web lint
corepack pnpm@10.33.0 -C web type-check
```

Then start the local stack and verify:

- `/eval` desktop and mobile
- Assistant trace roundtrip
- LangGraph passthrough trace
- RAG retrieval trace
- score submission
- export redaction for headers, cookies, tokens, and raw chunks

Store screenshots and command logs in this runbook folder before merge.
