# Handoff

- Active phase: ACU-00
- Active feature: ACU-F001
- Status: ready
- Completed: Nothing implemented yet. The program was scaffolded on 2026-08-14 from a read-only platform architecture review; the target law is `docs/harness/platform-architecture.md`, and the coexistence decision for `permissions` plus `capabilities` is settled in its §3.
- Evidence: Baseline facts verified during scaffolding — `AgentSpec` today has identity/instructions/model/capabilities/knowledge/memory only (`src/api/schemas/agents.py:68`); `ChatRequest` carries 11 builder-level knobs (`src/api/schemas/assistant.py:141-220`); the assistant never references `AgentSpec` (repo-wide grep, zero hits); subagents are code (`apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py`, 1563 lines); no SDK references `agent-public`.
- Next action: Run `./init.sh` to record the ACU-00 baseline, then add `mode`, `permissions`, and `budget` to `AgentSpec` additively, leaving every existing `capabilities` binding untouched.
- Blockers: none
- Confirmation: none
- Decision: continue
