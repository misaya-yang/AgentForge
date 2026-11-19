# Native candidate source assembly

This opt-in assembly builds native Gateway and the independent native Knowledge
binary, but **runs retained Python Knowledge API + worker**. It preserves the
[hybrid ownership contract](knowledge-native-python-hybrid.md), existing Runtime
and capability-worker builders, Python migration authority, Web and optional
Python SDK worker. Default quickstart/source-build remains the Python Gateway
lane and is not proof of the epoch-17 native candidate.

## Safe source-only entry point

From a verified checkout or verified recovery source archive, run:

```sh
python3 scripts/harness/native_candidate_plan.py
python3 scripts/harness/native_candidate_plan.py --sdk
```

These commands print argument arrays and do not run Docker, read env/config
secrets, contact providers, migrate or start anything. Placeholder paths require
operator substitution. The plan is not a deployment command. Before executing
any build, follow [runtime ownership and resource rules](../harness/runtime-and-secrets.md),
set `COMPOSE_PARALLEL_LIMIT=1`, check disk/memory, and verify existing container
labels. Use a unique local `NATIVE_GATEWAY_IMAGE`, `KNOWLEDGE_IMAGE`,
`MIGRATE_IMAGE` and `FRONTEND_IMAGE`; with SDK opt-in also set its reviewed
image/token/retention. Reuse the existing Runtime/Worker source receipt and
release-unit verification, not unrelated published images. This plan deliberately
contains no `up`, `run`, `migrate`, cleanup or publication operation.

## Resolved native input is mandatory

Set `NATIVE_GATEWAY_CONFIG_FILE` to an existing, operator-reviewed complete
`GatewayDocument` JSON outside the source tree. Keep it owner-readable and ensure
container UID 1000 can read the runtime secret mount. Compose local secrets are
file mounts, not a promise of an encrypted secret store. Never commit or attach
this file. Native Gateway does not consume the base Compose environment. Its
settings must already be resolved according to `bootstrap/configuration.rs`;
there is no implicit `.env` exporter. Use the explicit offline preparation path below. Without such a reviewed document, runtime
configuration remains blocked even when source builds succeed.

Required relationships within the explicit document:

- `environment.AI_PLATFORM_TOPOLOGY_MODE` must explicitly be `full`; the native
  architecture projection does not inherit the ambient Compose setting.
- `settings` supplies the reviewed PostgreSQL/Redis/auth/storage/server settings;
  bind Gateway to `0.0.0.0:8080` to match existing health checks and internal routes.
- `environment.KB_SERVICE_URL` is `http://knowledge-service:8092`, and
  `native.allow_private_knowledge_http` is explicitly true only for this approved
  private Compose network. Python Knowledge remains the only Knowledge owner.
- Keep Python `GATEWAY_URL=http://gateway:8080`; all Knowledge consumers, including
  capability worker, must resolve to that same Python owner. Keep API/worker roles
  from the full topology and their existing shared storage and ingestion state.
- Match both sides' internal primary secret, signing keyring/active id and Redis
  replay protection. Do not log or place those values in build arguments.
- `native.kernel_revision` matches the verified Runtime/overlay source identity;
  Runtime origin and all capability broker settings must match the selected images.
- SDK opt-in requires `environment.SDK_AGENT_WORKER_URL=http://127.0.0.1:8096`
  and matching `environment.SDK_AGENT_WORKER_TOKEN` in this JSON. The SDK overlay's
  ambient Gateway environment alone cannot configure native Gateway. Retain the
  SDK worker's existing shared network namespace and security controls.
- Poppler tools are `/usr/bin/pdfinfo`, `/usr/bin/pdftotext`, `/usr/bin/pdftoppm`;
  use a reviewed writable scratch directory. No hidden sandbox or provider defaults
  are invented. Keep unsupported local-channel/registration modes disabled.

