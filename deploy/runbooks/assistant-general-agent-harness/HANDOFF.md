# Handoff

- Active phase: AGA-06
- Active feature: AGA-F007
- Status: done
- Completed: Implemented thinking policy (default off, explicit Qwen false, budget, loop-state raise after turn 1), proportional system prompt, relevance-gated discovery-first tool advertisement, native search as a user flag, no Analyzer construction, thinking_level on chat API + composer select.
- Evidence: `2439 passed, 1 skipped` in `tests/services/assistant` (2026-08-13); `test_thinking_policy.py`; `test_tool_selector.py::test_discover_mode_plain_question_advertises_only_discovery_bridges` plus relevance/pin selector cases.
- Next action: Optional live `你好` receipt against running `ai-gateway-assistant-service` after a source hot-update; do not claim production 13s is gone until that container is refreshed.
- Blockers: none
- Confirmation: none
- Decision: continue
