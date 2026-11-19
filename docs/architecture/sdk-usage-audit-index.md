# SDK audit observation lookup index (epoch 16)

This additive, non-unique partial B-tree supports the durable SDK observation
journal in existing gateway.audit_logs. It adds no table, billing ledger, row
rewrite, role grant or authority. Frozen baseline files and epochs 1–15 remain
unchanged; the canonical ordered migration manifest advances to epoch 16.

## Query and predicate

The index is gateway.sdk_usage_audit_lookup on
(tenant_id, user_id, resource_id, action, id), restricted to
 event_type = 'sdk_usage_observation' AND resource_type = 'sdk_request'.

The journal SELECT embeds those two server-owned discriminator literals, while
binding resource ID, tenant ID, user ID and phase. This matters for generic
prepared plans: parameterized discriminator predicates need not imply a partial
index predicate. Equality on the first four indexed columns permits ordered
LIMIT 2 lookup without a separate Sort, including the deliberate duplicate check.
The journal checks that this named index is valid/ready before any history lookup
and fails admission before worker dispatch if the paired migration is absent.

This is intentionally not a unique index. Existing duplicate rows remain intact
as reconciliation evidence; the writer rejects duplicate/conflicting phases.
No blanket deduplication, DELETE or uniqueness constraint is authorized here.

## Canonical rollout

Use the existing migration authority with the epoch-16 manifest entry and measured
016_verification.json; do not run the SQL ad hoc or rewrite baseline schema.sql.
The change is owner-scoped, transactional and old-binary-compatible, with a
10-second lock-acquisition budget and 300-second statement timeout. New journal
writers require the index. Drain/schedule a maintenance window before migration:
ordinary transactional CREATE INDEX scans the audit table and blocks concurrent
audit INSERT writers during construction. SDK admission fails closed while audit
persistence is unavailable; do not treat this as a zero-downtime index build.

A build that exceeds the budget rolls back transactionally; diagnose and schedule
an appropriate maintenance window rather than removing timeout protection. A
CONCURRENTLY deployment would require a separately reviewed canonical
nontransactional/resume contract, not a manual replacement of this migration.

## Verification evidence

Measured in two fresh disposable PostgreSQL 16 single-user clusters with no
network/socket listener, using the canonical fresh_install/apply_epoch and exact
fingerprint functions through the existing pipe adapter:

- Exact canonical epoch-15 fingerprints matched before applying epoch 16
- All four epoch-16 fingerprints measured; ACL, extensions and reference-data
  hashes remain identical to epoch 15, while structural hash changes for the index
- Upgrade and independent fresh baseline-through-16 verify/replay/verify passed
- Actual non-superuser Gateway role SELECT/INSERT worked; CREATE INDEX was denied
- With force_generic_plan and 20,000 synthetic audit rows, the actual literal
  journal query used sdk_usage_audit_lookup without a Sort
- A duplicate SDK phase row remained insertable, preserving evidence; all synthetic
  fixture writes were rolled back
- Both owned PostgreSQL processes shut down cleanly

Eight static manifest/query-binding tests accompany the source. The journal's
separate 12 SQLite tests cover persistence/reopen/duplicate behavior using its SQL
constants; they do not prove PostgreSQL concurrency. Neither the isolated
single-user checks nor static tests establish multi-connection advisory-lock
races, network role authentication, application execution or live deployment.
Those acceptance gates remain open. No shared/live database was changed.

## Common SDK run linkage (epoch 17)

Pure, Knowledge-only and governed SDK requests now all obtain their genuine
assistant_runs parent through the existing SDK authority before journal admission.
The journal therefore receives a server-owned assistant_run_id for every profile.
Its existing scoped request hash/resource identity and unknown-cost semantics stay
unchanged. This does not retrofit identities onto historical journal entries, create
a fake runtime thread, or turn provider observations into a settled charge ledger.
