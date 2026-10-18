# Append partitions: native business wiring

Candidate source for U-DATA-03. No database has been changed and no partition
family is enabled by this source change. Keep the existing deployment singleton
and native-authority, restored-copy PITR, role/RLS and retention acceptance gates.

The native `data_connection::append_store` boundary checks
`partition_ops.family_enabled(text)` on the business connection. Before the
successor exists, or while a family's authority state is `registered`, it executes
the original SQL and original SQLx arguments. Discovery/permission/query errors
propagate. It never retries a failed new statement through legacy SQL. The
installed flag takes a shared transaction lock; authority expansion takes the
exclusive lock before installing global directory triggers. It is not an
application environment flag, and cannot be disabled after expansion.

For admitted upserts, the adapter parses the compiled INSERT VALUES, preserves
all bind positions, applies actual column types returned by the approved schema,
and calls the invoker writer with an exact original conflict-tail policy. Policies
are immutable application-side: `native-writer-policies.sql` registers the twelve
actual native statement bodies, including the original SET, WHERE and RETURNING.
Any changed body needs renewed authority admission. Parameters used only by a
trace UPDATE retain the original `$31::text` owner guard. No SQL text from an HTTP
body becomes a policy or a column identifier.

Business transactions keep their original commit boundary. Recorder batches
retain their collapsed identities and atomic commit but group admitted writes by
verified tenant before transaction-local binding. Request task scope cannot be
replaced by a record tenant. Accounting stage receipts, cumulative checkpoints,
provider batch quota settlement, raw usage and trace/outbox effects stay in their
existing transaction. Retired update identities raise before returning to the
caller, including the no-RETURNING checkpoint path; a missing checkpoint payload
cannot authorize another quota charge. Guarded trace/usage conflicts retain empty
RETURNING semantics, and metadata functions reveal no foreign-tenant key/time.

| Family | Actual owner path / write semantics |
|---|---|
| `gateway.usage_records` | Foundation `usage_recorder` max/terminal/error/pricing-snapshot merge; `provider_batch_settlement` DO NOTHING plus exact body verification |
| `gateway.billing_events` | Foundation `usage_stage_receipts` receipt DO NOTHING with RETURNING; `usage_recorder` reconciliation DO NOTHING plus locked quarantine merge |
| `gateway.price_book_v2_accounting_checkpoints` | Foundation cumulative observation upsert; provider batch and Gateway task-batch DO NOTHING, original locked body checks retained |
| `gateway.agent_traces` | GatewayEval ingest owner/tenant-guarded upsert and durable runtime-reconciliation DO NOTHING; original transaction/outbox fence retained |
| `gateway.agent_trace_spans` | GatewayEval ingest trace-bound merge and reconciliation DO NOTHING; parent/reference checks retained |
| `gateway.agent_trace_events` | GatewayEval ingest `(trace_id,sequence_no)` merge and reconciliation DO NOTHING; independent event UUID collision still fails if selected target is absent |
| `gateway.request_traces` | GatewayEval sampled usage plain INSERT, per-record tenant binding, original sampling and two transaction stages |
| `gateway.audit_logs` | Gateway guardrails/studio/governance, Foundation config/plugin/MCP audit, Integrations SDK audit plain INSERT; original advisory locks and bodies retained |
| `gateway.tool_audit_log` | Existing audit writers use plain INSERT; native tenant-policy audit count/list use the nullable-clock pruning boundary |
| `gateway.login_audit` | Foundation/Auth plain INSERT; directory row guards preserve original generated global ID |
| `gateway.service_health_records` | Existing external/authority plain INSERT interface, global ID/clock directory guard; no native service writer currently exists |
| `gateway.task_reconciliation_audit` | Foundation operator reconciliation plain INSERT in its original delivery transaction; original `(task,claim)` audit verification |
| `gateway.price_book_v2_activation_audit` | Foundation price route activation plain INSERT in route CAS transaction |
| `gateway.price_book_v2_import_audit` | Foundation price import attributed/quarantined/inactive INSERT in original import transaction |
| `gateway.price_book_v2_usage_receipts` | Foundation frozen pricing receipt and Gateway batch freeze INSERT; duplicate global request remains an error, never replaced pricing |
| `assistant.assistant_audit_events` | Gateway skill builder plain INSERT, original owner actor/resource scope |
| `assistant.assistant_runtime_items` | Runtime checked `append_assistant_runtime_item` function, original thread lock/sequence/event-key/body identity; contextual Runtime patch adds the transaction activation fence |
| `assistant.assistant_capability_events` | CW checked `append_assistant_capability_event`, original execution/event unique key and dispatch/terminal constraints; contextual CW patch adds transaction fences |
| `assistant.assistant_runtime_model_leases` | Gateway checked `issue_assistant_runtime_turn` routine; plain INSERT plus original six-column FK scope; no ON CONFLICT replacement |
| `assistant.assistant_runtime_model_calls` | Gateway checked 6/8-argument reserve routine in its original atomic function transaction; lease budget/run fence/replay checks stay authoritative |
| `gateway.knowledge_model_calls` | Gateway checked reserve routine, original atomic function transaction, original user/call identity and quota reservation |
| `knowledge.kb_document_progress_events` | Knowledge document-row trigger plain INSERT, same dataset/document projection; contextual KN patch keeps sequence replay exactly, with a transaction activation fence |

