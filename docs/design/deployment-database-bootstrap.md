# Explicit fresh-deployment database bootstrap

Root Compose now runs PostgreSQL health → `database-bootstrap` → `migrate` →
application readiness. The one-shot bootstrap uses the same authority image and the
explicit administrator DSN; migrate receives only its scoped login. No application
or migrator obtains bootstrap credentials, and no Docker socket is mounted.

The public command is:

```
database-authority deployment-bootstrap --expected-database gateway --expected-admin postgres
```

The names are deployment inputs, not inferred targets. The command requires the
validated `AI_GATEWAY_ROLE_PREFIX` and seven distinct explicit
`AI_GATEWAY_DATABASE_{ADMIN,MIGRATOR,GATEWAY,RUNTIME,CAPABILITY_WORKER,KNOWLEDGE_API,KNOWLEDGE_WORKER}_DSN`
values. All must address the same explicit database/host/port/TLS mode with their
assigned identities and independent 32–1024 character printable passwords. Query
options that redirect endpoints or roles are refused. Errors/receipts never expose
DSNs, plaintext passwords or SCRAM verifiers.

This command is separate from `initialize-local-credentials`. That command retains
its existing repository/Compose ownership and numeric loopback guards. The public
command refuses `AI_PLATFORM_LOCAL_DEV_CREDENTIALS=true`; it cannot replace the
`.env.dev` workflow or claim that a DNS endpoint is a local owned container.

Before DDL the administrator connection must prove current_user=session_user equal
to the explicitly named bootstrap administrator, SUPERUSER and the expected database.
The operation uses the migration advisory lock, finite connection/statement/lock
budgets and a 120-second operation deadline. Only an empty admin-owned database
without preexisting application schemas may receive a new canonical role scope.
Foreign schemas, partially existing/unrecognized prefixed roles, incorrect role
attributes/memberships/default ACLs, or nonempty unadopted databases are refused.
A nonempty adopted deployment must pass frozen baseline/epoch verification before
credential work. This is not an automatic repair or legacy cutover command.

Fresh provisioning reuses the frozen native role and extension primitives. Role
creation, extension preparation and absent-password initialization share an outer
transaction; pg_authid is locked to serialize the role/password check. Existing
SCRAM passwords must match every configured login and are preserved. Only NULL
passwords receive newly salted SCRAM verifiers; mismatches fail and never rotate
credentials. After commit all seven real role connections are verified before the
job emits a pass receipt. A post-commit connectivity failure leaves prepared
credentials intact and fails the job; a retry verifies and preserves them.

The bootstrap installs no application baseline or successor. Its scoped migrator
performs that separately using the existing frozen authority, followed by normal
schema admission. Runtime/Worker's pending reader successor remains required.
No frozen SQL, fingerprint or ledger is rewritten by this implementation.

Source verification: Rust formatting and public Compose structure checks passed.
Added unit cases cover missing/redirected/shared/wrong-role DSNs, wrong expected
admin/database, local-only opt-in refusal, unknown/partial/unverified scopes,
role-switched/non-superuser sessions and NULL-only/nonrotating SCRAM decisions.
Cargo/unit execution and fresh/adopted/rollback real-DB rehearsals are primary-owned
and have not run for this source change. No DB or Docker action was performed.
