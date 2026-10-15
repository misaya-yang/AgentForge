# Native34 real Gateway SDK HTTP acceptance

**PASS: both supported SDK frameworks, 70/70 recorded assertions each (140 total).**
The root controller completed existing-epoch17 canonical read-only verification,
both HTTP suites including the audit-lock case, and clean shutdown of its owned
Gateway, Redis and PostgreSQL processes. This run performed no schema migration.

## Artifact and source boundary

- Gateway build: `native34`, 179453464 bytes.
- Binary SHA256: `c9ea3d693bc709b0cbe979dc260a028751c1feb7bae1cdd4e50ecd6404fa48b5`.
- Source manifest: `native34/source-after.json`; SHA256
  `5159e0b6ff994ec6b72f5d9a2c286ba1271560fe6d95902c5fffb81a21c96c4e`.
- Each suite verified the running binary and recorded source, and checked source
  stability during that suite. The subsequent recovery-terminal source change
  is **not covered** by this acceptance. This is not a latest-HEAD pass claim.

## Real checks passed in each framework

Existing customer/operator login and exact identities passed. Uniquely tagged,
active/http/session-enabled/zero-retry service rows had no provider key, route or
selected tools; they were synthetic DB fixtures, not registered or usable agents.
The real Gateway SQL principal invoked canonical `assistant.issue_sdk_execution`
to create each run. No registration validation, role, grant or runtime gate changed.

- Owner GET200 returned running SDK identity, no Runtime thread/turn, no public
  request/service lookup IDs, and unavailable usage with accounting not asserted.
- Anonymous GET401; same-tenant foreign-owner operator GET404 and cancel404;
  database state remained running and unrevoked after foreign denial.
- Owner cancel200 persisted cancelled status and revoked authority; subsequent
  owner GET200 returned cancelled.
- A separate run naturally exhausted its 30-second ownership lease. GET200
  projected failed / sdk_authority_expired while the durable row remained running
  and unrevoked until ordinary cleanup. No clock or expiry timestamp was changed.
- With a real ACCESS EXCLUSIVE audit-table lock held, status remained HTTP200
  with unavailable audit persistence, and owner cancel200 durably revoked the run.
  The lock was verified still held before and after cancellation, then rolled back.
- Exact run-scoped checks found no SDK tool call, capability execution or approval
  records before/after cleanup. SDK Worker configuration was absent.

Observed single-request timings, milliseconds; these are not load benchmarks:

| Framework | Assertions | Owner read | Owner cancel | Expired read | Locked-audit read | Cancel while locked |
|---|---:|---:|---:|---:|---:|---:|
| openai_agents | 70/70 | 19.8 | 15.3 | 18.5 | 118.1 | 15.4 |
| claude_agent_sdk | 70/70 | 24.0 | 11.8 | 11.1 | 115.8 | 11.8 |

The locked-read and locked-cancel responses each satisfied the <2s bound, before
the 3s server safety release. This demonstrates availability under a blocked
optional audit read; it does not specifically exercise pool-acquisition timeout.

## Health and honest dependency state

Both suites passed GET/HEAD `/health`200 and `/health/live`200, exact GET bodies
and empty HEAD bodies; `/api/v1/health` returned200/status=ok. Actual GET/HEAD
`/health/ready` returned503. Its public GET body was exactly
`{"status":"not_ready","checks":{"core":"unavailable"}}`, with no private fields.
The existing operator's authenticated `agent_runtime` health projection confirmed
an unreachable required Runtime. Probe targets were fixed owned loopback URLs,
and Runtime/Capability Worker/Knowledge listeners remained absent. This verifies
truthful not-ready behavior, not Runtime/Worker/OCI or overall ready acceptance.

## Cleanup and retained evidence

All six synthetic sessions were removed through genuine owner DELETE requests.
Both exact service rows were removed by the same non-superuser Gateway SQL
principal that inserted them, with exact metadata/shape and no-dependent-route
checks. All four created login sessions were logged out. No public service API
permission was bypassed. The controller confirmed all three owned services stopped.

The six SDK parent/authority records and six deletion barriers/scopes remain as
legitimate durable synthetic evidence: runs cancelled/revoked; cleanup scopes
**pending, attempts=0, deleted_at=NULL**, explicitly unacknowledged with no Worker
configured. Their exact IDs are in each receipt. They were not purged or falsely
acknowledged. If a Worker is later configured against this isolated DB, those
synthetic cleanup requests may be delivered; service/session removal is not proof
of outbox delivery.

Not verified: provider/SDK inference, Runtime/Worker/OCI readiness, cross-tenant
authenticated callers, five-minute absolute authority expiry, production or
load/performance acceptance, or code integrated after the native34 source snapshot.

## Immutable execution evidence

Evidence directory: `platform-verification/sdk-http-acceptance-next/`.

- Controller: `controller-77a46db82c344b30a50e5279448f7394.json`.
- OpenAI Agents: `receipt-443cfa8b32724343859ed08b5ed5f5fc.json`.
- Claude Agent SDK: `receipt-d5eb3ccb9480493a85c5c24edba49d00.json`.

Earlier failed receipts remain preserved; this report does not relabel them as
passes. It was consolidated solely from existing receipts, with no new live action.
