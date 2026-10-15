# Canonical successors: one inactive018–020 source chain

Status: **source preparation only**. The main canonical authority remains017;
current startup ceilings and default memory queue are unchanged. No database,
permission, service, migration discovery, fingerprint or durable-mode activation
is authorized by this document. Proposed numbering requires root's allocation
review before admission. The machine carrier is `canonical-schema-successors.json`.

## Minimal integration order

1. Integrate the inert proposals, this plan, checker and opt-in probe only. Do not
   integrate recovered independent018 startup/ceiling patches. The checker pins
   the exact017 manifest, frozen baseline/bootstrap/ACLv1 and all001–017 files.
   It rejects changed preimages instead of rebasing history automatically.
2. Review and approve the exact persistent authority deltas below. In a separately
   authorized disposable PostgreSQL16 environment, compose the external preview,
   use the canonical migrator/owner path, and measure018, then019, then020. Each
   change and its success ledger row commit in the runner's normal transaction.
   The preview cannot pass canonical verification until real reference files exist.
3. For each successor, retain raw four-class measurements and the exact ACLv1
   compatibility proof, produce its real verification reference, verify → no-op
   replay → verify, then independently fresh-install/replay the full chain. Keep
   original source/Git provenance available; an unversioned recovery copy alone
   is not proof of frozen-baseline provenance. Do not modify the runner, hide ACL
   lines, copy017's changed fingerprints or execute proposal SQL ad hoc.
4. Run the prepared actual-login probe and the additional acceptance gates below.
   Only then integrate the separately reviewed authority/application activation
   patch and measured references as one matching candidate. Generate the Python
   identity from that exact manifest; native admission embeds the same carrier.
   Build/typecheck and execute actual startup failures before rollout.
5. Keep memory as default. Durable activation is a separate, explicitly approved
   coordinated cutover after its own real Redis/claim/cancel/crash/HTTP gates.
   Quiesce all task writers and consumers; do not run memory and durable replicas
   against ledger-admitted tasks, import old task rows, or replay unknown claims.

## Exactly three separately measured deltas

| Candidate | Source and authority | Deliberately absent |
| --- | --- | --- |
|018 application_schema_admission|Parameterless STABLE SQL SECURITY DEFINER reader owned by canonical unprivileged NOLOGIN owner. Static fully-qualified reads of baseline/change/attempt ledgers; only nonsecret metadata. EXECUTE only for Gateway and Knowledge API/worker, no grant option.|No ledger SELECT/DML, public-schema USAGE, new membership, schema CREATE, credential, business object or default-privilege change.|
|019 knowledge_progress_trigger_authority|Replace only the existing progress trigger routine with same owner/signature/payload body, SECURITY DEFINER, qualified event inserts and strict AFTER ROW INSERT/UPDATE/DELETE/no-args/knowledge.documents guard. Owner-only EXECUTE ACL preserved.|No direct event DML or sequence access for apps; no modified trigger definitions, event vocabulary, retention routine or frozen SQL.|
|020 gateway_task_deliveries|Create Gateway-only ledger, pending-order partial index and contract comment. Revoke inherited app/PUBLIC rights on the new relation, then grant exact Gateway SELECT/INSERT/UPDATE.|No app DELETE/TRUNCATE/REFERENCES/TRIGGER, other-role ledger access, schema rights, automatic data import, reset/replay or API-key changes.|

Both definer functions explicitly set `search_path=pg_catalog, pg_temp` so the
temporary schema cannot implicitly precede catalog type lookup. Relation names
remain qualified. The script's source-body MD5 comparison is derived from known
SQL bytes; it is not a measured database fingerprint. Descriptors and full SQL
SHA256 values bind the candidate; PostgreSQL parsing and postconditions remain
unverified until actually run. All new references are intentionally absent.

The confirmed blocker019 addresses is the exact production Knowledge requeue
UPDATE failing SQLSTATE42501 in `kb_document_progress_events` under the worker's
existing rights. A successful administrator-only predicate matrix did not clear
that block.018 alone cannot make Knowledge worker writes succeed.

## Candidate admission windows, not current activation

