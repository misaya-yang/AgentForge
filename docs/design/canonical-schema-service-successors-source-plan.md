# Source-owned service successors030–032

Later fixed033–035 additions are specified in
[the final service successor extension](canonical-schema-final-service-successors-source-plan.md).
The original030–032 recipes/specs and scoped proofs remain unchanged.

This is an inactive, fixed source/measurement/bundle implementation. Canonical
source remains029. No new fingerprint, measurement receipt, package lock or main
DB state is synthesized. Primary owns controlled Cargo, actual isolated PG/N-1
acceptance and any activation. Data partition/RLS/admin proposals stay separate.

The single generic factory is `source_tools::service_successors`, with CLI
`schema-admission-service-successors`. It accepts only the compiled catalog at
`proposals/service-successors/catalog.json` and the actual hashed originals named
there. No caller-provided SQL, hash, epoch allocation or receipt override exists.
All generated SQL is reconstructed from exact source bytes, closed replacements
with exact occurrence counts, and fixed owner/grant literals. Its generated file
must match both the reconstructed bytes and its pinned SHA-256.

| Step | Dependency-closed result | ACL change |
| --- | --- | --- |
|030 core expansion | Runtime thread-owner table/three functions; SDK model-plane calls/two functions and two revocation hints; Knowledge pipeline claims; append-only authorization and resource audits; A2A contexts/tasks/outbound effects | Exact owner lines, Runtime SELECT/three EXECUTEs; Gateway SDK SELECT/five column UPDATEs/two EXECUTEs, audit INSERT only, A2A SELECT/INSERT and named state/result column UPDATEs; Knowledge Worker SELECT/INSERT and named CAS column UPDATEs. API/Capability Worker receive none. |
|031 Local Node expansion | Nullable constant-default algorithm column, NOT VALID named algorithm and non-null checks, three policy/verifier/evidence tables | Exact owner plus Gateway SELECT on those three tables only. Old key checks remain. |
|032 Local Node contraction | Validate both031 checks, drop only the two superseded legacy key-format checks, SET algorithm NOT NULL | No ACL change. Hardware schema admission remains closed until032. |

The Runtime/SDK SECURITY DEFINER recipes additionally append explicit `pg_temp`
last in their trusted search_path. Original proposal files remain unchanged.
This prevents temporary-relation shadowing of their unqualified schema-owned
lookups. Only the known exact search_path strings and occurrence counts change;
function bodies, tenant/run/fence/accounting checks and query order are retained.
SDK's exact outer BEGIN/COMMIT pair is removed because native authority owns the
transaction. Authorization audit's one hardcoded Gateway INSERT is replaced with
owner-prefix-derived, `%I`-quoted grants. Local Node's exact unsafe combined
proposal is staged across031/032; every original byte is still hash-bound.

Runtime has no direct owner-table writes. SDK workers receive no call-ledger
access. Gateway cannot read/rewrite audit decisions or enroll attestation
verifiers. A2A updates cannot alter tenant/user/publication/message bindings.
The context `created_at` UPDATE column exists solely to permit the existing
SELECT FOR UPDATE lock without granting identity-column/table UPDATE. Knowledge
API cannot claim/write/read pipeline work; the worker cannot UPDATE tenant,
dataset or document identities. No consumer DELETE, TRUNCATE, REFERENCES,
TRIGGER, ownership or grant option is introduced. All new functions are revoked
from PUBLIC. The new owner must be the actual canonical non-login role, own all
application schemas and lack every cluster/escalation attribute.

Raw ACL measurement requires exactly old lines UNION the fixed per-step additions;
missing, extra, duplicate and removed lines are refused.032 requires identical ACL.
All steps require actual structural change and unchanged extension/reference-data
fingerprints. Real raw evidence reconstructs each reference, complete change
prefix, compatibility ACL and source provenance. No prepare operation writes a
verification reference or `post_migration_noop=true`.

The selected prefix is canonically hashed independently of unselected future
steps/proposals/admission features. A Local Node or later Data change cannot alter
an already frozen030 plan. Existing activated SQL/reference/measurement/prefix
activation proof bytes must match the supplied previous measured preview before
another stage prepares. Per-prefix activation proof files are immutable and
reconstructed identically in later review bundles. The actual029 manifest/lock
remain byte-exact archived anchors; new distribution pins are primary-reviewed.

## Controlled workflow

Run inside the existing controlled authority container with an explicitly owned,
external artifact mount. `/source` is read-only; `/authority-artifacts` below is
an example external container mount, not an instruction to start a new stack.

