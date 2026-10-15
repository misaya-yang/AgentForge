# Reproducible local development

The project root is the directory containing `harness.yml`, `Makefile`, `src/`
and `web/` (`AI--Platfform/` in checkpoint 125). The parent recovery directory
is not part of this Git repository. `RESTORE.md`, `RECOVERY_MANIFEST.json`,
`VERIFY_RECOVERY.py` and `recovery-candidate/` remain recovery inputs. Before any
edits or dependency installation, the original verifier passed for all 5,591
manifest files on 2026-10-15. Its exact-file-set contract applies to the original
extraction; a development checkout with `.git`, dependencies and edits is no
longer an unmodified recovery extraction. Do not rewrite its manifest to hide that.

The final handoff is [FINAL_CODEX_HANDOFF_20261015.md](../handoff/FINAL_CODEX_HANDOFF_20261015.md).
The two isolated Rust proposals remain unintegrated. Existing source, migrations,
fixtures, locks, architecture evidence and the hybrid Python services are retained.

## Entry points

From the project root, with Bash, Python 3, existing `uv`/`rustup`, curl, tar and make:

```sh
make local-bootstrap
bash scripts/new/init-env.sh --env .env --if-missing
make local-baseline
```

On macOS, Xcode Command Line Tools provide clang/Swift. The bootstrap reuses
existing system dependencies and installs missing pkgconf/CMake through an
existing Homebrew. This machine also has OpenSSL, Poppler and libpq. System
package installation on a host without a package manager or compiler may require
its administrator; the script does not change security permissions.

Bootstrap installs uv 0.13.0, Node 26.11.1 and pnpm 12.11.2 under ignored
`.tools/`, and selects CPython 3.12.12 from `.python-version`. Node downloads
are checked against the official SHA-256 manifest. The initial uv installs the
pinned uv tool; subsequent dependency work uses that isolated version. Python
distributions/caches use uv's normal user cache. Web dependencies use pnpm only.
Both Python environments and Web use their existing frozen locks. Rust 1.99.0
(Native) and 1.95.0 (Runtime) are installed with the minimal profile plus rustfmt
and clippy, without changing the global default toolchain. No service is started
and no image is built. Network access to PyPI, Python standalone builds,
nodejs.org and the npm registry is required for a cold install.

To run any canonical command with these tools, from any working directory:

```sh
bash /path/to/checkout/scripts/dev/with-local-toolchain.sh make harness-check
```

The wrapper resolves paths from its own location and exports `UV_FROZEN=1`, one
Cargo job, no debug/incremental, and the external native target directory
`${XDG_CACHE_HOME:-$HOME/.cache}/agentforge/targets/native-1.99.0`.
`AGENTFORGE_CACHE_DIR` can select another external cache root. The verified
offline tokenizer cache is adjacent under `tokenizers/`; its asset/hash comes
from the locked tiktoken wheel, and partial downloads resume with curl.
The baseline driver calls the existing gates separately, records their actual
exit codes and continues after failures. Logs and JSON/TSV results are under
ignored `tmp/local-baseline/`. A failed command makes the aggregate exit nonzero;
a successful command's pytest skips must still be read from its log. The doctor
is a runtime readiness check and can fail while offline development works.
The Python lint scope matches the existing CI script checks plus the changed
harness validator, new Python setup helpers and the repaired startup-import
fixture; this is not repository-wide lint or MyPy coverage.
The Web checks cover both TypeScript projects, lint, Node tests, i18n and the
production build including its existing bundle budget.

The baseline also runs Native rustfmt, locked all-targets check, the Gateway
library tests, and both Gateway/Knowledge executable builds, using 1.99.0.
These host checks were explicitly authorized for this local setup. Runtime's
1.95.0 product build remains its existing composed-source Docker workflow.

## Isolated development services

```sh
make local-services-up
make local-services-check
make local-services-init
make local-services-status
make local-services-stop
```

`up` generates ignored `.env.dev` with fresh local-only secrets and mode 0600,
then starts PostgreSQL, Redis, Qdrant and RustFS S3, serially. The Compose project
name includes a hash of the checkout path; there are no fixed container names.
Volumes are project-specific and stop preserves them. Before actions, container
labels must identify this exact checkout. Other projects' containers/volumes are
untouched. Images are bound to the digests actually pulled for this baseline.

