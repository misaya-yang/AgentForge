# Phase 04 - Dead analyzers

- PHASE_ID: AGA-04
- FEATURE_ID: AGA-F005
- DEPENDS_ON: none

## Outcome

AgentLoop does not construct QueryIntentAnalyzer or ScenarioAnalyzer.

## Scope

In: `agent_loop.py` `__init__`

Out: deleting the analyzer modules

## Done when

- [x] Analyzers are only stored if injected

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Source | `AgentLoop.__init__` assigns injected values only | No default construct |

## Stop or confirm

- none
