# AHR-00 Comparative Baseline Evidence Plan

**Phase:** AHR-00 Comparative Baseline Evidence

**Feature:** AHR-F001

**Date:** 2026-07-01

## Plan

1. Confirm the active phase and feature from `loop-state.json`.
2. Re-check the current `main` checkout for the Assistant, Eval, trace, memory, tool safety, and frontend files named by the PRD.
3. Re-open the local Hermes technical report; if source is approved and reachable, record official Hermes remote source anchors as read-only comparison evidence.
4. Re-check the local OpenClaw paths named by `openclaw-synthesis.md`.
5. Write the AHR-00 source matrix, terminology invariants, implementation boundaries, and validation command map into `source-packet.md` and `continuity-ledger.md`.
6. Write the actor report and a separate critic verdict artifact.
7. Update AHR-F001 only in `feature-oracle.json`, then update `progress-log.md`, `agent-handoff.md`, and `loop-state.json`.
8. Run source-map validation, strict harness validation, and AHR-00 completion-gate validation.

## Minimal Change Boundary

This phase is docs/runbook-only. It may edit only the AHR harness files under `deploy/runbooks/assistant-hermes-runtime-prd/` and must not modify runtime code, deployment files, migrations, secrets, or production data.

## Validation

- `rg -n "AHR-F001|Hermes|OpenClaw|ExecutionGateway|MemoryProvider|Context Compiler|memory_search|memory_get|transcript|Doctor|Eval|Hermes_Agent_技术分析" deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json`
- `python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --quality-score`
- `python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --completion-gate --phase AHR-00 --quality-score`
