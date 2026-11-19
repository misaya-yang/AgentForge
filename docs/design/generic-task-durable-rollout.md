# Native generic task durable dispatch

Status: connected source integrated behind an explicit opt-in, pending canonical schema and real-database acceptance. No migration, permission or service activation is authorized by this document. This concerns `/api/v1/submit` and `/api/v1/tasks`, not scheduled tasks, managed runs, SDK execution or Knowledge jobs.

## One connected path

`GATEWAY_TASK_DELIVERY_MODE=memory` is the default. It preserves the current canonical17 queue composition and its existing limitations. An explicit `durable` value wires the same public APIs and TaskManager to one Gateway-only `PgTaskDeliveryLedger`, the existing TaskWorker lifecycle and a current-account validator. Unknown mode values and zero durable worker concurrency fail configuration. Missing ledger, wrong owner, unexpected application/PUBLIC access, or absent Redis session authority fail durable startup. Admission or storage errors never downgrade to the memory queue.

The startup ledger guard checks only its local schema-contract marker and least-authority surface. It is not canonical epoch/fingerprint admission. The reviewed process-wide schema reader and complete canonical epoch chain are mandatory deployment prerequisites; their consolidated018–020 source plan remains inactive; see `canonical-schema-successor-chain.md`. No current schema ceiling or authority history is modified here.

The HTTP boundary attaches a non-bearer JWT session identifier, verified absolute expiry and the existing password revision to the trusted preflight context. Caller JSON cannot provide these values. Session/history handling remains in the existing dispatcher, then task+executable envelope are inserted in one supervised PostgreSQL transaction. A lost admission response returns HTTP503 `task_admission_unconfirmed` with the original `task_id`. Inspect that exact task before deciding what happened. There is no request-id deduplication guarantee and no permission to blindly resubmit.

Workers use `FOR UPDATE SKIP LOCKED` on pending ledger rows and atomically commit a fresh UUID claim with the public processing projection. There is no lease expiry, reset, reclaim or automatic replay of claimed work. A crash before claim leaves recoverable pending work; a crash after claim leaves `claimed_unknown` and requires reconciliation without replay. A malformed envelope or conflicting projection is quarantined as `reconciliation_required`.

After claim, the worker checks the verified JWT absolute expiry, Redis session allowlist, precise password revision, exact user/tenant binding, active/nonlocked account and current tenant/RBAC authority. Missing, expired, logged-out, reset or disabled authority rejects dispatch. Redis TTL does not extend JWT lifetime; legacy actor proofs without absolute expiry fail closed in durable mode. Redis/database failures fail closed. The canonical account authority reader supplies fresh roles; stored roles cannot grant execution. It then calls the existing GatewayDispatcher, service registry, preflight, capacity admission, adapter, accounting and cancellation machinery. The consistency boundary is explicit: Redis session lookup, the account/password-revision/lock read and the current-role/active-tenant query are separate reads. The last query has its own repeatable-read transaction; the whole actor check is not one atomic account+revision+RBAC snapshot. Absolute JWT expiry is checked again after those reads. Concurrent account/session changes are not globally locked against each other or against remote execution.

The mounted native submit API remains JWT-only. Internal API-key, guest or static-key contexts have no durable session proof and fail closed with `task_durable_session_authority_required` before session/history writes. No API-key secret/hash/generation is stored or changed. Future durable API-key support needs a separately reviewed non-secret credential-generation and scoped service/model/rate revalidation contract; a stable key ID alone cannot prove rotation safety.

## Terminal, cancellation and uncertainty

Completion/failure is a ledger UUID+full-identity-fenced transaction with the existing public task row. Cancellation uses the same ledger-first lock order, including before dispatch and after claim. A terminal winner cannot be overwritten. Legacy pre-cutover task rows remain readable/cancellable but are never imported as executable work.

The worker polls the protected claim every250ms while invoke is active. Persisted cancellation or a lost fence drops the local future and signals existing cleanup. A read error also drops/signals execution and leaves the claim for reconciliation. Cancellation confirms task state only; remote effects, provider cancellation and settlement remain separate observations.

