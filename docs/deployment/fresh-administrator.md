# First administrator in a fresh development database

Fresh schema initialization deliberately installs only schema and immutable
system reference rows. It does not create mutable tenants, accounts, providers,
models or prices. The Gateway's existing password initialization updates an
already existing administrator; it cannot make an empty database usable alone.

After the native fresh schema authority succeeds, the independent native
`admin-create --fresh-development` one-shot provisions the first account. Run it
in the owned operational container with the Gateway's canonical `DATA_GATEWAY_*`
file/mTLS configuration, only its own service credential volume, and a bounded
JSON object containing `email` and `password` on stdin. The object is private:
never put it in an argv, build input, committed file, report or log. It uses the
configured `AUTH_ALLOWED_EMAIL_DOMAIN`, defaulting to `example.com`.

The command verifies the actual `ai_gateway_gateway` role, connection budget,
physical database `gateway_dev_fresh` and owner-installed fresh catalog reader.
It refuses legacy inline credentials, another database, a transaction-pool alias,
an incompatible/drifted fresh schema and malformed/oversized input. The password
must satisfy the normal account policy and is hashed with the existing bounded
Argon2id engine.

One transaction serializes first-account initialization, creates an absent
active `default` tenant, creates administrator `admin`, and installs its current
`gateway.user_roles` binding to the canonical system `admin` role. Existing
system role/permission rows are verified, never edited. The account retains
`force_password_change=true`; complete the normal first-login password change
before existing-account browser acceptance.

Repeated execution only verifies an already matching active administrator,
active tenant, nonempty password hash and current unexpired admin role binding.
It never resets credentials, renames/reactivates an account, promotes another
user or reinstates a revoked role. A conflicting nonempty account database fails
closed. Commit uncertainty requires reconciliation, never an automatic reset.
The legacy mode remains explicit and retains its duplicate-row failure behavior.

The native development packager's `auth-bootstrap` component packages only the
already compiled `admin-create` executable and its runtime dependencies. Its
public `image.env` contains `ADMIN_BOOTSTRAP_IMAGE`; this is a one-shot image,
not another resident microservice. Packaging performs no compilation or account
writes. Real account creation, repeated execution, conflict rejection and login
are separate integration evidence, not established by unit tests or image build.
