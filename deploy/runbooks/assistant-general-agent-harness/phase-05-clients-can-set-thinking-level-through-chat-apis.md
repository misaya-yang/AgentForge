# Phase 05 - thinking_level API

- PHASE_ID: AGA-05
- FEATURE_ID: AGA-F006
- DEPENDS_ON: AGA-00

## Outcome

Clients can send off/low/medium/high through gateway, assistant-service, and the composer.

## Scope

In: `ChatRequest`, `AssistantChatRequest`, `useChatSession`, `ChatInputArea`

Out: session persistence of the chip

## Done when

- [x] Field exists on both chat request models
- [x] Composer select exists

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Types | request models accept thinking_level | API surface |

## Stop or confirm

- none
