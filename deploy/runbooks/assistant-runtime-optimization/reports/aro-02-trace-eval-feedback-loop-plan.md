# ARO-02 Trace Eval Feedback Loop Plan

**Phase:** ARO-02 Trace Eval Feedback Loop

**Feature:** ARO-F003

**Date:** 2026-06-29

## Plan

1. Inspect existing Eval API/schema/services/repository and trace capture tests for assistant, langgraph_proxy, and rag families.
2. Reuse existing dataset/evaluator primitives where possible; avoid schema migration unless the current contracts cannot express source trace links and redacted cases.
3. Add a small self-hosted trace feedback service that can:
   - classify failed or low-score traces into bounded failure modes;
   - produce redacted eval dataset-case payloads with tenant scope and source trace references;
   - produce reviewed, non-applied harness/profile proposals with rollback metadata;
   - evaluate a candidate against simple replay/evaluator gates.
4. Expose focused API/schema helpers only if existing API surfaces need an entrypoint; otherwise keep the first cut service-level and testable.
5. Add tests proving assistant/langgraph_proxy/rag trace handling, redaction, dataset-case construction, proposal gating, and known-bad candidate blocking.
6. Run ARO-02 validation:
   - focused ruff for changed eval files/tests;
   - Eval feedback pytest subset;
   - web checks only if web files change.
7. Write the ARO-02 actor report, critic artifact, oracle evidence, progress log, source-packet facts, continuity-ledger notes, and ARO-03 handoff.

## Minimal-Change Boundary

Prefer a service/test implementation that reuses current eval contracts. Do not add SaaS dependencies, production trace access, destructive data changes, or migrations unless a hard blocker is documented.

## Review Focus

Completion must prove redaction and review-gating. Any harness/profile proposal remains proposed; ARO-02 must not auto-activate generated runtime changes.