`gateway --check-config-stdin` validates projection only; it is not a schema,
DB, role, provider, runtime handshake or live-readiness check. The image command
can be overridden to this flag with the same explicit secret mount after build
and ownership authorization. The bundled native `knowledge-service` binary is
for separate reviewed native-Knowledge acceptance; this override never starts it
or splits the Knowledge API between Python and Rust.

## Schema and release boundary

Retain the one-shot `python -m database.authority migrate` owner and its inherited
`service_completed_successfully` dependency. Do not run migrations as an app,
change immutable migration files, expand grants or reuse superuser credentials as
proof of role acceptance. Target the candidate's epoch-17 baseline/migration and
per-process scoped-role evidence before authorizing startup. Python Knowledge's
window is 5–17; retained Python Gateway's is 2–10, so it cannot be the rollback
process for this database. Native process-wide schema admission remains a separate
tracked gate; successful build or config projection does not close it. See
[architecture contract](../harness/architecture.md) and the migration authority docs.

## Restored source identity

The main project may be a verified archive without `.git`. Harness root discovery
is anchored to the script location and checked repository markers, so it neither
requires `git init` nor accidentally selects a surrounding unrelated Git repo.
The separate `AI_PLATFORM_AGENT_RUNTIME_SOURCE` must still be a genuine clean
controlled upstream checkout containing the pinned commit. Its archive/receipt,
overlay hash and supply-chain checks remain mandatory and unchanged.

## Verification truth

Static source/Compose parsing, archive-root tests and printed-plan tests do not
establish Docker build success, multiarchitecture images, live epoch/role admission,
SDK registration, provider availability, API/UI journeys or deployment readiness.
Those remain local Codex/operator gates after ownership/configuration approval.

## Offline configuration preparation

Use the project's installed Python workspace dependencies, without installing new
packages for this step. Prepare a private external JSON object mapping environment
names to string values, using reviewed **container** addresses and matching the
retained Knowledge/Runtime credentials. This is explicit input, not an ambient
shell or `.env` scan. Copy `deploy/platform-native/native-options.example.json`
outside the tree and replace its placeholders with approved native deployment
choices; it is intentionally not a runnable credential/configuration sample.

```sh
uv run --no-sync python scripts/harness/prepare_native_gateway_config.py --environment-json /private/container-environment.json --native-json /private/native-options.json --output /private/native-gateway.json
```

The preparer reuses `src.config.settings.Settings(_env_file=None)` and its actual
`model_dump(mode="json")`, with only the explicit map visible to settings loading.
It verifies Gateway port, PostgreSQL/Redis endpoints, disabled auto-init, native
Knowledge/Runtime/Worker origins and the optional SDK loopback. It creates an
exclusive mode-0600 file outside the source tree and never prints values or raw
validation exceptions. It does not prove full Rust projection or dependencies.
Run `gateway --check-config-stdin` with the output on stdin in the authorized
candidate image before attempting startup. Keep all three private files out of
recovery archives, logs and source context; align file readability with UID1000.

Rollback rehearsal remains Git-only because it archives a historical commit; do not run it from a source archive. The higher-level `scripts/rust/build-update.sh` still requires clean Git/HEAD
identity. Its dirty-source guard is not bypassed or weakened for archives. The
printed source plan uses the existing direct Runtime/Worker builders after the
restored archive's manifest verification and ownership/resource checks; those
builders retain controlled upstream Git and overlay verification. Do not claim
that every Git-dependent development command works from an archive.

## Native compiler pin

The native candidate builder uses `rust:1.99.0-bookworm`, matching the current
native product verification toolchain rather than the separate Runtime builder's
1.95 pin. [Rust's 1.99.0 announcement](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/)
and [official Rust Docker image tags](https://hub.docker.com/_/rust?tab=tags)
were checked on 2026-10-13; the official tags list includes `1.99.0-bookworm`.
This verifies the published tag listing, not an image pull/build or this image's
runtime acceptance. Native workspace and Runtime pins remain independent.
