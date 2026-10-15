# AS-08 Operations, Governance, Migration, and Rollback Evidence

**Date:** 2026-07-20
**Phase:** AS-08 / AS-F009
**Actor result:** passed, pending independent Critic

## Executed Required Gate

`uv run pytest -q --no-cov tests/api/test_agent_observability.py tests/security/test_agent_data_governance.py tests/database/test_agent_studio_migrations.py`

Final result after Critic remediation: exit 0; `24 passed`, zero failed, zero
skipped. The suite covers explicit Agent/Version/Publication/channel filters,
the complete operational metric set, recursive at-rest redaction, least
privilege, authoritative quotas, legal-hold finalization races, retryable object
cleanup, complete user/tenant cleanup, append-only receipts, additive schema
behavior and migration reentrancy.

## Operations Contract

| Contract | Executed evidence | Result |
| --- | --- | --- |
| Metrics and traces | one explicitly filtered trace CTE drives run/success, p50/p95 total latency, average/p50/p95 first-token latency, tool success, Knowledge hit, feedback, token, cost and channel/version/publication aggregation; response remains retention-limited | passed |
| Audit | explicit Agent/Version/Publication/channel/action/status dimensions; recursive database redaction before storage plus repair of existing Agent audit summaries; individual MCP/Connector/Skill/Knowledge binding and high-risk tool-decision events; Owner/Admin pagination | passed |
| Governance | durable retention/legal hold plus Agent count, active Publication, concurrency, daily token, MCP-call and attachment-storage ceilings; authoritative create/publish/runtime/MCP/storage enforcement, fail-closed policy lookup and threshold audit | passed |
| Revocation | Runtime API tokens and delegated grants are revoked transactionally and audited; cache epoch invalidation prevents stale reuse | passed |
| Deletion | durable idempotent retry receipt, including exact completed-tenant replay after Agent/ACL teardown; legal hold is locked and rechecked at finalization; object cleanup must succeed before relational cleanup; user and tenant scopes revoke Publications/tokens/grants and remove Draft/ACL/runtime/memory/feedback/idempotency/attachments/cache state while immutable Versions, Publication history, audits and receipts remain | passed |
| Least privilege | Viewer/Editor can inspect redacted runtime health; only Owner/Admin can read audits or mutate governance/credentials/deletion | passed |

The object-store boundary is fail-safe: an incomplete external-object cleanup
increments the durable attempt receipt, remains retryable with the same
idempotency key and cannot be recorded as a false terminal success. A legal
hold activated between prepare and finish converts the request to blocked
without deleting data. Terminal replay is limited to the original requester or
a Tenant Admin; another tenant user receives the same not-found boundary.

## Migration and Application Rollback

- `081_agent_studio_operations_governance.sql` is forward-only, additive and
  reentrant. It adds explicit audit dimensions, governance policy, durable
  deletion requests/receipts, attachment cleanup state, triggers, constraints
  and composite trace/session/run indexes; it does not rewrite immutable Agent
  Versions or Publication history.
- Migration 081 was re-applied directly with `ON_ERROR_STOP=1` after its
  additive Critic remediation; every statement committed successfully. A fresh
  `make migrate-status` on 2026-07-20 exited 0 and reported
  `Pending migrations: (none)`. Its last-owner/owner-invariant replacement
  preserves normal active-Agent protection while allowing an explicitly
  deleted tenant Agent to remove mutable ACL rows without dropping history.
- Application rollback uses separate `AGENT_STUDIO_MANAGEMENT_ENABLED` and
  `AGENT_STUDIO_PUBLISH_ENABLED` inputs. Disabling management rejects new Agent
  mutations while keeping authorized evidence reads. The frontend flag removes
  Agent navigation/public entry without unmounting Assistant, Knowledge, Eval
  or Share routes. No destructive down migration is required.

## Local Runtime Evidence

- Compose ownership labels for Gateway, Frontend, Assistant and Knowledge all
  resolve to `/Users/yang/projects/AI--Platfform`.
- All eight services were healthy after the final hot update: Gateway,
  Frontend, Assistant, Knowledge, MCP Docgen, PostgreSQL, Redis and Qdrant.
- Gateway and Assistant both ended with `ASSISTANT_E2E_STUB_LLM=false`.
- After the final hot update, an authenticated disposable-account HTTP probe
  logged in, created an Agent, verified TTFT/tool/Knowledge/feedback metric
  fields, read all six hard quota fields, saved a lower concurrency limit,
  invalidated cache, inspected the redacted audit stream and archived the
  Agent. Every step passed against the migrated local PostgreSQL/Redis stack.
- A second authenticated live HTTP probe completed tenant deletion and repeated
  the identical request after the Agent was marked deleted and its ACL removed.
  Both responses returned the same completed deletion ID/counts with
  `attempt_count=1`; no cleanup effect ran twice. A different authenticated
  tenant user received 404 and no deletion counts for the same Agent/key.
- The required compatibility command exited 0 after remediation. Its static
  isolation invocation reported four passes and two credential skips; the
  same hot-updated source snapshot was then run with an ignored disposable
  local account and the provider-free E2E Stub, producing `6 passed`, zero
  skipped and no external model call. Both Gateway and Assistant were recreated
  with `ASSISTANT_E2E_STUB_LLM=false` afterward and returned healthy. AS-09 must
  provide credentials to the aggregate run: the amended runner now rejects a
  required gate that reports any numeric skipped-test count even when its
  process exits zero.

No external provider quality, production monitoring backend, public deployment
or real user-data deletion is claimed. No API key or generated local credential
is present in this evidence.
