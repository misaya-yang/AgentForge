# Phase 04 - Add deterministic offline compatibility and failure-path tests

- PHASE_ID: CLI-04
- FEATURE_ID: CLI-F005
- DEPENDS_ON: CLI-02, CLI-03

## Outcome

Deterministic tests cover strict provider config, independent home, native dispatch, direct Responses config, Chat request/SSE/tool conversion, retries, fail-closed inputs, secret isolation, and the published bundle.

## Scope

In:

- `sdk/cli/src/**/*.test.ts`, non-secret fixtures, `Makefile`, `harness.yml`, and the independent CLI CI job.

Out:

- Live provider claims, hosted Rust claims, or unrelated SDK refactors.

## Done when

- [x] `make independent-cli-gate` is canonical and CI-wired.
- [ ] Native composed-source mock tests cover exec JSONL, approval, interrupt, resume, and provider stream faults.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| CLI full gate | `make independent-cli-gate` | Product config, adapter, launcher, secret isolation, and bundle pass together. |
| Harness | `make harness-check` | Make/harness/docs/CI command ownership stays aligned. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- A Node/mock/dry-run pass must not be reported as native Rust or real provider evidence.
