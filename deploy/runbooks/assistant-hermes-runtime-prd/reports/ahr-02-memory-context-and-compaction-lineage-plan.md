# AHR-02 Memory Context And Compaction Lineage Plan

Status: planned

## Selected Phase And Feature

- Phase: AHR-02 Memory Context And Compaction Lineage
- Feature oracle item: AHR-F003
- Dependency check: AHR-01 has `Status: passed` in `reports/ahr-01-entry-session-and-turn-contract-report.md`.

## Plan

1. Inspect the AHR-02 runtime memory surfaces named by the phase/source packet: `runtime/memory/source_store.py`, `runtime/compat/runtime_adapter.py`, `runtime/memory/retriever.py`, `agent/middlewares/runtime_memory.py`, `agent_loop.py`, and existing assistant memory tests.
2. Add a small memory lifecycle contract helper inside the assistant-service runtime layer rather than a new service. It should provide safe default provider hooks, completed-turn sync decisions based on AHR-01 terminal envelopes, source metadata, snippet provenance, and compaction lineage builders.
3. Harden `MemorySourceStore` writes with bounded text, deduped entries/facts, atomic file replacement for long-term/reflection writes, stable `profile` source type support, workspace memory enumeration, and compact threat-scan metadata.
4. Wire `AssistantRuntimeAdapter` and `RuntimeMemoryMiddleware` to preserve snippet provenance as structured metadata while still injecting only bounded sanitized text into the model context.
5. Gate `AgentLoop` runtime daily-memory sync using the AHR-01 `terminal_envelope.exit_reason`, so failed/cancelled/approval-pending/denied/interrupted turns do not sync by default; record skip evidence in completion payloads.
6. Extend `_compact_messages_by_turns()` to emit parent/child compaction lineage and summary provenance without changing the message-list contract.
7. Add focused tests for duplicate memory writes, profile/source separation, workspace source enumeration, provider failure safety, interrupted/approval-pending sync skip, snippet provenance, and compaction lineage.
8. Run `uv run --package assistant-service pytest -q --no-cov tests/services/assistant`, changed-file ruff, `git diff --check`, strict harness validation, and AHR-02 completion gate.
9. Record AHR-02 report, critic artifact, feature-oracle evidence, source-packet code facts, continuity-ledger boundaries, progress log, handoff, loop state, and next-window prompt.

## Minimal-Change Boundary

No DB schema, migrations, deployment, external service calls, production data, UI, Hermes import, or OpenClaw import. This phase expands from harness docs into the runtime memory files explicitly recorded in the AHR-02 source-packet boundary.
