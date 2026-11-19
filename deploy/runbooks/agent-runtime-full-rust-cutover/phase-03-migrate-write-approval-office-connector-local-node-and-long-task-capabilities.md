# Phase 03 - Migrate write, approval, Office, connector, Local Node, and long-task capabilities

- PHASE_ID: FRC-03
- FEATURE_ID: FRC-F004
- DEPENDS_ON: FRC-02

## Outcome

Every state-changing and long-running Assistant capability executes in Rust with exact approval binding, durable recovery, and artifact fidelity.

## Scope

In:

- File/code/planning-state/Artifact writes; DOCX/XLSX/PPTX/PDF create/edit/preview/download; images, Quiz, Confluence, Connector/MCP writes, Local Node actions, jobs, auxiliary model/media subleases, and sandbox controls.

Out:

- Public route cutover or deletion of the still-required Python baseline implementation.
- Rewriting Knowledge Service, Local Node, Gateway, or Web in Rust.

## Done when

- [ ] Every write/unknown execution requires and atomically consumes an approval bound to scope, call ID, descriptor revision, and arguments hash.
- [ ] approve/deny/expire/replay/timeout/disconnect/Runtime kill/Worker kill/Gateway restart all produce one safe terminal result.
- [ ] Office semantic and rendered visual goldens pass; unknown OOXML parts are preserved or modification fails closed without overwriting the source.
- [ ] No Worker receives provider or connector long-term credentials, and auxiliary model calls use bounded Gateway subleases.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Write safety | `make agent-capability-write-parity-gate` | Every Python write contract has a safe Rust equivalent. |
| Office | `make agent-office-visual-gate` | Four document formats satisfy semantic, visual, open, modify, and download checks. |
| Fault recovery | `make agent-capability-failure-gate` | Approval, cancellation, restart, timeout, replay, and unknown-effect invariants hold. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Required independent review must approve authorization, idempotency, secret isolation, Office fidelity, and side-effect recovery before FRC-04 unlocks.
