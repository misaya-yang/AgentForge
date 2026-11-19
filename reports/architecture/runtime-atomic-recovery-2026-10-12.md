# Runtime recovery transaction verification — 2026-10-12

## Integrated change

Recovery now records its owner-fenced terminal ledger state, lifecycle admission, expired approval cancellation and durable terminal event in one PostgreSQL transaction. Broadcast follows commit. Duplicate recovery reads its original receipt; mismatched scope or fencing fails closed. Unknown external side effects remain unknown and are not re-executed.

## Evidence

- Source overlay SHA256: `75962ffb6996a78c93f7a17c0a1fdf43bf229723b1393e65b4bf36ba2d26e8e5` (132 files).
- Full Runtime package all-targets type check: exit 0, 20.018 seconds, maximum RSS 761924 KiB. Receipt: `platform-verification/runtime-worker-build-next/runtime-atomic-check.json`.
- Exact extracted production store transaction compiled with the pinned dependency artifacts; harness executable SHA256 `20d567994bae8f2062528ca1e4747f6b6bc74808dfd58ce45f4bf9fc03fec11e`.
- PostgreSQL16 preserved fixture, existing Runtime role: all 11 reported cases passed. Receipt: `platform-verification/atomic-recovery-terminal-next/receipt-9fa36cb0365545ca8d4f73b9941d17b9.json`.
- Canonical epoch17 and its exact fingerprints verified before and after. Only the owned PostgreSQL process started and stopped. Eight uniquely tagged synthetic sessions were soft-closed; immutable recovery evidence retained. No grants, schema changes, provider calls or Worker dispatch.

Cases cover append failure after ledger mutation rolling back ledger/lifecycle/cursor; successful atomic persistence with unknown-effect barrier; immutable duplicate replay; wrong original scope; takeover while waiting; stale expiry scans; revocation; approval/lifecycle rollback; expired owner with otherwise valid authority; expiry during lock wait; and cancellation winning before recovery.

## Boundaries

This is actual PostgreSQL storage-transaction evidence, with the production functions extracted verbatim. It is not a linked full Runtime process, real HTTP recovery, process-kill acceptance, provider execution or distributed system acceptance. The full Runtime executable remains unlinked; historical compilation failures and their receipts remain retained. Current Gateway and client integrations require their separate gates.
