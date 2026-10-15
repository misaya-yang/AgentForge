# Agent Handoff

Current phase: PCH-06 (NO-GO; text-TTFT and stability thresholds remain open).

Current checkout is main@a49eac32d26ef5d19ed75aa1a7a7941f0ab281fc with a large, intentional
uncommitted Assistant optimization diff. Preserve all existing changes and the user's untracked
.claude/launch.json.

Current evidence:

- Final Python suite: 6,186 passed, 23 skipped, 0 failed (6,209 collected, exit 0).
- Web type/lint/build passed; build 795 ms with the forced 1.394 MB UI chunk removed. The
  first-response/first-text targeted Playwright passed. The unconfigured full Playwright run was
  103 passed, 4 skipped, 29 failed and 3 not run, mainly because E2E_API_URL/login state was absent.
- Real container capabilities pass for Responses (stream/non-stream), KB lifecycle/isolation,
  docgen artifact, image artifact, Quiz receipt, authenticated isolation and trusted-local Code
  Executor approval/sandbox/artifact/self-repair. After evidence capture, the privileged local
  overlay was disabled and current source hot-updated; final status is enabled=false/backend=none/
  docker_socket=no/healthy.
- Thinking remains low. Ten successful trials measured first reasoning p50 3.146 s / p95 3.358 s,
  text TTFT p50 3.925 s / p95 4.307 s. Text TTFT fails the 3.41 s release ceiling.
- Final complex full cohort is raw 5/8 and zero infrastructure errors. The unknown-effect answer
  is semantically correct and exact-output replay passes after a generic evaluator synonym fix,
  yielding 6/8. Security and Research remain genuine failures; three full cohorts were not run.
- Docgen bridge cancellation is fixed; the real artifact test now passes in 134.79 s, but that
  latency remains an optimization target.

Next work should add configurable provider/model-variant canary routing and generic constraint/
evidence validation, then run at least three full cohorts and configured full Web E2E. Do not
disable thinking, hard-code fixture answers, or use static report estimates as measured results.
Read loop-state.json and the active phase before changing code.
