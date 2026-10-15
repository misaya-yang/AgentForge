# AHR-01 Entry Session And Turn Contract Plan

**Phase:** AHR-01 Entry Session And Turn Contract

**Feature:** AHR-F002

**Date:** 2026-07-01

## Plan

1. Confirm AHR-00 is passed and AHR-01/AHR-F002 is active.
2. Add a small Assistant runtime contract helper that builds:
   - `context_snapshot` with provider/model, surface mode, policy, memory, workspace/bootstrap, tool availability, and stable hash/id.
   - `terminal_envelope` with run/session/thread/request ids, tenant/user scope, status, exit reason, timestamps, model/provider, checkpoint/trace fields, approval/resume/cancel/tool/max-iteration flags, and the context snapshot.
3. Wire the helper into `AgentLoop` streaming-first:
   - Emit `context_snapshot` on `run_started`, `run_finished`, `run_error` where available, and `streaming_first_completed`.
   - Persist the envelope in final checkpoint metadata and assistant message metadata.
   - Map approval pending, cancellation, model/tool errors, and max-iteration exits into stable exit reasons.
4. Wire the same helper into non-streaming `AssistantService.chat`:
   - Include `terminal_envelope` and `context_snapshot` in successful return payloads and domain-policy early returns.
   - Finish traces with the same terminal context metadata where supported.
5. Add focused tests under `tests/services/assistant` covering:
   - streaming direct success envelope/context snapshot,
   - streaming approval-pending envelope,
   - streaming model error/cancellation/max-iteration mapping where practical with existing fakes,
   - non-stream response envelope parity.
6. Run the phase validation command: `uv run --package assistant-service pytest -q --no-cov tests/services/assistant`.
7. Write the AHR-01 report, critic artifact, and update `source-packet.md`, `continuity-ledger.md`, `progress-log.md`, `agent-handoff.md`, `feature-oracle.json`, and `loop-state.json`.
8. Run strict harness validation and `--completion-gate --phase AHR-01`.

## Minimal Change Boundary

Implementation is additive only. No database migration, deployment, public field removal, production data mutation, or UI work. A new helper module is allowed only if it prevents duplicating terminal-envelope/context-snapshot logic between streaming and non-streaming paths.

## Validation

- `uv run --package assistant-service pytest -q --no-cov tests/services/assistant`
- `python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --quality-score`
- `python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --completion-gate --phase AHR-01 --quality-score`