| Process/profile after activation | Exact admitted range |
| --- | --- |
|Native Gateway, memory|018–020; business floor017 plus installed-reader floor018|
|Native Gateway, durable|020 only, before password/catalog writes, plus existing local ledger/Redis guard|
|Native Knowledge API/worker/all|019–020; require repaired trigger as well as reader|
|Retained Python Knowledge with reader|019–020; reject018 despite readable metadata|
|Retained Python Knowledge without reader|Existing direct-read005–017 path only, where access already existed|
|Retained Python Gateway|002–010 unchanged; not a native-feature rollback candidate|

Missing/denied reader, incorrect baseline, changed full checksums, holes, unknown
future epochs, incomplete/attempt rows and absent020 durable ledger fail closed.
No role switching or privileged/direct-read fallback follows a reader failure.
Only an absent reader preserves the already existing old Python Knowledge path.
The baseline marker comparison is metadata admission, not live schema drift or
fingerprint verification; migrator verification remains independently required.

Current canonical017 memory availability is preserved by the inert integration.
After a deliberate future migration, old native17 builds have no startup epoch
ceiling: that absence is not compatibility proof. Their business schema may be
structurally preserved, but actual mixed-version behavior is unverified. Old
Python Knowledge capped017 refuses018+; old Python Gateway capped010 cannot
serve the native schema. Drain incompatible processes before upgrade, select a
proven compatible image for rollback, and never downgrade durable to memory
while any ledger-admitted task can still be written/cancelled. No rollback SQL
or grant broadening is supplied.

## Reproducible source-only commands

Run the repository's Python environment; these commands install nothing:

```
uv run --offline --all-packages --extra test python scripts/database/check_successor_proposals.py
uv run --offline --all-packages --extra test pytest -q --no-cov tests/database/test_canonical_successor_proposals.py
```

The optional `--preview-output /absolute/external/new-directory` creates only an
exclusive external authority-source preview. Destination names are allowlisted
before writing; no absolute/parent-traversal paths, symlink ancestry, existing
output, input-tree destination or source overwrite are accepted. No apply flag
exists. It never reads credentials or opens a connection. Use the separate
activation source view when testing generated Python/native consumer contracts.

## Actual-role gate, prepared but not executed

After all018–020 reference files are measured and bound to the exact source, run:

```
uv run --offline --all-packages --extra test python scripts/database/probe_successor_roles.py --root /absolute/measured-candidate --run-approved-isolated-probes --database schema_successor_verify_UNIQUE --dataset schema_successor_probe_UNIQUE --process-rule EXISTING_SYNTHETIC_RULE --other-dataset schema_successor_probe_SECOND
```

Use lowercase unique suffixes of8–48 characters. Supply only the five separate
existing application DSNs through their normal `AI_GATEWAY_DATABASE_*_DSN`
variables. No admin/migrator DSN is read. The tool refuses optimized Python,
checks actual `session_user=current_user`, exact database and loopback endpoint,
and never changes role. Fixtures require a pre-authorized dedicated active
synthetic Knowledge dataset/process rule and a second distinct existing synthetic
dataset in that disposable database. The second dataset lets the negative test
prove the exact document/dataset composite FK, without a missing-parent false positive. The tool
creates no persistent fixture and runs no DDL in this mode. All document/task
DML rolls back; event sequence increments may remain and are disclosed.

It executes five genuine-login ACL matrices;22 cases of the exact production
requeue SQL extracted from `migration_knowledge_ingest/sql.rs` plus a composite-FK
negative under worker login; API event INSERT/UPDATE/DELETE/no-op/payload checks;
and bounded Gateway ledger claim/fence/terminal SQL probes. Worker retains no
progress-event SELECT; API payload assertions are separately labelled. The
ledger probe tests schema SQL behavior, not execution of native ledger methods.

A separately approved `--run-approved-temporary-shadow-probes` adds rollback-only
session-temporary relation/type DDL. If the existing application role lacks TEMP,
it reports that denial and does not claim shadow execution. No privilege is added.

Still required separately: foreign-table trigger attachment negative, seven-day
and cross-dataset retention, actual native/Python startup-before-writes failure
injection, exact native ledger transaction execution, multi-connection claim and
cancellation races, Redis logout/reset/expiry, process-kill/unknown-outcome behavior,
current authenticated HTTP and Runtime/Worker/provider acceptance. Current socket
and ptrace restrictions remain respected; no alternate listener route is attempted.
