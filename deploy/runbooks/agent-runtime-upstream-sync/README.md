# Agent Runtime Upstream Sync

- Owner: Primary Agent Runtime implementation session
- Repository: `/Users/yang/projects/AI--Platfform`

## Goal

Synchronize the platform Agent Runtime and Capability Worker to the fixed Codex Harness upstream snapshot while preserving platform contracts and proving the compiled system through real API, UI, provider, and rollback journeys.

## Non-goals

- Database redesign or migration beyond the existing Runtime schema contract.
- Repository governance, cleanup, dependency-policy expansion, or unrelated product features.
- Exposing upstream-only plugins, hooks, browser, terminal, Guardian, SQLite, or multi-agent surfaces.
- Publishing images, pushing this branch, merging `main`, or deploying production.

## Authorization

- Safe local reads, in-scope edits, and non-destructive validation may proceed without confirmation.
- Confirm external writes, destructive actions, meaningful cost, shared-state mutation, secret access, or material scope expansion.
- The user authorized local Docker builds, the existing local E2E identity, and live DashScope/Qwen acceptance for this task. Credentials remain execution-time inputs and must never be printed or persisted.
- Do not push, merge `main`, publish images, run shared migrations, or run any Cargo/Rust command on the host.

## Fixed source identity

- Platform base: `main@991b0cacd7ddbf75752d28061e949ecefe441885` (verified equal to `origin/main` before branch creation).
- Selected upstream: `94cbbddafc1776d5e377bca1b05932c697e82238` (fetched `upstream/main` on 2026-08-30, then frozen for this run).
- Previous Runtime upstream: `93c54bca38996b56d344a2ca65f01627b1953b27`.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| ARU-00 | ARU-F001 | [The fixed upstream snapshot is materialized as the composed Runtime and Worker source with platform overlays preserved.](phase-00-the-fixed-upstream-snapshot-is-materialized-as-the-composed-runtime-and-worker-source-with-platform-overlays-preserved.md) | none |
| ARU-01 | ARU-F002 | [Runtime, app-server, Capability Worker, Gateway, and web compatibility adaptations form one source-green integration.](phase-01-runtime-app-server-capability-worker-gateway-and-web-compatibility-adaptations-form-one-source-green-integration.md) | ARU-00 |
| ARU-02 | ARU-F003 | [Docker-built candidate Runtime and Worker images start in Compose and the Gateway reaches the new Runtime.](phase-02-docker-built-candidate-runtime-and-worker-images-start-in-compose-and-the-gateway-reaches-the-new-runtime.md) | ARU-01 |
| ARU-03 | ARU-F004 | [Real API, SSE, chat, tool approval, cancellation, resume, history, UI, Knowledge, and Qwen journeys pass.](phase-03-real-api-sse-chat-tool-approval-cancellation-resume-history-ui-knowledge-and-qwen-journeys-pass.md) | ARU-02 |
| ARU-04 | ARU-F005 | [Candidate-to-frozen-to-candidate rollback preserves sessions and side-effect safety, and the branch is merge-ready.](phase-04-candidate-to-frozen-to-candidate-rollback-preserves-sessions-and-side-effect-safety-and-the-branch-is-merge-ready.md) | ARU-03 |

`loop-state.json` is authoritative for status and dependencies. Update this table only when the phase map or dependencies change.

## Operating Rules

1. On cold start, read `loop-state.json`, the active feature, `HANDOFF.md`, `init.sh`, and the active phase; inspect `git status --short` and recent `git log`.
2. Run `./init.sh`, then run the active phase's smallest check once to expose unrecorded breakage before editing.
3. Work on exactly one active feature through one `observe -> act -> verify -> decide` cycle at a time.
4. Continue after failure only with a changed, evidence-based hypothesis.
5. Mark a feature `passes: true` only after self-verification; never delete or weaken a feature to make the list pass.
6. Update `loop-state.json` and replace `HANDOFF.md`, then leave one scoped Git commit when repository policy permits; otherwise record the exact uncommitted diff.
7. Stop on success, a named blocker, `waiting_confirmation`, or the iteration cap.
8. Use independent review only when risk or subjective quality makes it useful.

## Validation Boundary

The harness validator checks structure and recorded claim metadata. It does not run product tests or prove that cited evidence is true.
