# General Agentic Assistant

- Owner: AI--Platfform
- Repository: `/Users/yang/projects/AI--Platfform`

## Goal

Ship a general assistant that answers ordinary work immediately, uses tools when the model chooses, and never classifies user text into simple vs complex.

## Non-goals

- Greeting/intent/length classifiers
- Replacing TurnKernel, approvals, or tenant policy
- Copying Hermes mandatory skill_view or grok greeting memory rewrite
- Making H1–H9 reliability items part of this harness

## Authorization

- Safe local reads, in-scope edits, and non-destructive tests may proceed.
- Confirm deploys, force-push, volume resets, and secret printing.
- Do not commit unless the user asks.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| AGA-00 | AGA-F001 | [Thinking protocol](phase-00-thinking-protocol-defaults-off-and-is-capability-based.md) | none |
| AGA-01 | AGA-F002 | [Prompt map](phase-01-system-prompt-is-a-proportional-map.md) | none |
| AGA-02 | AGA-F003 | [Discovery-first tools](phase-02-first-model-turn-advertises-discovery-tools-only.md) | none |
| AGA-03 | AGA-F004 | [Native search flag](phase-03-native-search-follows-the-user-capability-flag.md) | none |
| AGA-04 | AGA-F005 | [No dead analyzers](phase-04-dead-intent-analyzers-are-not-constructed.md) | none |
| AGA-05 | AGA-F006 | [thinking_level API/UI](phase-05-clients-can-set-thinking-level-through-chat-apis.md) | AGA-00 |
| AGA-06 | AGA-F007 | [Loop-state think budget](phase-06-after-tools-the-loop-may-raise-a-small-thinking-budget.md) | AGA-00 |
| AGS | session runtime | Thinking is session-scoped; iteration no longer raises. History compact runs with the context engine. [Long-job contract](phase-ags-long-job-contract.md). | AGA-00 |

`loop-state.json` is authoritative for status.

## Operating Rules

1. Cold start: read `loop-state.json`, active feature, `agent-handoff.md`, `init.sh`, active phase.
2. Run `./init.sh`, then the active phase check.
3. One feature per observe-act-verify-decide cycle.
4. No user-text classifiers.
5. Mark `passes: true` only with evidence.
