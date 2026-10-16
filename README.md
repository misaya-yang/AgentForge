# AI Gateway / AgentForge

Enterprise AI gateway, Assistant and knowledge platform. Platform services and operational
tools are Rust; the console is React/TypeScript. An optional Python user-code sandbox is
retained for scientific workloads.

## Rust source

Start with the [native Cargo workspace](rust/platform-native/Cargo.toml).

| Component | Source |
| --- | --- |
| Gateway, authentication and public API | [platform-gateway](rust/platform-native/crates/platform-gateway/src/bin/gateway.rs) |
| Knowledge API and worker | [platform-knowledge-service](rust/platform-native/crates/platform-knowledge-service/src/main.rs) |
| SDK Agent Worker | [platform-sdk-agent-worker](rust/platform-native/crates/platform-sdk-agent-worker/src/main.rs) |
| Database migration authority | [platform-migration-authority](rust/platform-native/crates/platform-migration-authority/src/lib.rs) |
| Local Node and clients | [platform-local-node](rust/platform-native/crates/platform-local-node/), [platform-client](rust/platform-native/crates/platform-client/) |
| Assistant Runtime and capability worker | [agent-runtime-overlay](rust/agent-runtime-overlay/) |

The Runtime overlay composes a pinned upstream Rust source tree. Its source lock and build
commands are in [deploy/agent-runtime-source](deploy/agent-runtime-source/). Generated
binaries, downloaded upstream source, credentials and build caches stay outside the repository.

## Repository layout

```text
.
├── rust/
│   ├── platform-native/          # Cargo workspace: services, clients and tools
│   └── agent-runtime-overlay/    # Assistant Runtime and capability worker
├── web/                          # React console and browser tests
├── apps/                         # Service packaging and native OS helpers
├── sdk/                          # SDK contracts and client delivery
├── database/                     # Frozen baseline, migrations and verification
├── deploy/
│   ├── local/                    # This project's local infrastructure/runtime
│   ├── compose/                  # Optional deployment/build overlays
│   └── runbooks/                 # Active programs and continuation state
├── config/                       # Public configuration templates
├── scripts/                      # Shell entry points for Make/operations
├── tests/                        # Shared contract/evaluation fixtures
├── docs/                         # Architecture, guides and handoffs
├── reports/                      # Dated verification evidence
├── src/, packages/               # Shared static catalogs/evaluation resources
├── docker-compose.yml            # Primary release deployment
└── Makefile                      # Canonical commands
```

The Git root is this directory. The previous nested `AI--Platfform/` checkout and Python
recovery/backup directories have been removed. Historical reports retain the paths they
measured; they are evidence, not source directories to restore.

## Development

Toolchains are pinned: native Rust 1.99.0, Runtime Rust 1.95.0, Node 26.11.1 and pnpm 12.11.2.
Run from this directory with an external Cargo target:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/agentforge/targets/dots-native-1.99.0"
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
make native-check
make harness-check
# Run the affected package/target before committing native changes.
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-auth --lib
```

Use [the command catalog](docs/harness/commands.md) for frontend checks, native installation,
image builds and live acceptance. Native Gateway needs explicit configuration based on
[the public example](deploy/platform-native/native-options.example.json).

For the existing development environment, keep `COMPOSE_PROJECT_NAME` in the selected ignored
`.env.dev` and use `make dev-status` / `make dev-start`. This preserves the project and volumes
when the checkout path changes. The local infrastructure definition is `deploy/local/compose.yml`;
Runtime/Worker integration uses `deploy/local/compose.runtime.yml`. Do not start a second stack.

For a new release installation, see [DEPLOY.md](DEPLOY.md) and the
[operator guide](docs/deployment/operator-guide.md). Previously published images are release
artifacts; they do not prove the current source has passed acceptance.

## Verification status and continuation

The Rust source import is complete. Full product acceptance is still in progress. Source
presence and compilation do not establish working Assistant, retrieval, desktop or deployment
journeys. See the [current acceptance evidence](reports/native-current-acceptance-20261016/),
[layout checkpoint](reports/repository-layout-20261016.md), and
[migration status](docs/migration/full-rust-status.md) for the measured scope and pending work.

Read [AGENTS.md](AGENTS.md) before editing, [the documentation index](docs/README.md) for area
ownership, and [CONTRIBUTING.md](CONTRIBUTING.md) / [SECURITY.md](SECURITY.md) for contributions
and vulnerability reporting.
