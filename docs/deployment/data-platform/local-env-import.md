# Import existing local credentials into file and mTLS inputs

`platform-data-security prepare-local-env` reads the existing private local env
file itself. It preserves the five application login passwords, the migrator
login password and the `postgres` bootstrap password. It calls the existing real
PKI preparer; it never connects to PostgreSQL, executes SQL, rotates passwords,
installs an HBA, starts a service or exports provider fields. `prepare` with the
original `--password-files` and `--bootstrap-password-file` inputs remains usable.

Run the controlled operational binary built by the primary. This command is an
explicit preparation operation; it invokes the selected existing OpenSSL binary.
The output parent must exist and the output directory must be new. The following
local paths and OpenSSL executable were checked to exist; no preparation was run
while implementing this mode. The CIDRs are preparation examples: verify the
actual project's selected networks before installing the generated configuration.

```sh
platform-data-security prepare-local-env \
  --repo-root /Users/yang/projects/AgentForge \
  --local-env /Users/yang/projects/AgentForge/.env.dev \
  --directory /Users/yang/.cache/agentforge-upgrade/data-private-fresh-20261018-01 \
  --database gateway_dev_fresh \
  --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24 \
  --openssl /opt/homebrew/opt/openssl@3/bin/openssl
platform-data-security validate-local-env \
  --directory /Users/yang/.cache/agentforge-upgrade/data-private-fresh-20261018-01
```

No env file is sourced, interpolated or passed as an environment to a child.
Arguments contain paths and public configuration only. The importer accepts an
owned, single-link regular UTF-8 file of at most 1MiB, with exactly0400 or0600
permissions. Final and ancestor symlinks, hardlinks and special files are refused.
Recognized assignments support literal bare, single-quoted or double-quoted
values and an optional `export ` prefix; no shell evaluation, dotenv variable
expansion, backslash unescaping or multiline value syntax occurs.

All seven `AI_GATEWAY_DATABASE_{ADMIN,MIGRATOR,GATEWAY,RUNTIME,CAPABILITY_WORKER,
KNOWLEDGE_API,KNOWLEDGE_WORKER}_DSN` entries are mandatory. The admin identity is
`postgres`; the other six identities must be their canonical `ai_gateway_*`
roles. All legacy URIs must identify one identical existing source database, one
canonical numeric loopback address and one explicit nonzero port. The explicit
`--database` selects the **output target**, which may differ from the old source:
`gateway_dev_fresh` therefore reuses existing credentials read from old `gateway`
DSNs without rewriting them or requiring the new database to exist. The receipt
records the public source/target names; all generated role DSNs, pooler/HBA
configuration and deployment paths bind the explicit target. This command never
creates either database; the native fresh-schema authority owns that operation. `localhost`, external
addresses, query/fragment overrides, omitted passwords, malformed percent
encoding, invalid UTF-8, decoded control characters and role/database mismatches
are rejected. URI percent encoding is decoded exactly once; `+` stays a plus.
Reserved password characters must use URI percent encoding. Passwords containing
Unicode are supported through UTF-8 percent encoding.

Repeated recognized assignments fail even if identical. If present,
`POSTGRES_*` identity/password aliases, `LOCAL_DB_*_PASSWORD`,
`AI_GATEWAY_ROLE_PREFIX`, `GATEWAY_DATABASE__DSN` and `KNOWLEDGE_DATABASE__DSN`
must agree with the selected canonical **source** URIs, including the old
`POSTGRES_DB`; they do not override the explicit output target. Unknown role-password names and
unknown `AI_GATEWAY_DATABASE_*` authority assignments fail closed. Other fields
are ignored without exporting their values. Multiline quote/continuation syntax
is rejected across the whole file so provider text cannot impersonate standalone
database assignments. All errors are fixed
redacted descriptions; URI/parser/OS error text is never forwarded.

The new0700 output tree contains0600 files only:

- `inputs/`: the original six login passwords and bootstrap password in separate
  files; `imported-password-paths.json` lists the six login paths.
- `password-files.json`: the exact five canonical application password-file map,
  compatible with the original preparation entry point.
- `material/`: the existing PKI preparer's actual CA, nine server/client
  identities, passwordless role DSNs, SCRAM auth file, HBA and pooler config.
- `authority/{migrator,gateway,runtime,capability_worker,knowledge_api,knowledge_worker}/`:
  role-specific passwordless direct DSN, existing password and copied matching
  client certificate/key/CA. These directories match the existing authority
  mounts; the issuer key is excluded.
- `deployment-paths.json`: `DATA_PRIVATE_DIRECTORY=.../material`,
  `DATA_AUTHORITY_DIRECTORY=.../authority`, physical database and bootstrap/map
  paths, for the existing data-security Compose overlay. This JSON contains
  paths/public identities only; it is not an application environment file.
- `authority-connection-files.json`: role-specific file/mTLS connection metadata.
  `import-receipt.json` records preparation only, without credentials or URIs.

The outer `INCOMPLETE` marker is removed only after the actual PKI preparer
succeeds and the wrapper verifies PKI copies, password copies, path maps, direct
DSN identities, ownership and all file/directory permissions. Failures preserve
the incomplete tree and never replace any prior file. Use a distinct new output
path for a new attempt. `validate-local-env` repeats this offline integrity
validation; its success does not establish database authentication, service UID
readability, network installation, migration authorization or product acceptance.
Keep issuer/inputs/bootstrap and authority role directories away from application
mounts, and install only the exact selected service material under its reviewed
UID/GID. See the [file-secrets rollout](README.md#private-material-and-file-secrets-rollout).

Meaningful parser, permission, role, encoding, duplicate/conflict, symlink,
hardlink, special-file, overwrite and redaction tests live in
`data_security/local_env/tests.rs` and `tests/data_security_cli.rs`. They use
synthetic credentials and invoke no OpenSSL or database. Run them only through
the primary's shared native build/test workflow. No new dependency, tools-main
registration or Runtime source-package update is required.
