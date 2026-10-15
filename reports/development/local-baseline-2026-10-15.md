# Local development baseline — 2026-10-15

The isolated developer foundation is installed and infrastructure starts. The
required aggregate is **FAIL**, with retained source/gate debt and missing
historical Git provenance. This is a reproducible development baseline, not
integrated application or release acceptance. Continue a Goal against the
recorded failures; do not interpret environment preparation as product completion.

## Source and changes

The project root is `AI--Platfform/`. The parent recovery package and its
unintegrated proposals remain outside Git and unchanged. Before mutation,
`python3 VERIFY_RECOVERY.py` passed all **5,591** original manifest files.
Neither Rust candidate was applied. No historical commit was substituted for
the recovered current source.

Changes are limited to local bootstrap/command wrappers, explicit service and
cache configuration, documentation, ignore rules, a Native toolchain pin, and
these verified portability/test-fixture repairs:

- Two Web helper modules shared case-insensitive basenames with TSX components.
  Distinct `*State.ts` names and their existing imports fix actual macOS type
  and production-build failures without changing UI behavior.
- The composed-source receipt test now resolves the macOS temporary-directory
  symlink, preserving the production validator's exact working-directory check.
- Knowledge startup negative tests now inject failure into the actual required
  router registry's importer. They retain the original fail-closed assertion.
- Two token-shaped cache-test strings were replaced with explicit synthetic
  credentials. No provider credential was used to validate them.

All five checked dependency locks are byte-identical to recovery: root `uv.lock`,
Worker `uv.lock`, Web pnpm lock, Native Cargo lock and Runtime overlay Cargo lock.
No gate threshold, allowlist or skip policy was changed to obtain green.

## Installed environment

| Item | Verified value |
| --- | --- |
| Host | macOS arm64, 36 GiB RAM |
| Python / uv | CPython 3.12.12 / uv 0.13.0 |
| Node / pnpm | 26.11.1 / 12.11.2, isolated under `.tools/` |
| Rust Native | 1.99.0, minimal with rustfmt/clippy |
| Rust Runtime | 1.95.0, minimal with rustfmt/clippy; global default unchanged |
| System dependencies | Existing Apple clang/Swift, OpenSSL, Poppler/libpq; installed pkgconf 3.0.8 and CMake 4.4.4 |
| Build policy | One Cargo job, debug/incremental disabled, external targets |
| Native target footprint | Approximately 2.1 GiB after check/test/build |
| Remaining source-volume disk | Approximately 284 GiB after preparation |
| Docker | Desktop running, approximately 7.7 GiB available to its VM |

Root workspace and separate Worker dependencies installed from frozen locks.
The exact tokenizer asset is hash-verified in the external cache. The Runtime
upstream checkout is clean at `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`, with
tree `572cdecd45cd985f0ad80b63d8b86ffcb345f47a`. It lives outside this repository.
Runtime/Worker Linux product images were not built or started.

The isolated Compose project is derived from the checkout path, with loopback
ports 15432 (PG), 16379 (Redis), 16333/16334 (Qdrant) and 19000/19001 (S3).
All four image digests are recorded in `deploy/local/compose.yml`. The services
used approximately 432 MiB in the observed sample, within explicit individual
memory limits. Existing other-project containers and volumes were untouched.
Configuration is generated in ignored mode-0600 `.env` / `.env.dev` files.

## Actual validation

Commands below use `bash scripts/dev/with-local-toolchain.sh` where applicable.
Detailed current logs and the aggregate JSON/TSV are in ignored
`tmp/local-baseline/`; the pre-commit aggregate is preserved under
`tmp/local-baseline-before-first-commit/`. Supporting attempts are retained under `tmp/`.

