# Native authority file credentials and mTLS

`database-authority` deployment-bootstrap, provision-roles, provision-extensions,
bootstrap-ledger-acl, migrate/cutover/init-fresh, verify, and startup-check resolve
one explicit principal connection inside Rust. Files are never expanded into
process argv or environment variables. Role, owner, privilege, source and isolated
measurement checks remain in their existing authority paths.

Each principal accepts exactly one configuration prefix:

| Principal | Canonical prefix | Common file prefix |
| --- | --- | --- |
| admin | AI_GATEWAY_DATABASE_ADMIN | DATA_ADMIN |
| migrator | AI_GATEWAY_DATABASE_MIGRATOR | DATA_MIGRATION |
| gateway | AI_GATEWAY_DATABASE_GATEWAY | DATA_GATEWAY |
| runtime | AI_GATEWAY_DATABASE_RUNTIME | DATA_RUNTIME |
| capability_worker | AI_GATEWAY_DATABASE_CAPABILITY_WORKER | DATA_CAPABILITY_WORKER |
| knowledge_api | AI_GATEWAY_DATABASE_KNOWLEDGE_API | DATA_KNOWLEDGE |
| knowledge_worker | AI_GATEWAY_DATABASE_KNOWLEDGE_WORKER | DATA_KNOWLEDGE_WORKER |

For the selected prefix set `DSN_FILE`, optional `PASSWORD_FILE`, `TLS_CA_FILE`,
`TLS_CERT_FILE`, `TLS_KEY_FILE`, `EXPECTED_HOST`, `DATABASE_ALIAS` and
`PHYSICAL_DATABASE`. `MODE` is absent or `direct`; the two database names must
match. Authority session locks and owner switching never use a transaction pool.
Admin additionally requires `EXPECTED_ROLE`, matching the explicit
`deployment-bootstrap --expected-admin` argument. Other roles derive exactly from
`AI_GATEWAY_ROLE_PREFIX` and their principal suffix; an optional EXPECTED_ROLE
must match. The DSN must contain an explicit nonzero port, exact role/host/database,
no embedded password, no query string, fragment or startup option. Passwords are
separate file secrets. Bootstrap requires all seven distinct strong passwords,
as before; ordinary role startup also supports certificate-only authentication.

All five credential/TLS material files must be absolute regular, non-symlink,
nonempty UTF-8/PEM as appropriate, at most64KiB and mode0400/0440/0600/0640.
Use secret subPath mounts or pre-resolved owned files rather than projected final
symlinks. TLS always uses VerifyFull with the configured CA, client cert and key;
there is no require/disable fallback in file mode. Seven-role bootstrap and role
verification reject reused client private keys and mixed file/legacy connections.
Server certificate SANs must match EXPECTED_HOST; server pg_hba must validate each
client identity against its dedicated database role.

The original canonical `_DSN` environment interface remains for explicitly selected
legacy local tooling. It cannot coexist with that principal's file DSN. Plaintext
`DATA_*_DSN` and every `_PASSWORD` environment variable are refused. No DSN,
password, PEM or private path is included in file-resolution/connection errors or
Debug output; SQL statement logging is disabled on configured connections.

Bootstrap's existing endpoint/password policy sees a synthesized *in-memory*
canonical DSN snapshot; it does not mutate the environment. The separate local
initializer still requires its exact owned Docker/127.0.0.1 guard. Isolated
successor measurement keeps its numeric-loopback/disposable-database restriction
while carrying the same principal TLS options. File configuration does not grant
permission to measure or migrate any database.

Unit source coverage exercises canonical/common prefixes, arbitrary valid role
prefixes, wrong role/host/physical DB, pooled mode, DSN redirects/startup options,
embedded/environment secrets, conflicting prefixes, missing PEM, private file
modes and symlink refusal. Live certificate chains, pg_hba and per-role connection
acceptance remain the deployment owner's explicit real-service acceptance.

## Secure deployment without a TCP superuser

The standalone read gate is:

```text
database-authority deployment-file-admission --expected-database gateway --phase pre-migration
database-authority deployment-file-admission --expected-database gateway --phase post-migration
```

`--phase` defaults to `pre-migration`. No `--principal`, `--expected-admin`, local
credential opt-in, adoption flag or mutation/evidence flag is accepted. Only the
five business roles plus migrator are required; **admin files are not read**.
Use their direct backend DSNs, distinct password/client-key files and the prefix
mapping above. Pooled service DSNs require separate direct authority/preflight DSN
files for the same principal. Common pooled application MODE values cannot be
reused for authority sessions. No admin TCP exception or HBA change is needed.

An independent, separately authorized operator **local authority** stage must
first provision roles, role memberships, passwords, canonical schema ownership/
CREATE/default ACLs and exact extensions. The pre-migration gate verifies that
prep through six actual unprivileged mTLS sessions, exact current/session role,
expected physical endpoint/database, forbidden role attributes and private
catalog/owner/ledger isolation. Client TLS configuration alone is insufficient:
each backend's own `pg_stat_ssl` row must contain actual authenticated client
certificate evidence. Shared client keys/passwords are refused. The migrator's
owner membership must have SET but no ADMIN/INHERIT option. An authority advisory
lock prevents overlap with a normal migrator; closing releases it on every path.

Allowed pre-migration states are an actually empty, operator-prepared database
with exact extension identities, or an adopted exact existing source prefix with
all four current fingerprints, ledger names/checksums and postconditions intact.
Partial/foreign ledgers, unknown nonempty databases, future epochs, drift and any
migration-attempt row are refused. Passing this phase reports
`business_schema_admitted=false`, `operator_bootstrap_performed=false`,
`admin_connection_used=false` and `database_mutations=false`. It does not claim
fresh installation, password initialization, migration or service startup.

Next run the separately authorized migrator `init-fresh`/`migrate` stage, using
only its file/mTLS identity. Finally the post-migration gate requires the exact
compiled source epoch and repeats normal own-role metadata-reader admission for
each business role, plus active service-schema minimal/forbidden ACL checks.
Applications retain their own startup checks. A missing metadata reader or denied
reader never falls back to migrator/admin/ledger access. Superuser provisioning
is not emulated by either phase. The gate emits one final JSON result only after
all six sessions pass; no credential or certificate identity is emitted.