| Service | Host listener |
| --- | --- |
| PostgreSQL | `127.0.0.1:15432`, database `agentforge_dev` |
| Redis | `127.0.0.1:16379` |
| Qdrant HTTP / gRPC | `127.0.0.1:16333` / `16334` |
| S3 / object console | `127.0.0.1:19000` / `19001` |

`check` executes authenticated PG SELECT, Redis set/get/delete, Qdrant readiness
and signed S3 put/get/delete of unique synthetic data. It does not claim an
application schema or product acceptance. `init` uses the existing authority to
provision roles, verify seven distinct connections, migrate and verify. Migration
currently fails on the missing frozen-baseline Git provenance below, after role
verification succeeds. Application services must wait for that failure to close.
The S3 fixture uses [RustFS's official container](https://docs.rustfs.com/en/installation/container)
with explicit local credentials; MinIO's old official image could not be pulled.

Restore Runtime source independently with `make local-runtime-source`. It writes
only under the external cache, verifies the exact upstream commit/tree and a
clean working tree, and never applies recovery candidates. The verified result
is `.../upstream/codex-092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`.
Supply that path as `AI_PLATFORM_AGENT_RUNTIME_SOURCE` to the existing builders.

## Network behavior

No system proxy, StarVPN/TUN, DNS, routing or shell proxy settings were changed.
Downloads retain the current environment by default. A process-only test with
`curl --interface en0`, explicit HTTPS DNS resolution and `--resolve` bypassed the
tunnel without changing other applications. The npm sample was about 301 KB/s
direct versus 58 KB/s through the current proxy; Azure direct was slower.
These samples do not justify switching every download to direct routing. Never
toggle the user's VPN or clear global proxy variables as a setup shortcut.

## External prerequisites and retained blockers

| Check | Required inputs and passing evidence |
| --- | --- |
| `make doctor` | Running Docker/Compose v2; sufficient memory/disk; free ports; container labels owned by this checkout. |
| `make validate-config` | An ignored mode-0600 `.env` generated by the existing initializer, or `ENV_FILE=/secure/path`; valid Compose rendering. No provider call is made. |
| `make core-boundary-gate` | Original Git object `1d25e67937dd387a572e8006607d859d0991f977` and tree `c40d617402dbe9259d5ca0d56ce97528de46c731`, as declared by the retained inventory. A fresh Git init does not recreate them; do not replace this provenance to obtain green. |
| Rust release/change gate | Existing hosted CI composed-source identity and changed-crate gate. Native local checks are separate sub-evidence, with the user-authorized local profile described above. |
| Runtime recovery/build gates | Running Docker plus a clean immutable upstream checkout at `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`, supplied as `AI_PLATFORM_AGENT_RUNTIME_SOURCE`; existing overlay/lock verification and serialized builders. |
| `make kb-migration-gate` | Dedicated PostgreSQL with temporary-database privileges, matching role/schema policy and `pg_dump`/`pg_restore`. No shared database migrations are part of baseline setup. |
| Live Gateway/Knowledge/Runtime/Worker, RAG and browser journeys | Owned running stack, compatible migrated schema, Redis/Qdrant/object storage, dedicated authenticated test accounts and explicitly configured test provider credentials/dataset. Passing offline fixtures or health checks do not satisfy these gates. |
| Full SDK SSE | Maven and Dart in addition to Python/Node; `SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract` must pass without optional-tool skips. |

The original handoff's authentication revocation, uncompiled Rust proposals,
native Local Node pairing/parity and integrated acceptance queue remains future
Goal work. This setup does not implement that queue or certify enterprise release.

The [baseline receipt](../../reports/development/local-baseline-2026-10-15.md)
records the executed commands, failures and evidence boundaries.

Local secrets, user data, caches, logs, environment installs and build/browser
outputs are ignored. Product images and the Dart SDK's `lib/` are tracked source;
SQL migrations, sanitized examples, fixtures, lockfiles and required documents
remain eligible for tracking. Recovery candidates outside this root are excluded
by the repository boundary, not deleted.
