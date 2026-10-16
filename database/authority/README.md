# Database authority

The Rust `database-authority` binary is the supported migration and verification
entry point. The frozen baseline, SQL migrations, manifests, and fingerprints
remain the authority inputs; the former Python `database.authority` implementation
has been removed. References to `python -m database.authority ...` are obsolete.
Do not rewrite frozen SQL or treat its historical Python origin as a live Python
service.

At this checkout checkpoint, the active canonical manifest and main database
are at epoch 024. Migration, native authority verification, and actual-role
Local Node column-SELECT checks passed. See the [active manifest](../migrations/2026_08_post_kb_v1/manifest.yml),
[024 activation receipt](../migrations/2026_08_post_kb_v1/SUCCESSOR_024_ACTIVATION.json),
[native Local Node authority evidence](../../reports/native-current-acceptance-20261016/schema-024-real.json),
[successor-chain design](../../docs/design/canonical-schema-successor-chain.md), and
[024 source plan](../../docs/design/canonical-schema-successor-024.json). The
plan's inactive-candidate marker records its pre-activation state; the activation
receipt and manifest describe current status. For normal migration, role, and
ACL operations, see the [native authority guide](../../rust/platform-native/crates/platform-migration-authority/README.md).

## Native CLI

The binary's command catalog and option syntax are defined by `--help`:

```sh
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin database-authority -- --help
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin database-authority -- status
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin database-authority -- verify
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin database-authority -- startup-check \
  --principal knowledge_worker
```

The CLI also lists `migrate`, `init-fresh`, `cutover`, `source-kind`,
`fingerprint`, `provision-roles`, `provision-extensions`,
`bootstrap-ledger-acl`, `prepare-cutover-ownership`, and
`verify-role-connections`. `status`, `source-kind`, `verify`, `fingerprint`, and
`startup-check` are read-only. Migration and bootstrap operations require the
usual reviewed deployment procedure; running a command or compiling the binary
does not constitute database acceptance.

Every connection uses an explicit `AI_GATEWAY_DATABASE_<PRINCIPAL>_DSN`, with
`AI_GATEWAY_ROLE_PREFIX` and the repository or packaged database-artifact
directory configured separately. There is no shared `DATABASE_URL` or retry as
admin.

## Role boundary

- The object owner is `NOLOGIN`. The migrator runs ordinary migration and holds
  the existing session advisory lock. Where DDL or metadata reads require
  ownership, the runner uses bounded owner scopes and restores the migrator
  identity before migration-ledger DML.
- The separate admin DSN is required for role and extension provisioning,
  cutover-ownership preparation, and the one-time ledger ACL bootstrap. Ordinary
  migration, status, and verification do not fall back to admin.
- Application principals are `gateway`, `runtime`, `capability_worker`,
  `knowledge_api`, and `knowledge_worker`. `startup-check --principal NAME`
  connects as that application's own role. Application roles do not inherit
  owner or migrator authority. `verify-role-connections` checks that configured
  DSNs are distinct and authenticate directly as their named roles.

## Ledger ACL revision v1

[`ledger_acl_v1.json`](ledger_acl_v1.json) is the digest-pinned bootstrap
contract. Frozen baseline and epoch files stay unchanged. The migrator receives
public-schema `USAGE` only, without `CREATE` or grant option, and these exact
ledger rights:

| Table | Migrator rights |
| --- | --- |
| `platform_schema_baselines` | `INSERT`, `SELECT` |
| `platform_schema_changes` | `INSERT`, `SELECT` |
| `platform_schema_change_attempts` | `INSERT`, `SELECT`, `UPDATE` |

No delete, truncate, references, trigger, schema-create, application-role, or
`PUBLIC` grants are added. A canonical version/digest/role-prefix receipt is
stored in the baseline ledger table comment. Verification checks the exact
additive public-schema ACL line and proves that all remaining raw ACL
fingerprints still match the frozen baseline and epoch artifacts; unexpected
owners, grantors, grantees, rights, grant options, partial grants, or a missing
or changed receipt fail closed.

This repair is an explicit admin-only bootstrap before epoch discovery, not a
normal epoch: the pre-repair migrator cannot read its ledger to discover and
record that epoch. Fresh installation applies the same verified contract before
writing its adoption marker.
