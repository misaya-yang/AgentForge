# SOTA Performance and Agent Quality Dual Gate

- Owner: Product/engineering
- Repository: `.`

## Goal

Integrate the staged Grok hardening batch, improve platform performance and agent quality, and prove both through Docker and browser evidence.

## Non-goals

- Do not weaken tenant isolation, platform-admin separation, usage idempotency,
  tool-call/result pairing, approval gates, or unknown-side-effect recovery for latency.
- Do not add default semantic caching, Qdrant quantization/sharding, a session-message
  schema migration, or Assistant multi-worker execution without their measured gates.
- Do not commit, push, publish, prune Docker state, or touch `.claude/launch.json`.

## Authorization

- Safe local reads, in-scope edits, and non-destructive validation may proceed without confirmation.
- Confirm external writes, destructive actions, meaningful cost, shared-state mutation, secret access, or material scope expansion.
- Local Compose startup, hot update, migrations against the repository-owned local
  database, the dedicated E2E account, and bounded configured-provider acceptance are
  authorized. Never print or persist credentials.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| SPD-00 | SPD-F001 | [Integrate and repair the staged Grok correctness batch](phase-00-integrate-and-repair-the-staged-grok-correctness-batch.md) | none |
| SPD-01 | SPD-F002 | [Close streaming pairing, backpressure, and telemetry safety](phase-01-close-streaming-pairing-backpressure-and-telemetry-safety.md) | SPD-00 |
| SPD-02 | SPD-F003 | [Optimize Gateway Assistant and Knowledge hot paths](phase-02-optimize-gateway-assistant-and-knowledge-hot-paths.md) | SPD-01 |
| SPD-03 | SPD-F004 | [Optimize Web SDK and agent quality](phase-03-optimize-web-sdk-and-agent-quality.md) | SPD-02 |
| SPD-04 | SPD-F005 | [Validate current Docker browser and live provider dual gates](phase-04-validate-current-docker-browser-and-live-provider-dual-gates.md) | SPD-03 |

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
