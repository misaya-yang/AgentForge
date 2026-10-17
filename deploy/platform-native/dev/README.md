# Docker-only native development compilation

Use the existing Docker daemon/context and the single `agentforge-dev-7801d4a9a4` project.
This compiler role starts no application or infrastructure service and creates no builder.
Daily registry/git/target state lives in **one** Docker volume,
`agentforge-dev-7801d4a9a4_native-dev-cache`. Release BuildKit caches remain separate,
explicit release packaging concerns. No host `CARGO_TARGET_DIR` is mounted or used.

```sh
scripts/new/native-container.sh                 # plan only; no Docker invocation
scripts/new/native-container.sh doctor          # read-only Docker/source ownership checks
scripts/new/native-container.sh prepare         # explicit compiler image build
scripts/new/native-container.sh prepare --fetch # additionally populate public registry cache
scripts/new/native-container.sh check --workspace --all-targets
scripts/new/native-container.sh test --workspace
scripts/new/native-container.sh test -p ai-platform-config-loader --lib
scripts/new/native-container.sh build -p ai-platform-tools --bin platform-tools
scripts/new/native-container.sh run --binary platform-tools -- --help
scripts/new/native-container.sh install --binary platform-tools --prefix "$HOME/.local"
```

Cold prerequisite: the primary must explicitly provision the selected official
`rust:1.99.0-bookworm` base into the existing daemon. `prepare` first checks that it exists
locally, then uses `docker build --pull=false`. It never automatically pulls a compiler
image. Its tiny build context contains only the development Dockerfile; no repository
source, `.env`, `.secrets`, registry credentials, or runtime configuration enters it.
Package installation occurs only during explicit prepare. The bootstrap compiles a small
std-only Rust launcher with the existing host Rust 1.99 compiler into an external temporary
file; it never creates a Cargo target directory. No host service/library compilation occurs.

Cargo check/test/build use `docker compose run --rm --no-deps --pull never`, not a Dockerfile
RUN layer: each requested execution actually runs. Cargo may reuse its normal incremental
fingerprints, but incremental compilation is disabled. All profiles have debug info disabled,
with one job and one CPU. Check/test/build are locked and offline; use explicit
`prepare --fetch` for public registry dependencies first. Tests default to network `none`
(which permits their own loopback), without runtime/DB/provider environment. A test needing
an external fixture fails rather than silently receiving administrative credentials.
The fetch override alone uses a bridge network, without service networks or secrets.

Public source subtrees are mounted read-only at `/source`; the repository root is never
mounted. The launcher rejects `.env*`, `.secrets`, source `target`, `node_modules`, `.git`,
and symlinks in those subtrees before Docker access, without reading their contents. Known
public fixtures, official SDK schemas, docs/design, root Makefile/harness and the dated
source-import receipt are included. Missing/new compile-time fixture paths must be added
explicitly to both the source policy and Compose mounts, never fixed with a root mount or
by dropping tests. Source-linked operational tools that need Git metadata, generated writes,
or private runtime configuration need their existing purpose-specific workflow; this
secret-free role does not impersonate those authorities.

Ownership is verified from existing project container labels and the cache volume owner.
A fixed ephemeral container name and the canonical platform-tools resource-lock fence
serialize heavy executions with Runtime/Worker and release work. An unexpected stale
lock/container fails closed; inspect it rather than automatically removing it. Nothing prunes/deletes Docker caches, volumes or business containers.

`run` executes only a previously built executable under `/native-cache/target/debug`
(or `--release`); plain binary names prevent path escape. It does not build implicitly.
`install` writes an external host **shell shim** back to `run`, never copies a Linux ELF
as a macOS executable. Existing unrelated commands are not overwritten. Application args
remain distinct arguments. Host repository-root path arguments are mapped to `/source`
(the same explicit public mounts remain the boundary). No env-file/network override exists for run in this first seam:
it is suitable for secret-free CLI commands; privileged runtime commands must keep their
existing role-scoped authority workflow until that explicit integration is provided.

