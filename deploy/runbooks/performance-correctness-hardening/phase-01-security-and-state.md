# PCH-01 Security and State Correctness

Scope: confirmed High authorization, session/idempotency, Assistant cancellation/recovery and
Web terminal ownership defects.

Required gates:

- Focused security/API/shared-core/Assistant tests.
- Negative cross-tenant and cross-user cases.
- Cancellation and disconnect tests with bounded completion.
- Approval resume and forced-synthesis single-answer tests.
- Web terminal/session-switch Playwright tests if UI changes.

Stop if a fix requires an unreviewed production migration or changes a public API contract.
