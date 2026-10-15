# HANDOFF

Cold start: `loop-state.json` (authoritative) → active phase file → this file.

## Current state

- Branch: `product-convergence/main` (checkpoint `bf74ff6`).
- PC-00 done; PC-01 next (deletions).
- Approved plan: session plan file `.claude/plans/rippling-floating-waterfall.md`.

## What the next agent must know

1. Do not assume a clean tree history: the checkpoint commit absorbed a prior session's
   harness/docs refresh.
2. Load-bearing endpoints that must survive PC-01 (verified): `POST /assistant/quiz/generate`,
   `GET /assistant/quiz/{id}`, `POST /assistant/quiz/{id}/submit`, share endpoints, public
   `/quiz/shared/{code}` GET+submit — used by useChatSession, QuizCard, QuizShareDialog,
   quiz-history.spec.ts.
3. Parallel tracks run in isolated worktrees; merge order C→D→B; conflict surface:
   `src/api/router.py`, `src/config/settings.py`, `CHANGELOG.md`, `ci.yml`.
4. Migration files are immutable: 011/041/042/043/050 stay; new work = 083/084.
5. Gates from `docs/harness/commands.md` §7. Live-stack items never marked passed without a run.
6. Do not commit further unless the user asks.