| Check | Actual result |
| --- | --- |
| Configuration | `make validate-config validate-example-config`: PASS, provider-setup warnings retained |
| Harness and import architecture | PASS; 634 Python files, zero import violations; five existing program warnings |
| Retired dependencies / Gateway-KB boundary / CI enforcement self-test | PASS |
| Python lint | Existing CI script scope plus new setup helpers and repaired Knowledge fixture: PASS; not a repository-wide Ruff/MyPy claim |
| Web types, lint, i18n, production build/bundle budget | PASS after the two basename fixes |
| Web Node tests | **2,034 passed**, zero skips |
| Knowledge gate after importer-fixture repair | **2,464 passed, 3 skipped**; skips require the optional PG backfill DSN and default-port Qdrant configuration |
| Explicit Qdrant integration on isolated port 16333 | **2 passed**, zero skips |
| Official Worker result boundary | **5 passed** with its installed frozen official SDKs; SDK runner/provider loops remain untested |
| Local Node | **73 passed** on the final run; original cold Swift compiler timeout retained, unchanged 30-second limit |
| Setup repair regressions | **37 passed** across cache credentials, Knowledge startup and composed-source receipt tests |
| Python package build | All five workspace packages built as wheels and source archives |
| Native locked all-targets check | PASS, existing compiler warnings retained |
| Native Gateway library tests | **92 passed**, zero ignored |
| Native Gateway and Knowledge executable build | PASS, external targets, 26 existing library warnings |
| Native rustfmt | FAIL: existing formatting differences; no broad source reformat applied |
| Infrastructure | Authenticated PG SELECT, Redis set/get/delete, Qdrant readiness, S3 signed put/get/delete: PASS |
| Service lifecycle | Scoped stop then start and health/CRUD checks: PASS; volumes preserved |
| Database roles | Seven distinct role connections verified |
| Canonical schema initialization | FAIL before schema adoption: frozen baseline's source Git commit is absent |
| Real PG migration gate | **141 passed, 9 failed**; see `tmp/local-kb-migration.log` for retained failures |
| Stock `make doctor` | FAIL: four fixed-name containers belong to another checkout; isolated project's ownership/health checks pass |
| Core boundary provenance | FAIL: retained inventory references absent original Git object |
| Test hygiene | FAIL: two unbaselined skips; 77 no-assertion advisories retained |
| Gateway unit aggregate before initial Git commit | **3,816 passed, 143 failed, 8 skipped, 3 deselected**; failures include legacy authority fixtures/source assertions, a Linux `/proc` assumption and two missing-HEAD cases |

Initial archive runs of `single-instance-guard` and the LOC generator fixture
failed because no HEAD existed. The initial source commit makes these checks
runnable; their post-commit result belongs to the final rerun, not the earlier
source-archive receipt. This does not restore the historical Git objects below.

Browser inspection used the local Vite server at `http://127.0.0.1:4181`.
The login page rendered with no captured console warnings/errors. `/agents`
correctly returned to login. The authenticated Agent/Knowledge interaction flows
remain unverified because the canonical schema/application stack is unavailable.
The module-name repair has type/unit/build evidence, not an authenticated UI
acceptance claim. No performance trace was needed for import-name-only changes.

## Remaining prerequisites

1. Restore verifiable original history/ancestry for frozen DB source
   `386344c6c15e92547cabe32b07fad304afbf4a98`. A new root commit cannot satisfy
   the authority's source-object/ancestor contract. No frozen manifest was edited.
2. Restore the core inventory's original commit
   `1d25e67937dd387a572e8006607d859d0991f977` and tree
   `c40d617402dbe9259d5ca0d56ce97528de46c731` before that provenance gate can pass.
3. Address retained Gateway, migration, hygiene and Native formatting failures
   within their owning Goal scopes. The aggregate preserves nonzero status.
4. Build the current composed Runtime/Worker Docker images and configure a
   dedicated test provider/account before authenticated application journeys.
   Provider credentials, production data and production credentials were unused.
5. Native Local Node device pairing and real Office/host permissions remain the
   final handoff's open work; no macOS security permissions were granted.

## Git and network scope

Git is initialized only at the project root on `main`, with origin
`https://github.com/misaya-yang/AgentForge.git`. Source, examples, dependency locks,
migrations, fixtures, required docs, product PNGs and Dart SDK sources are eligible
for tracking. Local installs, config/secrets, user data, caches, logs, browser
state and build outputs are ignored. Twenty-five representative ignore/keep
checks passed. The final staged scan must be clean before the authorized push.
Initial-add whitespace diagnostics are inherited source debt; immutable SQL
migrations were preserved rather than reformatted.

No global proxy, DNS, routing, StarVPN or TUN setting was changed. The measured
process-only npm direct route was faster; Azure direct was slower. Setup retains
existing network behavior by default. Runtime Git download succeeded on retry.

Reproduction and lifecycle commands are in
[`docs/harness/local-development.md`](../../docs/harness/local-development.md).
