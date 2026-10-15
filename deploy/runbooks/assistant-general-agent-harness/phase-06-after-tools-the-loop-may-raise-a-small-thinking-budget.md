# Phase 06 - Loop-state thinking

- PHASE_ID: AGA-06
- FEATURE_ID: AGA-F007
- DEPENDS_ON: AGA-00

## Outcome

After the first model turn, an off request may rise to low. User text is not read.

## Scope

In: `resolve_turn_thinking_level`, `agent_model_turn.py`

Out: greeting detectors

## Done when

- [x] iteration 1 stays off; iteration 2 becomes low

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Policy | `test_loop_raises_budget_only_after_first_model_turn` | Run-state only |

## Stop or confirm

- none
