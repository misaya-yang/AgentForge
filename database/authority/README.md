# Native authority and ledger ACL revision v1

**Last verified:** 2026-10-17 (source/command review; no live acceptance claim).

The Rust [`database-authority`](../../rust/platform-native/crates/platform-migration-authority/README.md)
is the only schema writer. Frozen baseline and epoch artifacts are immutable;
`ledger_acl_v1.json` is a separate digest-pinned bootstrap contract. Fresh installation
verifies the baseline and applies ledger bootstrap before recording adoption.

An already adopted database uses the explicit admin-only command:

```sh
database-authority bootstrap-ledger-acl
```

It reads `AI_GATEWAY_DATABASE_ADMIN_DSN`, holds the migration advisory lock, proves
pre-bootstrap fingerprints and emits a nonsecret receipt. Normal migrate/status/verify
use the separate migrator DSN and never retry as admin. Command existence does not
authorize a shared-data mutation.

## Exact ACL contract

Only the migrator gains public-schema USAGE, without CREATE or grant option.
The named ledger rights are:

- `platform_schema_baselines`: INSERT, SELECT
- `platform_schema_changes`: INSERT, SELECT
- `platform_schema_change_attempts`: INSERT, SELECT, UPDATE

No DELETE, TRUNCATE, REFERENCES, TRIGGER, schema CREATE, application-data grant,
PUBLIC grant, new membership or application-role grant is added. The owner remains
NOLOGIN. Schema/condition reads and DDL borrow owner only within bounded scopes;
ledger DML runs after restoring the connected migrator role.

A version/digest/role-prefix receipt is stored in the baseline-ledger table comment.
Missing/tampered receipts, extra table/column grants, grant options, unexpected
owners/grantees/grantors and partial grants fail closed. Raw fingerprints remain raw:
compatibility requires the exact canonical public-USAGE line and proves every other
ACL line against the frozen baseline/epoch hash. This is an additive proof, not
permission drift hidden by filtering.

The repair is bootstrap because a migrator lacking ledger access cannot discover
or record an ordinary successor. Do not change fingerprints to bypass it.

## Current native admission

The current source manifest is epoch 22. Gateway/Knowledge application-role admission
uses the owner-installed metadata reader before providers, workers or readiness:
Gateway 18–22; durable Gateway 20–22; Knowledge API 19–22; Knowledge Worker 21–22.
It checks baseline-marker identity, the exact contiguous change/checksum prefix and
unresolved attempts. `database-authority verify` separately recomputes full fingerprints.

Runtime/Capability Worker are not wired to this guard yet; their CLI startup branches
retain legacy 2–10 checks and cannot certify epoch 22. This is tracked under U-GW-20
in the [active program](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json).
No current-database or live-acceptance result follows from the source window.

Successors 018–022 have source activation receipts in
[`database/migrations/2026_08_post_kb_v1/`](../migrations/2026_08_post_kb_v1/).
Successor023 remains a staged proposal. Read the
[successor-chain design](../../docs/design/canonical-schema-successor-chain.md) and
the native authority's actual commands before preview or measurement. Preserve all
historical receipts; retired Python services are not current startup or rollback profiles.