```text
schema-admission-service-successors validate --source-root /source --sequence 30
schema-admission-service-successors prepare --source-root /source --sequence 30 --output /authority-artifacts/preview030
schema-admission-service-successors measure --source-root /source --sequence 30 --preview-root /authority-artifacts/preview030 --expected-database schema_successor_verify_services030 --run-approved-isolated-measurements
schema-admission-service-successors verify-preview --source-root /source --sequence 30 --preview-root /authority-artifacts/preview030 --measured
schema-admission-service-successors bundle --source-root /source --sequence 30 --preview-root /authority-artifacts/preview030 --output /authority-artifacts/bundle030
```

030 can be measured and reviewed/activated without031/032. Later preparation adds
`--previous-preview /authority-artifacts/preview030 --sequence 31`, followed by
measure/verify/bundle with its explicitly approved disposable DB. Repeat for032 using
its real measured031 preview. The current source may be029 or an already strictly
verified service prefix. A full real029 source copy is also retained under each
preview's `anchor_source_029`; no Git preimage or fake historical receipt is used.
A stage cannot skip its immediately previous real measured prefix. Partial output,
reused destinations, symlinks, altered frozen inputs and unexpected SQL/JSON
inventory fail closed. Retrying failed measurement requires an explicitly new,
valid preview and actual exact DB predecessor; nothing is auto-repaired.

`measure_single_step` retains all existing loopback, distinct migrator, safe role,
exact ledger/ACL/fingerprint, advisory-lock, owner transaction, postcondition and
real no-op replay gates. No measurement operation connects an admin role. All
steps bind their actual approved disposable database and exact previous receipt. A
new clone is allowed only when the unchanged runner verifies its exact complete
predecessor ledger, all fingerprints and postconditions before applying a step.

A bundle includes candidate SQL/reference/measurement/prefix proofs, canonical
manifest/lock, exact029 archives, proposed Runtime/Worker metadata-reader exports
and service minimum/forbidden ACL queries. It reports the new lock SHA for primary
review; it never copies files into canonical source or performs database actions.
Primary must compile the new lock pin, retain the explicit029 proof pin, review
all already-existing overlay files for byte identity, then apply the reviewed
bundle. Current binaries compiled at029 still admit only029; later compiled
service prefixes admit only their own exact complete epoch. `verify-activated`
checks the compiled distribution pin and reconstructs/verifies the complete real
029 ancestry, all fixed service steps and actual measured receipts.

Native admission and secure deployment post-admission execute the source-owned
minimal/forbidden queries after their existing own-role metadata reader passes.
The active manifest/exports are not advanced by this source task. Gateway A2A's
`verify_schema` and Knowledge worker's `validate_storage` now compile and consume
these same catalog queries directly. They retain SELECT/INSERT, every named UPDATE
column, forbidden table-wide UPDATE/DELETE and immutable identity checks. The A2A
guard additionally retains its later outbound-authority schema gate: all five new
columns must exist, remote_task_id/cancel_state require individual UPDATE, and
remote_message_id/card_sha256/card_key_id cannot be updated. That separate proposal
needs its own ordered schema/grant successor;030's source bytes are not changed or
claimed to include it. No table-wide UPDATE is introduced to satisfy a guard.

## Delivery safety and remaining acceptance

The029 linter now recognizes only its complete frozen SQL SHA and full ChangeSpec
SHA. Its three exact safe quoted prefix-grant bodies, four validated monotonic
CHECK widenings and two alterations of newly created empty tables are schema facts, not
sequence-only exemptions. Any byte/spec/owner/precondition change loses this
recognition and remains rejected. All other dynamic SQL, populated-table NOT
NULL/UNIQUE, mixed expand/contract and transaction rules remain unchanged.
Source-owned030/031 grant templates are admitted only for their complete fixed
ChangeSpecs;032's cross-epoch non-null proof is recognized only for its complete
fixed SQL/spec with both prior checks validated before SET NOT NULL.

N-1 is still required for029 and every new step. Static lint cannot set
`release_eligible=true` or emit a compatibility pass. Actual old-application
execution, role-positive/negative schema/tenant tests, Runtime owner contention,
SDK quota/receipt/accounting transactions, Knowledge claim/lease/fence recovery,
A2A Unknown/idempotence and Local Node attestation remain real acceptance work.
Batch's original marker remains empty; this chain never activates it. No HA or
singleton guard is removed. No optional profile or feature is enabled here.

Source tests cover real anchor/proposal pins, exact recipes, external previews,
unmeasured/fabricated evidence refusal, ordered-prefix closure, future-input
independence, minimal raw ACL and negative extra/duplicate/identity privileges,
proposed admission exports, source-owned lint recognition and unchanged rejection
outside those exact schemas. Rustfmt/static review may run separately; Cargo,
PostgreSQL, mTLS and N-1 executions belong to primary's serial validation window.
