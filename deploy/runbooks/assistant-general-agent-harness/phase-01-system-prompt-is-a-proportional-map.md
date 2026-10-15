# Phase 01 - Prompt map

- PHASE_ID: AGA-01
- FEATURE_ID: AGA-F002
- DEPENDS_ON: none

## Outcome

Default prompt asks for proportional answers, not eval checklists.

## Scope

In: `CORE_ASSISTANT_PROMPT`, `get_streaming_first_prompt`

Out: structured-output eval fixtures

## Done when

- [x] Prompt contains proportional-map language
- [x] Prompt does not contain FINAL_JSON checklist

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Contract | `pytest tests/services/assistant/test_streaming_prompt_contract.py` | Prompt text |

## Stop or confirm

- none
