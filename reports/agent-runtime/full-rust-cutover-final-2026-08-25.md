# Full Rust Assistant cutover — final acceptance

Date: 2026-08-25

## Result

The Assistant execution plane is cut over to the Rust Agent Runtime and Rust
capability worker. The Python Assistant package/container is absent from the
current Compose application. Public V1/V2 session and streaming contracts are
covered by the release gate, and the current release completed a real
current → frozen pre-delete release → current rollback rehearsal without a
schema downgrade or volume deletion.

## Verification evidence

- Full Python regression completed before the final rollback work: 4,037
  passed, 27 skipped, 0 failed. The skips were environment-scoped (three
  digest-pinned coding sandbox cases, opt-in live-provider cases, and locally
  unavailable Java/Dart toolchains).
- Rust Runtime/ThreadStore verification: 59 passed, including the real
  PostgreSQL contract.
- Web verification: type-check, lint, production build, and bundle budgets
  passed. Public entry gzip was 316,767 bytes; Assistant increment gzip was
  164,918 bytes.
- Authenticated Docker/browser journeys covered ordinary chat, arithmetic,
  long-form writing, Transformer explanation, native search, stop and
  continue, DOCX generation/download, Todo approval, Knowledge, two
  subagents, reasoning Activity, routes, themes, and responsive layouts.
- `make agent-runtime-release-gate` passed on 2026-08-25. It covered source
  provenance, single-kernel/V1/V2 contracts, read-only capabilities, Runtime
  regression, offline Agent Eval, isolation, offline RAG retrieval, SDK SSE,
  and the repository harness. Java/Dart SDK execution and opt-in live-provider
  judging were reported as not run by that command, not counted as passes.
- `make agent-runtime-rollback-rehearsal` passed on 2026-08-25. The frozen
  Gateway, Assistant, and Runtime images are digest-pinned in the rollback
  bundle. Every E2E-user session history returned by the public 500-message
  limit, the session set, Runtime thread/item content, and capability
  execution content, and duplicate execution keys were identical before,
  during, and after rollback. See
  `reports/agent-runtime/rollback-rehearsal-latest.json`.
- `make validate` and `make status` passed after the rehearsal; PostgreSQL,
  Redis, Qdrant, Knowledge API/worker, Gateway, Web, Agent Runtime, and the
  capability worker were healthy.

## Known non-blocking limitations

- The serial release target intentionally does not call a provider. Live Qwen
  evidence came from the authenticated browser matrix rather than that target.
- Maven and Dart were not installed locally, so the SDK SSE target reported
  those implementations as skipped while Python and CLI contracts passed.
- The rollback release images are local, digest-pinned artifacts associated
  with the frozen pre-delete Git commit; the rehearsal fails closed if any
  local artifact digest differs from the bundle.