Every Cargo/run exit code is preserved (signals map to failure), with external JSON evidence
under `$XDG_CACHE_HOME/agentforge-native-launcher` or `$HOME/.cache/agentforge-native-launcher`.
Evidence distinguishes actual execution from plan and never claims release/runtime acceptance.
Image preparation is separately reported by its real Docker build exit status.
Runtime/capability worker remain on their controlled Rust 1.95 source-image workflow.
Rust changes still require compilation, then the existing owner/paired-lock-aware hot-update
workflow to replace the corresponding service. The compiler role never restarts services itself; explicit watch restart delegates to the existing hot-update workflow.

Primary integration: `native-check` invokes `check --workspace --all-targets`, `native-test`
invokes `test --workspace`; package/target checks append their bounded Cargo args. Build CLI
bins once inside the same role, then `install` each desired shim. Do not retain fallback host
Cargo builds or copy Docker Linux artifacts into a host bin directory.

Measured primary verification is recorded in [the workflow receipt](../../../reports/platform-upgrade-20261017/docker-native-workflow.json): Docker compiler image preparation, source/ownership doctor, locked public fetch and actual configuration-loader tests (9 passed, 0 failed, 0 ignored) succeeded. Old host service intermediates were removed after that proof; the retained bootstrap tools stayed byte-identical. Whole-workspace, privileged integration and installed-shim execution remain separate work.

## Explicit cold-cache input and build-only watch

A real public Cargo **directory-source** can be mounted read-only without copying its
sources into Docker. Only an extracted package directory with `Cargo.toml` and
`.cargo-checksum.json` in every package is accepted; selecting `~/.cargo` (credentials,
index, config, etc.) is rejected. The option is launcher-level and goes before any `--`:

```sh
scripts/new/native-container.sh plan --public-registry-source /public/checksummed-crates
scripts/new/native-container.sh check --public-registry-source /public/checksummed-crates --workspace --all-targets
scripts/new/native-container.sh test --public-registry-source /public/checksummed-crates -p ai-platform-config-loader --lib
scripts/new/native-container.sh build --public-registry-source /public/checksummed-crates -p ai-platform-tools --bin platform-tools
```

The container receives `/af-public-registry:ro` and standard Cargo `--config`
`source.crates-io.replace-with="af-public-registry"` and
`source.af-public-registry.directory="/af-public-registry"`. `--locked --offline` and
network `none` remain mandatory. A missing crate or checksum fails; no implicit registry
fallback/download or host target cache is enabled. Application/test args after `--` are
not consumed as launcher configuration.

Observed on 2026-10-17: the current host registry/src directory contains 886 extracted
package directories without `.cargo-checksum.json`; merely having public Cargo sources is
therefore insufficient for this directory-source interface. The real plan correctly rejects
that directory. Do not invent checksum files or call this cold-cache validation a pass.
The supported explicit fallback is `prepare --fetch`: after checking the locally provisioned
Rust base, it builds the compiler image and runs `cargo fetch --locked` through the explicit
bridge override into the same single cache volume. This is an operator-requested public
registry network operation, never an automatic check/test fallback. Real Docker fetch and
cold compilation remain to be measured by the primary.

```sh
scripts/new/native-container.sh watch --build-only \
  --watch-path rust/platform-native/crates/platform-config-loader -- \
  -p ai-platform-config-loader --lib
# Optional --public-registry-source is accepted before the watch `--` delimiter.
```

Watch defaults to build-only, performs an initial container build, then polls the selected
explicit public source paths for content changes and runs that same build again. Build-only
never starts or restarts services. Each child preserves the regular ownership checks, lock,
single cache, network policy, exit/evidence recording and real Cargo execution; failures are
printed and watch waits for another source change.

Explicit restart mode delegates the entire build-and-replace operation to the existing
`scripts/new/hot-update.sh`, as its **only compilation/replacement entry**, wrapped once
by the canonical controller's `resource-lock run --resource integration-runtime`:

```sh
scripts/new/native-container.sh watch --restart --env .env.dev \
  --watch-path rust/platform-native/crates/platform-gateway -- --gateway
# --knowledge (or both --gateway --knowledge) are the only supported service scopes.
```

