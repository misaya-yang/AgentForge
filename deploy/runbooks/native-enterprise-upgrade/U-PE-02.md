# U-PE-02 — Native documentation truth

Package state and scope are owned by [loop-state.json](loop-state.json).

## Activation checkpoint — 2026-10-17

Base is `22d9ce0ff9cba51b130f525e7b1add5cc3853744` plus the source/config changes
verified in [U-PE-01](U-PE-01.md). That package has direct verification; its current
image and live journey obligations remain open.

Current entry documentation still describes deleted Python services, deleted command
scripts, inactive source profiles and an obsolete repository location. Correct these
against native entrypoints and the current source lock. Preserve accepted ADRs and
dated receipts; P-ADR-1 is a proposed successor, not an invented accepted decision.

The primary session owns all edits/builds. Read-only reviewers may check command mappings
and architecture claims. No runtime, provider or database mutation belongs to this package.

Next: repair the selected live docs, add a bounded command/link/freshness check, and run
the declared direct gates. Documentation acceptance does not close product acceptance.

## Verification checkpoint

23 documentation/CI tests and 10 bounded performance-tool regressions passed; nine living
documents passed command/link/date checks. Harness passed with zero errors and five pre-existing
program warnings. Source-lock validation passed with no runnable artifacts.

Review corrections retained the Responses compatibility requirement and registered its open
source defect as `UP-PH0-RESPONSES`. Proposed P-ADR-1 and factual docs are complete for this
package; enterprise functionality, images and live acceptance remain open.

[Source-bound result and limits](../../../reports/platform-upgrade-20261017/U-PE-02.json).