Canonical `UPSTREAM_OUTCOME_UNKNOWN`, capacity lease loss, dispatcher cancellation without persisted user cancellation, post-dispatch session/metadata failures and cleanup uncertainty preserve `claimed_unknown`; when the projection write succeeds, public status remains processing with `task_dispatch_outcome_unknown`. Process death may leave processing without that explanatory code. There is no automatic failure overwrite after an uncertain terminal COMMIT, no invocation retry and no callback from an unconfirmed terminal write. A failed durable iteration increments an inspectable failure count, emits a rate-limited sanitized diagnostic and backs off one second before polling a new pending row; it never retries the previous claim or completion write. Shutdown aborts/joins worker lanes before draining supervised delivery operations and closing pools. The existing dispatcher may still have detached usage/provider-cleanup work; worker join does not prove all remote cleanup finished.

Only a confirmed, winning completed/failed write permits a best-effort callback. The callback uses protected envelope destination and the worker-owned outcome, never a mutable task-row reread. Existing destination validation, DNS pinning and retry rules remain in force. Callback delivery is not durable or atomic with completion and may be absent after a crash.

## Canonical migration and rollout

The unnumbered `generic-task-durable-schema.sql.proposal` describes the minimal Gateway-only ledger and ACL delta. It is outside canonical migration discovery. The consolidated source plan proposes018 reader,019 Knowledge trigger repair and020 this ledger. Root must confirm those allocations, authorize the exact security deltas, measure each successor’s four verification fingerprint classes, and admit the paired activation candidate. The inert source bundle does not alter migration discovery or current startup ceilings. Frozen baseline and epochs001–017 stay immutable. Application roles must never receive owner/migrator credentials or fall back to another identity.

Required sequence:

1. Review the final candidate and actual compilation/unit evidence. Do not infer SQL behavior from mocks or text tests.
2. Authorize and apply only the assigned canonical migration in a disposable authority database. Verify fresh chain, upgrade, no-op replay, immutable history, fingerprints and actual-role ACLs.
3. Run the multi-connection atomic-admission/claim/cancel/finish, unknown-COMMIT, process-kill and actor-revocation matrix. Preserve all unknown identities; never reset them to get a green result.
4. Quiesce all Gateway task consumers and HTTP writers. Record existing pending/processing legacy rows as nonrecoverable legacy work; do not manufacture ledger rows for them.
5. Deploy compatible schema-admitted Gateway code and select durable mode on every replica together. Mixed memory/durable replicas are unsupported: a legacy cancellation writer does not update the protected delivery fence.
6. Verify authenticated task APIs, binary/JSON results, callbacks, real-provider uncertainty and restart recovery before declaring acceptance.

Rollback is an operational decision, not switching the variable while work runs. Quiesce all writers/workers first, preserve the ledger and reconcile unresolved claims. Old code/memory mode must never resume ledger-admitted work or serve its cancellation API concurrently. There is no destructive rollback SQL or ledger purge grant. Retention is separately authorized work because payloads can contain user content and non-bearer session references.

## Verification boundaries

Prepared unit tests execute the real manager/worker orchestration with explicit storage/auth/dispatch doubles, plus ledger codec/fence/recovery policy tests and setting-parser tests. They do not prove PostgreSQL transaction/lock/ACL semantics, actual JWT/Redis revocation, authenticated HTTP, provider behavior or process crash recovery. Current environment private socket denial prevents real database acceptance; no alternative network route is attempted. See the external candidate receipt for exactly which checks actually ran.

## Integrated-source regression checkpoint

Root verified all23 exact candidate file hashes before integration. The final native source matched the checked external snapshot. Full foundation57 tests and Gateway37 tests passed; the auth library target compiled but contains zero tests, so this is not functional auth acceptance. The current Gateway executable linked successfully with source unchanged (SHA256 cf2506699e6ad5fde3e0f3062d785873c332a7a6a1119972b3c7eccbbc9343d9). Harness and architecture gates passed. No service started, durable mode remains inactive, and the SQL proposal remains outside canonical migration discovery. Receipts are under platform-verification/durable-task-integration-next/root-full-* and root-current-gateway-executable.json.
