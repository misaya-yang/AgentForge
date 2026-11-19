# Full Rust Assistant Cutover

- Owner: Product/engineering
- Repository: `.`

## Goal

Migrate the complete Assistant execution surface to one Rust Agent Runtime plus a Rust capability worker, preserve every public product contract, and delete the Python AgentLoop only after parity evidence passes.

## Non-goals

- Rewriting Gateway, Knowledge Service, Local Node, or Web in Rust.
- Changing public Assistant/Responses/V2 Agent HTTP or SSE contracts.
- Production deployment, schema downgrade, or deletion of persistent Docker data.
- Keeping a dormant Python model/tool loop in `main` as a rollback mechanism.

## Authorization

- Safe local reads, in-scope edits, and non-destructive validation may proceed without confirmation.
- Confirm external writes, destructive actions, meaningful cost, shared-state mutation, secret access, or material scope expansion.
- Local Docker and authenticated E2E are authorized; credentials are read only at execution time and are never printed, copied, committed, or stored in this harness.
- The user explicitly authorized deleting the tracked Python Assistant execution plane after FRC-00 through FRC-04 pass. Docker prune, volume deletion, production rollout, force-push, and history rewrite remain unauthorized.
- Scoped commits, ordinary pushes, the final merge into `main`, and deletion of the one short-lived integration branch/worktree are authorized by the approved plan.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| FRC-00 | FRC-F001 | [Freeze full-product compatibility baseline and rollback bundle](phase-00-freeze-full-product-compatibility-baseline-and-rollback-bundle.md) | none |
| FRC-01 | FRC-F002 | [Implement the Rust capability execution contract and worker](phase-01-implement-the-rust-capability-execution-contract-and-worker.md) | FRC-00 |
| FRC-02 | FRC-F003 | [Migrate read-only capabilities and Gateway control ownership](phase-02-migrate-read-only-capabilities-and-gateway-control-ownership.md) | FRC-01 |
| FRC-03 | FRC-F004 | [Migrate write, approval, Office, connector, Local Node, and long-task capabilities](phase-03-migrate-write-approval-office-connector-local-node-and-long-task-capabilities.md) | FRC-02 |
| FRC-04 | FRC-F005 | [Switch every Assistant and Agent product entrypoint to the Rust execution plane](phase-04-switch-every-assistant-and-agent-product-entrypoint-to-the-rust-execution-plane.md) | FRC-03 |
| FRC-05 | FRC-F006 | [Physically delete the Python Assistant execution plane and docgen implementation](phase-05-physically-delete-the-python-assistant-execution-plane-and-docgen-implementation.md) | FRC-04 |
| FRC-06 | FRC-F007 | [Complete automated, Docker, browser, Agent Eval, performance, and rollback acceptance](phase-06-complete-automated-docker-browser-agent-eval-performance-and-rollback-acceptance.md) | FRC-05 |

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