Only this repository's selected, Git-ignored `.env.dev` is accepted. The launcher passes
its path to hot-update without reading/exporting its contents, clears ambient environment
except Docker context/tool lookup, and pins the existing project and serial Compose limit.
Hot-update itself does not acquire the resource fence: watch wraps each sole hot-update
invocation with `integration-runtime`, sharing the canonical low-memory singleton with
`rust-build`. Existing hot-update loads the selected runtime configuration and performs its original
ownership checks, Runtime/Worker paired-image locks, image build, replacement and health
checks. No dev Cargo build occurs before or after that invocation, so an artifact is never
built once in the dev cache and then again just to restart. The runtime file is not mounted
in the compiler role or passed as build arguments; existing canonical Dockerfile source
allowlists remain authoritative. No new builder/infra/project/cache is created by watch.

Restart rejects Cargo target flags, frontend/all scopes, and public registry overrides:
those belong to separate entry points, and the production hot-update build is not the dev
Cargo directory-source build. A failed hot-update exit is printed as failure, never as a
successful restart. Real paired-lock/ownership/build/restart acceptance has not run here.


## Independent SDK engine

`engine-build [bounded Cargo target options]` and explicit online `engine-fetch` use only
`/source/rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.toml`.
The public engine source is an explicit read-only mount; no host Cargo invocation or second
target/registry volume is created. Engine fetch requires the already-prepared compiler image
and uses the same explicit fetch bridge; engine build is locked/offline with the regular
one-job/debug-zero settings. `--public-registry-source` is accepted when a real checksummed
directory-source is available. Engine artifacts share `/native-cache/target/debug` or
`release`, so the existing `run --binary NAME` and shim `install --binary NAME` work with
the actual engine executable name too. Run does not compile it implicitly.

`refresh-lock` is not exposed by this launcher: the primary owns the one-off lock-generation
and constrained candidate-export operation. No `cargo update` or writable source mount is
introduced as a shortcut.


## Canonical shared resource controller

Heavy `prepare`, `check`, `test`, `build`, `engine-build`, and `engine-fetch` commands require
a retained **native bootstrap** platform-tools executable. They delegate to that binary's
`resource-lock run --resource rust-build --expected-end-condition ... --` around the internal
launcher execution. This is the existing Git-common-dir resource protocol and low-memory
singleton used by Runtime/Worker and release work; no independent dev lock protocol remains.
Do not add an outer resource-lock around these commands, which would try to acquire twice.

Select the controller explicitly (a machine-local path is never hardcoded in this repo):

```sh
scripts/new/native-container.sh check --lock-controller /absolute/path/to/retained/platform-tools --workspace --all-targets
```

Or put exactly one absolute executable path, with an optional final newline, in the external
`$XDG_CACHE_HOME/agentforge-native-launcher/lock-controller.path` (falling back to
`$HOME/.cache/agentforge-native-launcher/lock-controller.path`). The explicit option takes
precedence. The config must be a small regular file; the target must be an executable native
ELF/Mach-O binary. A shell/Docker shim is rejected. No host Cargo build or PATH fallback is
attempted. The bootstrap binary can be retained after its old build/dependency directories
are cleaned; it is a controller, not a second daily build cache.

The internal child verifies its actual parent PID against both canonical `rust-build` and
`.low-memory` receipts, with matching checkout, schema and owner token. Ordinary environment
variables and a manually supplied internal flag cannot establish inherited ownership.
Read-only plan/doctor and host shim install need no acquired resource lock. Build-only watch
passes the resolved controller to every child build; restart watch instead acquires
`integration-runtime` around its only hot-update child, without a dev Cargo child or nested
rust-build lock.

Explicit fetch now derives the compiler host triple using `rustc -vV` **inside the container**
and adds `cargo fetch --target <that-host>`, printing the resolved target. It does not fetch
all Windows/macOS targets. This changes neither Cargo.lock nor the scope of Linux checks/tests;
normal test/build remain offline/network-none. The receipt's process args describe the base
Cargo invocation; the fetch wrapper's printed compiler target records the resolved suffix.

Source verification after the final fence integration: 18 standalone Rust tests passed. Real contention with the running Runtime/Worker export was rejected before any second compiler container started. The final default-controller success path, host-target fetch and service restart still require primary execution; earlier Docker tests do not prove these later branches.
