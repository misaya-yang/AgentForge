# Assistant Runtime Regression Gate — PASS

**Status:** pass
**Timestamp:** 2026-07-13T08:57:11.960902+00:00
**Total elapsed:** 6.51s
**Groups:** 5/5 passed

## Phase Summary

| Phase | Status |
| --- | --- |
| AHR-01 | pass |
| AHR-02 | pass |
| AHR-03 | pass |
| AHR-04 | pass |

## Group Details

| Group | Phase | Passed | Elapsed | Summary |
| --- | --- | --- | --- | --- |
| ahr-01-turn-contract | AHR-01 | pass | 1.19s | ======================== 26 passed, 1 warning in 0.43s ========================= |
| ahr-02-memory-lifecycle | AHR-02 | pass | 0.94s | ======================== 77 passed, 1 warning in 0.39s ========================= |
| ahr-03-tool-safety | AHR-03 | pass | 0.89s | ========================= 7 passed, 1 warning in 0.35s ========================= |
| ahr-04-trace-eval | AHR-04 | pass | 2.9s | ======================= 81 passed, 13 warnings in 2.14s ======================== |
| ahr-04-golden-gate | AHR-04 | pass | 0.59s | status=pass, pass_rate=1.0, critical_pass_rate=1.0, trajectory_pass_rate=1.0 |

## No-Go Thresholds

- All groups must pass; any failure is a no-go.
- Critical phases: AHR-01, AHR-02, AHR-03, AHR-04.
- No production data, secrets, or deployment involved.

## Waiver Policy

A failed group may only be waived when:
1. The user explicitly waives the specific group.
2. The failure root cause is documented in this report.
3. Remaining evidence still proves the affected feature-oracle item.
