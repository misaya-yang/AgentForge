# Product Convergence (PC)

- Owner: AI--Platfform
- Repository: `/Users/yang/projects/AI--Platfform`

## Goal

Make the product users see match the README: an AI gateway that holds identity, credentials,
models, and connectors; agents run on top and declare capabilities; capabilities extend via
plugins. Concretely: delete the Confluence fossil and the standalone quiz/exams stack, ship
first-run onboarding, regroup the nav, and de-hardcode the default model.

## Non-goals

- #6 local-node console page, #7 per-call trace drawer, #9 oversized-file splits (separate rounds)
- Implementing connector `mode: ingest` (design doc only this round)
- Touching eval judge model defaults (`EVAL_JUDGE_MODEL` stays independent)
- Tenant-level model overrides (deployment-level default only)

## Authorization

- Safe local reads, in-scope edits, and non-destructive tests may proceed.
- Checkpoint commit approved via plan (Phase 0). No further commits unless the user asks.
- Confirm deploys, force-push, volume resets, Docker lifecycle actions, and secret printing.

## Phase Map

| Phase | Track | Contract | Depends on |
| --- | --- | --- | --- |
| PC-00 | — | [Baseline](phase-00-baseline.md) | none |
| PC-01 | — | [Deletions](phase-01-deletions.md) | PC-00 |
| PC-02C | C (default model) | [Default model](phase-02c-default-model.md) | PC-01 |
| PC-02D | D (first-run/nav/i18n) | [First-run + nav](phase-02d-first-run-nav-i18n.md) | PC-01 |
| PC-02B | B (connectors) | [Connectors](phase-02b-connectors.md) | PC-01 |
| PC-02M | — | [Merge C→D→B](phase-02m-merge.md) | PC-02C, PC-02D, PC-02B |
| PC-03 | — | [Quiz plugin + shares](phase-03-quiz-plugin-shares.md) | PC-02M |
| PC-04 | — | [Final verification](phase-04-final-verification.md) | PC-03 |

`loop-state.json` is authoritative for status.

## Operating Rules

1. Cold start: read `loop-state.json`, active feature, `agent-handoff.md`, active phase file.
2. One feature per observe→act→verify→decide cycle.
3. Gates from `docs/harness/commands.md` §7; evidence quoted in `loop-state.json`, never claimed.
4. Parallel tracks (PC-02*) run in isolated worktrees; merge order C→D→B; conflict surface is
   `src/api/router.py`, `src/config/settings.py`, `CHANGELOG.md`, `ci.yml`.
5. Migration files are immutable history: 011/041/042/043/050 stay; new work = 083/084.
6. Mark `passes: true` only with evidence (named test or command output).
7. Live-stack items that cannot run locally are reported explicitly, never marked passed.
