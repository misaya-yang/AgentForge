# Phase 00 - Thinking protocol

- PHASE_ID: AGA-00
- FEATURE_ID: AGA-F001
- DEPENDS_ON: none

## Outcome

Qwen requests omit no thinking flag. Off is explicit false.

## Scope

In: `thinking_policy.py`, `model_catalog.py`, `model_registry.py`, `assistant_service.py`

Out: user-text routing

## Done when

- [x] Omitted thinking_level → `enable_thinking is False`
- [x] `low` sends `thinking_budget=256`

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Default off | `pytest tests/services/assistant/test_thinking_policy.py` | Body contract |

## Stop or confirm

- none