Plain INSERT and UPDATE keep their original statements: the admitted source/leaf
row guard reserves global identities, enforces sealed partition state, updates
directory times during key movement and preserves registry FK actions. New-key
reservation runs in the first AFTER INSERT row trigger, after original INSERT RLS
WITH CHECK/NOT NULL/CHECK checks and before FK constraint triggers. The BEFORE
trigger holds the retention barrier; a caught unique error cannot skip original
INSERT validation. A plain
writer does not need a second JSON adapter. Missing families or missing original
unique indexes are prerequisites that fail admission, not silently omitted tables.

Twenty clocks are already NOT NULL: partition them directly by the existing time
column so their existing time bounds prune. The two legacy nullable clocks
(`usage_records.created_at`, `tool_audit_log.created_at`) retain their original
values and MINVALUE routing. Foundation billing summaries, breakdown/timeseries,
request-meter summaries, usage analytics, price cache metrics, daily aggregation
and tenant-policy audit reads add matching COALESCE range bounds while retaining
the original predicates and session timezone date semantics. Sequence-only or
unbounded identity reads keep their complete replay semantics; do not add an
invented time cutoff to obtain a pruning claim. Real EXPLAIN remains a gate.

Admitted families stop legacy payload DELETE loops: Foundation raw usage cleanup,
GatewayEval trace/request cleanup, the existing SQL usage cleanup function, and
the Knowledge progress age/count-cap trigger. Each legacy path still operates
unchanged before admission. Once enabled, retire complete eligible partitions
with the native `platform-append-partitions` plan/archive tool. This deliberately
changes the operational retention granularity; confirm replay-window/archive
requirements in the per-family retention approval. No direct leaf delete can
replace the archive/PITR gate or discard global replay tombstones.

Install the independent proposals only through a reviewed native-authority
successor, in this order: `expand.sql`, `migrate.sql`, `write-compatibility.sql`,
`retention.sql`, `native-writer-policies.sql`, `legacy-retention-adapters.sql`.
All metadata and DDL remain private. The last proposal revokes PUBLIC routines
and grants only metadata/invoker entrypoints to existing bounded native roles
with original family privileges. It grants no payload or directory DML. The
original write RLS and privileges still apply to actual payload rows.

Before any family expansion, the authority receipt must include both modes of
all actual writers, concurrency/rollback and foreign-tenant negatives; expiry of
an accounting checkpoint; incoming composite FK update/delete actions; matching
range-query EXPLAIN; cancellation/drain; original cleanup vs native retention;
PITR/role/RLS receipts. Before cutover recycle prepared pools/rebind views/routines
that retain old relation OIDs, and retain the prior application/schema rollback
pair. Registering policies or compiling these sources is not that acceptance.

External owner patches are delivered under the external evidence directory
`append-partition-completion/handoff/`, with original source and patch SHA256 and
five-line contexts. Apply after each owner finishes, recheck the preimage, format,
and include their native tests. They are not applied to active Runtime/CW/KN
sources by this module owner. No new Cargo dependency is required by those patches.
