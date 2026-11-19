# U-DATA-02 service database configuration

Root Compose uses distinct canonical `gateway`, `knowledge_api`, `knowledge_worker`,
`runtime`, `capability_worker` and `migrator` logins. `POSTGRES_USER/PASSWORD` are
infrastructure/bootstrap inputs only. `init-env.sh` creates six independent private
`LOCAL_DB_*_PASSWORD` values, preserves existing passwords with `--if-missing`, and
replaces only the historical loopback bootstrap Gateway DSN. It does not provision
or rotate database credentials. The existing native `local-dev-env` preparation
keeps credential-verification DSNs query-free and applies finite timeouts to host
application DSNs. No private env file is checked into source control.

Provisioning remains explicit native authority work: `provision-roles`,
`provision-extensions`, fresh-install/cutover and credential initialization must use
their documented separate identities and verification. `initialize-local-credentials`
remains restricted to its owned loopback local target. Remote/Helm operators provision
and deliver scoped credentials through their approved native authority workflow;
there is no admin fallback when a scoped login is unavailable. Existing volumes and
frozen baseline artifacts are preserved.

Compose DSNs include current approved role schema order and 30-second statement /
15-second idle-transaction session timeouts. Runtime/Worker use the same finite
options with `pg_catalog,assistant` on both admission and business connections.
Pool caps remain explicit (Gateway/Knowledge 10, Runtime 10, Worker 4 by default).
Schema ACLs remain authoritative. Current role-level search paths are fingerprinted;
`service-role-connection-policy.sql.proposal` is an unactivated, unnumbered proposal
for per-role connection limits and server-side timeouts. It requires a separately
reviewed native authority/admin transition, full raw fingerprint measurement, source
activation and live verification; session options do not establish that acceptance.
No script runs this proposal and no frozen fingerprint is rewritten.

Helm requires `databaseRoles.existingSecret`, separate from the chart bootstrap
Secret. It must contain URL-encoded complete `GATEWAY_DSN`, `KNOWLEDGE_API_DSN`,
`KNOWLEDGE_WORKER_DSN` and `MIGRATOR_DSN` values with the corresponding canonical
service identities and connection options. Pods receive only their assigned key.
Enabled legacy LangGraph agents require separately provisioned isolated databases
and `LANGGRAPH_<NAME>_DSN` keys; the former admin-owned `psql` database-creation hook
is removed. This does not certify the retired Helm backend as native release-ready;
U-PE-30 still owns its replacement.

The new `platform-tools` module `database_identity` checks rendered Compose own-role
DSNs/split identities, rejects bootstrap/admin/foreign service identities, and checks
rendered Kubernetes workload Secret-key isolation. It never reads private env files,
retrieves cluster Secrets or emits DSN values. Primary integration must register
`database-identity-gate` → `database_identity::run` in tools main, then run it with
`--config <rendered-compose.json>` (and `--role-prefix` when overridden), or
`--format kubernetes --config <rendered-helm.yaml>` and wire both profiles into CI.
The module includes positive/negative role, missing input, unresolved substitution,
admin side-channel and Kubernetes wrong-key/whole-secret tests.

Implementation is not live acceptance. Required remaining gates: compile/unit tests,
Helm rendering, all supported Compose profile rendering, per-service own-role startup,
negative cross-schema reads/writes, timeout/connection-limit measurement and authority
fingerprint verification. Runtime/Worker also require the separately staged reader
successor before their strict admission can pass.

Root Compose fresh installation now has an explicit, separately scoped
[`deployment-bootstrap`](deployment-database-bootstrap.md) administrator job before
migrate. It provisions canonical roles/extensions and absent SCRAM credentials;
nonempty adopted databases require full frozen verification and no repair occurs.
The original local-only initializer and ownership guards remain separate.
