# Phase 06 - Reach RELEASE_100 through regression rollback documentation and final review

- PHASE_ID: CLI-06
- FEATURE_ID: CLI-F007
- DEPENDS_ON: CLI-05

## Outcome

All required offline, per-platform native, real Responses/Chat provider, packaging, review, documentation, local-state rollback, and security receipts pass at one release candidate with no required skips.

## Scope

In:

- Final CLI repair, CLI-owned docs, receipts, and independent read-only review.

Out:

- Optional upstream feature parity, local App Server exposure, or unrelated product debt.

## Done when

- [ ] Responses- and Chat-profile third-party models are covered by CLI-owned provider receipts or explicitly block release.
- [ ] TUI/exec, resume, approval, interrupt, failure, packaging, source identity, and local-state rollback gates pass at the final SHA.
- [ ] No blocker/high review findings remain and docs describe only verified behavior.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Release regression | `make independent-cli-gate` plus hosted composed Rust tests | Launcher and native Runtime contracts pass at one SHA. |
| Provider matrix | Serialized real provider journeys for `responses` and `chat_completions` profiles | Third-party providers work without Gateway config or secret persistence. |
| Final rollback | Per-platform prior-native -> candidate local-home rehearsal | The shipped local product has a proven state/side-effect-safe rollback path. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Any required skipped provider, rollback, secret-safety, approval, interruption, or source-identity check blocks `RELEASE_100`.
