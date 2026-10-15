# Phase 05 - Reach CANDIDATE_80 through serialized CLI runtime and provider acceptance

- PHASE_ID: CLI-05
- FEATURE_ID: CLI-F006
- DEPENDS_ON: CLI-04

## Outcome

The integration owner observes a usable independent CLI candidate through the composed local Runtime and one real CLI-owned provider profile, including text, function tool approval/deny, interrupt, and resume.

## Scope

In:

- Serialized native build/provider receipts; repair remains limited to CLI packaging, launcher/config, or bounded adapter behavior.

Out:

- Hosted Gateway/Runtime changes, provider secret capture, publication, or claiming unrun native/provider checks.

## Done when

- [ ] Offline CLI gates pass at the candidate SHA.
- [ ] Native artifact source/overlay identity is verified before live use.
- [ ] One configured third-party model completes a local Runtime turn, tool allow/deny, resume, and interruption without credential exposure or duplicate side effects.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Runtime acceptance | Integration-owner journey through the packaged local Runtime | The independent CLI is usable on a CLI-owned provider. |
| Local state smoke | candidate -> prior CLI -> candidate against a copied local home | Sessions remain readable and tool side effects are not replayed. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Provider credentials/native build resources are unavailable in this worktree; the integration owner must run this phase without printing secrets.
