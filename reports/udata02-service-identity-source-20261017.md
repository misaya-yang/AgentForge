# U-DATA-02 deployment identity source implementation — 2026-10-17

Source base: 316b96a67b554cf64fa53e97f5551e873910b769 with concurrent upgrade
implementation changes. This receipt covers only the identity wiring owned by
sol_admission; it is not an integrated source-lock/build/live acceptance receipt.

Implemented: root Compose uses distinct service and migrator role credentials;
Knowledge worker overrides the shared API environment with its own DSN. Local
Runtime/Worker honor the canonical prefix. Bootstrap credentials stay only with
PostgreSQL/admin paths. Public examples and the existing shell/native initializer
prepare private independent role credentials and scoped host application DSNs.
Gateway/Knowledge DSNs and Runtime/Worker options set finite session timeouts;
existing pool controls remain explicit. Helm workloads consume assigned DSN keys
from a distinct required Secret, never bootstrap identity variables. Legacy
LangGraph requires independent preprovisioned database identities; its admin psql
creation hook is removed. The retired Helm backend remains pending U-PE-30.

New Rust tools module database_identity implements rendered Compose own-role and
Kubernetes Secret-key isolation checks with positive/negative unit tests. Tools
main/CI registration and execution are primary-owned pending integration. The
unnumbered connection-policy proposal preserves the frozen authority boundary:
server-side per-role limits/timeouts require reviewed activation and measurement;
no DB or fingerprint was changed in this task.

Executed and passed:
- `bash -n scripts/new/init-env.sh`.
- YAML parsing of root/local/overlay Compose and Helm values with installed PyYAML.
- Isolated initializer fixture generated six unique 64-character hexadecimal role
  passwords, a scoped host Gateway DSN and mode 0600. Values were not emitted.
- Independent parsing/rendering of database expressions in root Compose with that
  isolated fixture confirmed all six service/migrator role mappings, including the
  Knowledge worker override. Static Helm inspection confirms bootstrap variables
  removed from application/migration templates.
- rustfmt parsing/format checks on touched Rust modules, plus scoped diff whitespace.

Failed/limited:
- The initializer fixture command returned zero but emitted missing platform-tools
  errors from existing runtime image helpers. Only the independently verified
  database fixture properties above passed; complete initializer/image output is
  not certified.
- `--if-missing` rerun stopped at those existing runtime helpers with platform-tools
  unavailable on the isolated PATH. Password preservation is not claimed as an
  executed passing gate (implementation preserves existing values).

Not run: Cargo build/unit tests, new gate execution, Helm template render (helm is
not installed on PATH), Docker/Compose real profile resolution, role provisioning,
credential changes, SQL activation, raw fingerprint verification, real-role service
startup, timeout/connection-limit measurements or negative cross-schema DB tests.
See docs/design/service-database-identities.md for primary wiring and live gates.
