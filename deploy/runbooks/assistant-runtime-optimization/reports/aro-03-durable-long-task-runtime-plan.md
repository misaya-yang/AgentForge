# ARO-03 Durable Long Task Runtime Plan

**Phase:** ARO-03 Durable Long Task Runtime

**Feature:** ARO-F004

**Date:** 2026-06-29

## Plan

1. Add an additive checkpoint table for assistant runs:
   - latest-safe run phase;
   - iteration;
   - message-state hash;
   - pending tool or approval state;
   - idempotency metadata;
   - bounded resume payload and status.
2. Add gateway helpers for saving, fetching, and preparing resume from checkpoints:
   - DB-authoritative when configured;
   - DB-less in-memory fallback for tests/dev;
   - checkpoint payloads sanitized so raw prompts, raw tool arguments, secrets, and unbounded messages are not stored.
3. Add a small assistant-service run resume route that validates tenant/user scope and returns the latest safe checkpoint state without executing duplicate side effects.
4. Wire AgentLoop checkpoint writes at existing boundaries only:
   - run started;
   - model turn started;
   - pending tool before side effects;
   - approval pending;
   - tool completed/failed;
   - terminal succeeded/failed/cancelled.
5. Add tests:
   - gateway checkpoint save/fetch sanitization;
   - approval resume preparation restores pending checkpoint and rejects missing/mismatched approval;
   - AgentLoop emits checkpoint writes without raw secrets;
   - command/approval idempotency tests remain green.
6. Run ARO-03 validation:
   - focused ruff while developing;
   - phase `ruff-checkpoint`;
   - `checkpoint-runtime-tests`;
   - `run-state-contract-tests`.
7. Write ARO-03 actor report, critic artifact, oracle evidence, progress log, source-packet facts, continuity-ledger notes, and ARO-04 handoff.

## Minimal-Change Boundary

Do not introduce Temporal or any external workflow engine. Do not rework AgentLoop orchestration. The first cut is additive checkpoint persistence plus a resume-preparation contract; actual replay continues through existing approval/tool invocation and command de-dupe paths.

## Review Focus

Completion must prove checkpoint payloads are bounded and redacted, schema changes are additive, tenant/user filters apply, and resume preparation cannot double-execute completed tool calls.
