# Rust source checkpoint → Codex continuation

Scope set by the owner at 2026-10-15 15:45 UTC: deliver necessary Rust implementation, compile/test and ZIP first; resume defect fixes afterward and hand off to Codex on 2026-10-16. This document supersedes the historical final-review handoff for this checkpoint.

**Current continuation:** the owner subsequently authorized local live database,
provider and browser acceptance and confirmed that all platform services are
Rust while the optional Python user-code sandbox remains available. Those
directions supersede the historical pauses and pending sandbox decision below.
The full supplied dots source is now imported into the canonical project; see
`reports/dots-source-import-20261015.json`. Use only this project's existing
`agentforge-dev-7801d4a9a4` environment (`deploy/local/compose.yml`, `.env.dev`).
Four existing infrastructure services are running; native application startup
and current-source end-to-end acceptance remain in progress.

## What is delivered

Rust service implementations and operational tools live under `rust/platform-native` and `rust/agent-runtime-overlay`. The connected native stack uses Axum, Tokio, Tower, internal Knowledge gRPC, WebSocket/SSE, and Redis Streams. Web remains TypeScript/React. Existing schemas, tenant authority and public wire contracts are retained except documented compatibility boundaries. This is a source checkpoint, not production acceptance or a prebuilt image release.

First-party Python production/utility source was replaced and recoverably archived. The original 691 legacy test/reference files are accounted for separately:
- 37 source suites have mapped native evidence and were recoverably retired.
- 22 files are empty/comment/docstring-only Python namespace markers.
- 632 are isolated, explicitly unported or partially mapped recovery references. Their removal from the native tree is NOT full test migration or equivalent coverage.

Read `reports/native-source-retirement.json`, `reports/native-framework-marker-retirement.json`, `reports/unported-test-recovery-inventory.json`, and `reports/native-test-delivery-audit.json`. Original hashes and historical Library ZIP version125 permit recovery. Restore legacy material to a separate reference workspace, not onto the native tree.

## Start

1. Read AGENTS.md, `docs/migration/full-rust-status.md`, and the newest checkpoint in `deploy/runbooks/native-enterprise-upgrade/loop-state.json`.
2. Verify the archive's native recovery inventory and safe member paths. Build artifacts, credentials, real environment files, user data and dependency caches are excluded.
3. Use pinned manifests/locks, one Cargo job, zero debug/incremental output, an external target directory and a free-disk guard.
4. Restore the pinned upstream Runtime using the existing source-assembly commands. The overlay is not a complete offline copy of upstream dependencies. Source locks deliberately do not claim runnable images.
5. Run compilation and relevant native test targets. Check exact recorded commands/results; ignored external-stack tests are unrun, not passes.

## Remaining work after this checkpoint

Prioritize complete end-to-end service journeys and confirmed defects. Avoid replaying broad audits or replacing public behavior solely to satisfy Python-private mechanics.

- Billing: per-stage accounting receipts and replay helper are present. Unknown commits remain quarantined; no full durable intake outbox is claimed. Trace writes are reconciliation-only. Post-checkpoint repair now makes uncertainty sticky and replay permission monotonic under transaction-locked conflict updates; six pure production receipt tests passed. Its new real database conflict/replay regression is compiled/opt-in but still unrun. Legacy callback/Redis-list DLQ and retry-count compatibility is incomplete; the old production container never configured its callback.
- Authentication: read-only `/auth/me` supports scoped metadata introspection without weakening protected-route authority. Protected-route external federation is intentionally stricter than historical Python behavior; decide explicit compatibility policy before broadening it.
- Test coverage: continue from retained per-source maps. Many legacy assertions are unported or only partially represented. Completed offline tests do not establish live multi-service behavior.
- Optional code execution: the Rust supervisor still offers an external Python-language scientific sandbox. First-party source contains no Python; zero-Python runtime/dependency claims would be false while that optional feature exists. Owner decision on retaining this capability remains open.
- External SDK/plugin boundary: arbitrary in-process Python LangGraph modules and official Python SDK execution are not supplied by native code merely by renaming interfaces. Preserve documented native protocol adapter boundaries and validate actual required integrations.
- Integration acceptance remains pending: real disposable PostgreSQL/Redis/Qdrant/S3, providers, macOS/desktop interactions, migrations/restore, source images, and end-to-end Web/Runtime/Worker/Knowledge journeys. Live execution was paused by the owner; obtain the appropriate scope before running it.

## Verification evidence discipline

`loop-state.json` and the source-status report distinguish the last complete all-target check from subsequent edits. Targeted logs in the delivery evidence record actual successes, failures, fixture corrections, and disk-guard interruptions. Compilation is not release acceptance. The final checkpoint must record its final compile/test outcome and archive hash before delivery.

## Latest bounded verification

Whole-workspace/all-target check 96 passed; final repeat 103 is recorded in the checkpoint evidence. Gateway run 98 selected offline tests passed (38 pass,11 explicitly ignored). Knowledge run 100 passed 76 tests across 13 targets; the remaining tools target overflowed the default debug test stack. Its same binary passed both tests with RUST_MIN_STACK=16777216 and one test thread, run 102. This is a harness workaround, not proof that production debug-stack sensitivity is eliminated; investigate after delivery. Packaging inventory/hygiene run 101 passed. SDK response run 92 and eval run 95 passed. Raw selected output is bundled under `reports/checkpoint-evidence-20261015/`; acceptance limits above still apply.

## Verified follow-up after version 126

Two bounded repairs were completed after initial ZIP delivery:
1. Billing failure-receipt conflicts preserve the original payload and can only tighten quarantine. Pure production receipt tests passed 6; actual SQL concurrency/replay remains unexecuted under the live-test pause.
2. Knowledge retrieval boxes its inner pipeline without spawning or changing deadlines/authority/cancellation. Measured public futures fell from311,320–311,696 bytes to1,544–1,920 bytes (multi-tool remains 240 bytes). The original two tools tests now pass on the default stack, with an additional <=16 KiB layout regression. This removes the observed test-stack abort; it is not a live RAG acceptance claim.

The immutable coding-v2 benchmark also passed an isolated offline functional check: its intended unsolved copy failed exactly four business assertions and passed the tenant-control assertion; applying the reference patch to a separate copy passed all five. Original fixture hashes were unchanged. Provider/agent/sandbox acceptance remains unrun.

Evidence is under `reports/post-checkpoint-evidence-20261015/`. Retain the original v126 results as historical proof; use the latest source and checkpoint record for continuation.
