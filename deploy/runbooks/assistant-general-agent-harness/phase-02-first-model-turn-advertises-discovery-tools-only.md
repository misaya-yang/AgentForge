# Phase 02 - Discovery-first tools

- PHASE_ID: AGA-02
- FEATURE_ID: AGA-F003
- DEPENDS_ON: none

## Outcome

Safe-profile ordinary questions advertise exactly the three discovery bridges.
Clearly relevant or explicitly pinned tools may also be advertised directly;
everything else stays callable through `tool_search` / `tool_call`.

## Scope

In: `tool_selector.py`, `agent_context_lifecycle.py`

Out: approval/policy

## Done when

- [x] An unmatched ordinary question exposes only the three discovery schemas
- [x] Existing relevance scoring can surface a clearly matched generation or other tool
- [x] Explicitly pinned tools can still be first-turn visible
- [x] Deferred authorized tools remain reachable through discovery

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Selector | `uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant/test_tool_selector.py` | Ordinary, relevant, and pinned advertisement |
| Discovery | `uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant/test_tool_discovery.py` | Deferred authorized tools remain searchable and invokable through the bridges |

## Stop or confirm

- none
