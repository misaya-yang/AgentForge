# Phase 04 - Validate current Docker browser and live provider dual gates

- PHASE_ID: SPD-04
- FEATURE_ID: SPD-F005
- DEPENDS_ON: SPD-03

## Outcome

The current repository source is running in the owned local stack and both measured speed
and agent quality meet the declared release gates through real browser/provider paths.

## Scope

In:

- Compose ownership/start/hot-update/migrations/health, static integration gates, in-app
  browser journeys, control/canary TTFT, cancellation, and three complete parity cohorts.

Out:

- Publishing, commit/push, destructive Docker cleanup, or production deployment.

## Done when

- [x] `make validate`, `make status`, Harness, Python, Web, and migration gates pass; every skip is separate.
- [x] In-app browser covers cold new chat, restore cancellation, long stream, running-tool stop, and same-session follow-up with no provider 400.
- [ ] Ten control and ten canary trials meet TTFT <=3.41 s or improve at least 10% without quality regression.
- [ ] Three eight-task cohorts each score at least 6/8, median at least 7/8, with zero infrastructure errors.

## Closeout evidence (2026-08-18)

- Owned Compose stack runs the current source; migrations 083-087 are applied and all
  Gateway, Assistant, Knowledge API/worker, Frontend, PostgreSQL, Redis, and Qdrant
  health checks pass.
- The in-app browser exercised a realistic callback-timeout incident: eight completed
  tool attempts, follow-up planning, an owner-scoped cancellation accepted in about
  1.1 seconds, stream closure in 3.6 seconds, and a successful same-session follow-up.
  No provider HTTP 400 was observed.
- Ten real `qwen3.7-plus` thinking-low trials all completed, but visible TTFT p50 was
  9.280851 seconds (first event p50 0.014213 seconds), so the 3.41-second gate failed.
- The user requested minimum-granularity closeout; the three-cohort stability gate was
  not run in this closeout and remains explicit follow-up evidence, not a pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Runtime | `make hot-update ARGS="--all" && make validate && make status` | Owned containers run current source and are healthy. |
| Browser | In-app browser acceptance using the dedicated E2E account | User-visible flows and stop/resume work. |
| Live quality | TTFT script and three native parity cohorts | Speed and quality both meet their gates. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop and report rather than weakening thresholds when a configured provider or required live capability is unavailable.
