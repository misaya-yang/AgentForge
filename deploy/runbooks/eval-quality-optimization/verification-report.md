# Eval Quality Optimization Verification Report

Date: 2026-07-10

## Delivered scope

- Golden expectations and recorded observations are separate inputs. Missing or malformed evidence fails closed.
- Typed assertions and runtime trajectory expectations are executed, including latency, exit reason, memory/resume, gateway, sandbox, arguments-hash, loop-guard, failure-mode, and redaction checks.
- KB RAG judge inputs use bounded untrusted JSON, unsupported metrics fail closed, duplicate metrics are stable-deduplicated, and non-finite/out-of-range scores become `review`.
- `context_precision` is computed locally from one boolean verdict per retrieved rank.
- RAG review rows remain retryable and do not pollute valid score averages/counts; executor aggregation uses persisted labels.
- The Assistant runtime gate supplies the maintained observations file, writes nested golden artifacts to a temporary directory, and supports `gate --no-write`. The Make target is read-only.

## Verified evidence

- Root focused eval suite: `66 passed`, one existing Starlette/httpx deprecation warning.
- Knowledge-service RAG judge suite: `9 passed`, one existing Starlette/httpx deprecation warning.
- Assistant runtime gate: `PASS`, five of five groups.
  - AHR-01: `21 passed`
  - AHR-02: `77 passed`
  - AHR-03: `7 passed`
  - AHR-04 trace/eval: `69 passed`
  - Golden gate: pass rate, critical pass rate, and trajectory pass rate all `1.0`
- Ruff on every changed Python file: pass.
- `make -n verify-assistant-runtime-dev`: resolves to `gate --no-write`.
- Runtime report hashes were identical before and after the gate:
  - JSON: `4c3a3cfee11ffced798773531da5879af846af33db3f169b3f20b0a7a82cbb2a`
  - Markdown: `8fc386ab6ce7f63fef0f7a99ef5d4cc2f609c57e85ef87dc79a2ec197d1fac42`

## Evidence boundary

The passing gate proves deterministic offline contracts and recorded-observation replay. It does not execute a live model, measure the live RAG distribution, or compare a newly executed candidate runtime against a baseline.

No Docker, provider call, database migration, deployment, or live traffic test was run.

## Remaining TODO

- Preserve approval pause/resume trace evidence with a monotonic persisted sequence cursor.
- Select only the latest compatible score revision per trace/evaluator/version/metric in historical KB RAG summaries.
- Add immutable candidate execution and per-case baseline/candidate deltas.
- Add transactional trace completeness/outbox semantics.
- Build a unified RAG evidence packet with citation-to-document identity and multilingual calibrated live datasets.
- Make the full offline quality gate a required dependency of tag publishing.
