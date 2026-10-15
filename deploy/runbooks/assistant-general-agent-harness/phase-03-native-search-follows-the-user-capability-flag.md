# Phase 03 - Native search flag

- PHASE_ID: AGA-03
- FEATURE_ID: AGA-F004
- DEPENDS_ON: none

## Outcome

Native web search follows `web_search_enabled`, not message keywords.

## Scope

In: `should_use_native_search`, `agent_model_turn.py`

Out: Tavily fallback

## Done when

- [x] Search-like text with enabled=false does not turn search on

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Flag | `pytest tests/services/assistant/test_thinking_policy.py::test_native_search_ignores_message_text` | No keyword gate |

## Stop or confirm

- none
