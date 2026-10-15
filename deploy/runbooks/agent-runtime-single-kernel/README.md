# Agent Runtime source Single-Kernel Runtime Migration

- Owner: AI--Platfform
- Repository: `/Users/yang/projects/AI--Platfform`

## Goal

Replace the Python AgentLoop with a Agent Runtime source single kernel while preserving platform contracts, tenant safety, and rollback.

## Non-goals

- Running Agent as a tool or sub-agent inside the Python `AgentLoop`.
- Copying Agent product UI, host configuration, credentials, or coding-only defaults.
- Deleting the control runtime before canary, rollback, and data-migration gates pass.
- Publishing a remote fork, OCI image, production rollout, commit, or push without explicit authorization.

## Authorization

- Safe local reads, in-scope edits, and non-destructive validation may proceed without confirmation.
- Confirm external writes, destructive actions, meaningful cost, shared-state mutation, secret access, or material scope expansion.
- Local work uses the dedicated migration worktree and local Harness fork. Preserve the dirty main worktree and `.claude/launch.json`.
- Local Docker and loopback E2E require reading `docs/harness/runtime-and-secrets.md`; never print the dedicated test credentials.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| CHR-00 | CHR-F001 | [Reproducible fork, supply-chain lock, and single-kernel architecture contracts](phase-00-reproducible-fork-supply-chain-lock-and-single-kernel-architecture-contracts.md) | none |
| CHR-01 | CHR-F002 | [Runtime shell, append-only ThreadStore, and V1 event projection](phase-01-runtime-shell-append-only-threadstore-and-v1-event-projection.md) | CHR-00 |
| CHR-02 | CHR-F003 | [Private model data plane, runtime leases, and pure-text Harness execution](phase-02-private-model-data-plane-runtime-leases-and-pure-text-harness-execution.md) | CHR-01 |
| CHR-03 | CHR-F004 | [Context, Knowledge, tools, MCP, artifacts, and office capabilities](phase-03-context-knowledge-tools-mcp-artifacts-and-office-capabilities.md) | CHR-02 |
| CHR-04 | CHR-F005 | [Write tools, approvals, interruption recovery, side-effect safety, and long tasks](phase-04-write-tools-approvals-interruption-recovery-side-effect-safety-and-long-tasks.md) | CHR-03 |
| CHR-05 | CHR-F006 | [Lazy legacy-session import, Docker/browser acceptance, and stable canary rollout](phase-05-lazy-legacy-session-import-docker-browser-acceptance-and-stable-canary-rollout.md) | CHR-04 |
| CHR-06 | CHR-F007 | [Native V2 Thread/Turn/Item API, full cutover, and gated Python loop deletion](phase-06-native-v2-thread-turn-item-api-full-cutover-and-gated-python-loop-deletion.md) | CHR-05 |

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
9. One Run has exactly one kernel owner. Candidate and control may compare fixtures, but they never nest or both dispatch side effects.
10. Every published tool call receives exactly one terminal result before the Turn terminal event.

## Validation Boundary

The harness validator checks structure and recorded claim metadata. It does not run product tests or prove that cited evidence is true.
