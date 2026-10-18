# Command Catalog

> The canonical way to do every routine thing in this repo. Agents run these commands rather than
> inventing equivalents, so results are comparable across machines and sessions.
> `harness.yml` holds the machine-readable subset; `make harness-check` verifies they still exist.

This is a lookup reference. Use only commands needed by the current task; the [task harness](README.md)
defines verification selection and reuse. Do not install tools or start services merely to work
through this catalog.

**Schema:** `harness/commands/v2`

---

## 0. Toolchain

| Tool | Required version / policy | Notes |
| --- | --- | --- |
| Rust (native services/tools) | 1.99.0 | Root and native workspace toolchain pins; Cargo.lock is authoritative |
| Rust (upstream Runtime overlay) | 1.95.0 | Independent pinned composed-source toolchain |
| Node | 26.11.1 | Current stable release; build/CI toolchain |
| pnpm | 12.11.2 | `npm install --global pnpm@12.11.2 --allow-scripts=pnpm` |
| Docker | Compose v2 | `docker compose`, not `docker-compose`, wherever possible |

Use **Cargo for native services/tools and pnpm for `web/`**.
Node 26 does not bundle Corepack. The global npm command above only bootstraps the
pinned pnpm executable; use pnpm for web dependencies.
`web/pnpm-workspace.yaml` preserves security overrides and rejects a mismatched pnpm executable. Use
`pnpm -C web install --frozen-lockfile`. pnpm 12 adds a separate package-manager
lock document; the existing application dependency document remains unchanged.
New toolchain releases
should replace these tested pins together after the same gates pass.

Node 26.11.1 is the latest Current release verified on 2026-10-12; Node 24.21.0 is
the latest LTS. Node is only the frontend builder/CLI test host here, and the
frontend runtime remains Nginx. The SDK CLI keeps its existing Node 22-compatible
output/API baseline while CI runs on the current toolchain.

Official releases: [Node.js](https://nodejs.org/dist/index.json),
[pnpm 12.11.2](https://github.com/pnpm/pnpm/releases/tag/v12.11.2).

## 1. Machine and stack

| Purpose | Command |
| --- | --- |
| Preflight the machine before anything else | `make doctor` |
| Check living-document scripts, links and implemented CLI names (also included in harness; dates do not expire) | `platform-tools docs-truth-check --repo-root .` |
| Verify the pinned Agent Runtime source release unit | `make agent-runtime-source-contract` |
| Verify the Runtime, capability Worker and Python Sandbox image release unit | `platform-tools source-lock verify-local-images --repo-root . --lock deploy/agent-runtime-source/lock.json --runtime-image <runtime> --worker-image <worker> --sandbox-image <sandbox>` |
| Rebase the Codex kernel source and host seams | Follow [Codex upstream upgrades](codex-upstream-upgrade.md) using the existing native `source-candidate` / `source-lock` tools |
| Verify the independent local-Runtime CLI | `make independent-cli-gate` |
| Build the independent native CLI inside Docker | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-cli-build-local` |
| Verify the pinned Rust Agent Runtime image | `make agent-runtime-contract` |
| Legacy Agent PostgreSQL ThreadStore (Docker live migration pending; do not execute locally yet) | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/path/to/fork make agent-thread-store-contract` |
| Serial single-kernel release contract | `make agent-runtime-release-gate` |
| Digest-pinned current→frozen→current rollback rehearsal | `make agent-runtime-rollback-rehearsal` |
| Build the independent Python sandbox under the existing Rust build lock | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-python-sandbox-build-local` |
| Build Runtime, control Worker, then sandbox serially (no deployment) | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork bash scripts/rust/build-update.sh --artifact all` |
| Build the pinned App Server image inside Docker and probe initialize | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-source-build-local` |
| Build the pinned Rust Agent Runtime image inside Docker | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-build-local` |
| Smoke-test the Rust Runtime in an isolated Docker network | `AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-smoke` |
| Run real Qwen Responses simple/long/resumed text gate | `ENV_FILE=/path/to/.env AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-text-gate` |
| Verify CHR-03 tenant/revision-bound read-only capability bridge | `make agent-runtime-readonly-gate` |
| Run Agent tool side-effect and interruption contract gate | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-write-gate` |
| First run: generate `.env`, pull images, start, migrate, validate | `make quickstart` |
| Maintainer first run from current source | `make quickstart-build` |
| Validate `.env` + Compose without starting | `make validate-config` |
| Validate config **and** running runtime | `make validate` |
| Validate the committed public example only | `make validate-example-config` |
| Health table (nonzero exit on failure — usable as a release gate) | `make status` |
| Live logs | `make logs` |
| Stop / restart without deleting data | `make stop` / `make restart` |
| Build native images and replace selected services | `make hot-update` |
| Preview / load local demo data | `make seed-demo` / `make seed-demo-apply` |

`*-build-local` means the Docker daemon is local; Cargo runs only in the multi-stage Docker builder.
`make validate-example-config` also runs `platform-tools compose-image-matrix --repo-root .`:
actual Compose rendering for fallback/overridden/source-build images across full/compact/scale,
plus collision and stale-pair negative cases. It uses public examples, starts no services and
does not admit image artifacts. The deploy/hot-update source-lock image checks remain required.
`make agent-runtime-recovery-tests` runs Runtime/Worker library tests and the
scoped Core original-turn recovery/suspension regressions inside that builder,
using `AI_PLATFORM_AGENT_RUNTIME_SOURCE` for the clean pinned upstream. It is
local Docker evidence, separate from the hosted CI gate.
For these local Runtime acceptance targets, retain the Docker-builder convention;
do not substitute host Cargo runs for their Docker evidence. Native services/tools now use the shared Rust 1.99.0 Docker development entry:
`make native-check`, `make native-test`, or `scripts/new/native-container.sh`
for affected package/target commands. One Docker-managed cache holds the native
registry and target; do not also build the server graph in a host Cargo target.
Builds remain serialized with debug and incremental output disabled. Host Cargo
is only for explicitly selected macOS clients/OS tests. A small std-only launcher
bootstrap does not build the server dependency graph. These commands do not
start business services or establish local runtime acceptance. `make rust-changed-crate-gate` remains a hosted-CI entrypoint,
not a replacement local acceptance command.


**Legacy live gates pending a separate Docker run phase:** `kb-migration-gate`,
`kb-integration-smoke`, `test-isolation` (including the selected ignored Gateway boot), and
`agent-thread-store-contract` retain their original explicit entrypoints and complete test
scopes. They have **not** been migrated to the daily secret-free/network-none compiler role
and were **not executed** in this checkpoint. Their retained legacy recipes are not host
build authorization: do not run them on the host under the current Docker-only server policy.
Runtime-only scoped secrets, fixture files, owned networking and required PG client tools
must be connected in a separate container run phase before these live gates resume. Do not
substitute the daily `native-container test` role, omit ignored tests, or count an unrun gate
as acceptance. This limitation does not change the tested daily Docker check/test/build path.

`platform-tools ci-capacity` records detected CPU/memory and the selected build/test limits.
The shared `setup-native` action defaults to one build job. Only its `capacity-profile: hosted`
opt-in on standard GitHub-hosted Linux runners allows up to four jobs, bounded by detected CPUs
and a 3 GiB/job budget after reserving 2 GiB. Self-hosted/local execution remains one job.
The action pins sccache 0.18.0 and the changed-crate gate pins nextest 0.9.148. That gate uses
nextest for the same selected packages with default filters ignored, then Cargo for enabled
library doctests. Only nextest's
explicit no-tests exit code falls back to the original complete Cargo test command; other
failures remain failures. Assembly, metadata and test phases share a 55-minute gate budget,
with progress receipts retained on failure; the enclosing CI job has its own 65-minute limit.
Source composition and Cargo targets use external scratch directories. PR latency acceptance
requires successful real PR runs; local policy tests and historical failed push runs do not count.

Every target above accepts `ENV_FILE=/path/to/.env` when the real env file lives outside the repo.

## 2. Deployment

| Purpose | Command |
| --- | --- |
| Deploy everything (no forced rebuild) | `make deploy` |
| Deploy and rebuild images | `make deploy-build` |
| Deploy using China mirrors | `make deploy-cn` |
| Infrastructure only (postgres/redis/qdrant) | `make deploy-infra` |
| Application services only | `make deploy-app` |
| Application services, skip migrations | `make deploy-app ARGS="--no-migrate"` |

`deploy-infra` and `deploy-app` are mutually exclusive; run them as separate commands.

## 3. Database

| Purpose | Command |
| --- | --- |
| Run pending migrations | `make migrate` |
| Initialize schema on a fresh database | `make migrate-init` |
| Show applied vs pending | `make migrate-status` |
| Back up / restore / list backups | `make backup` / `make restore` / `make backup-list` (external `AI_PLATFORM_BACKUP_DIR`, otherwise the XDG/HOME state directory; repository paths are rejected) |

`database-authority migrate` is the only schema writer; `make migrate` and
`scripts/new/migrate.sh` delegate to that native command. The former Python
entrypoints are archived migration references, not runnable setup instructions.
The authority recognizes historical filename, numeric, and per-service ledgers
only as adoption input, then freezes them after the baseline marker. Ambiguous
duplicate numeric revisions fail closed with a reconciliation receipt.

The authority acquires one PostgreSQL session advisory lock before bootstrap,
discovery, or DDL. A concurrent runner waits, then reloads the authoritative
ledger and exits as a no-op; connection loss releases the lock.

Native Gateway startup performs read-only database admission under the configured
application principal before serving requests. Its supported epoch and role
contracts come from the native authority; an incompatible schema or failed
required bootstrap must not become ready on the strength of `SELECT 1`.
Gateway never migrates or retries under a different database identity. The old
Python epoch 2–10 startup contract is archived history, not a retained deployment
option.

`make migrate-init` maps to the authority's `init-fresh` command and therefore
requires a truly empty database plus a complete frozen baseline. It never
falls back to directly piping `database/schema.sql`.

Restoring over shared data requires a current backup and explicit approval.

## 4. Development

| Purpose | Command |
| --- | --- |
| One-shot local dev environment | `make dev-setup` |
| Prepare private local inputs for the same development project | `scripts/new/init-env.sh --env .env.dev --if-missing --local-dev` |
| Start / stop / reset / inspect dev containers | `make dev-start` / `make dev-stop` / `make dev-reset` / `make dev-status` |
| Build and run native backend services from the current source | `make dev-compose` |
| Follow native development-service logs | `make dev-compose-logs` |

The explicit local profile delegates private S3/scoped-role configuration to
`platform-tools local-dev-env`. It preserves existing credentials and refuses tracked,
non-private, conflicting or other-checkout env files. The file must be in the selected
repository root and use the Compose-published numeric `127.0.0.1` database host.
It prepares inputs without starting Docker or writing
the database. The opt-in `AI_PLATFORM_LOCAL_DEV_CREDENTIALS=true` tells the normal native
migration wrapper to run `database-authority initialize-local-credentials --expected-database NAME`
after role provisioning. The marker is read only from the selected file; ambient values
cannot enable or disable it. The command verifies this binary's source checkout, exact
Compose ownership/configuration and published port before connecting to the database. It initializes
only missing SCRAM passwords, checks existing credentials without rotating them, and verifies
the independent role connections. Frozen SQL, fingerprints and application privileges remain
owned by the existing authority.

After local setup, run the explicit live no-rotation and role-denial regression under the
integration-runtime resource lock with `ENV_FILE` pointing to the selected `.env.dev`:
run `cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-migration-authority --test local_credential_bootstrap -- --ignored` **inside the explicit role-scoped integration container**. The daily secret-free compiler role does not receive this env file or a database network; the privileged container seam remains pending in the Docker-only workflow, so this live gate is not currently rerun by `native-container.sh test`.
It requires all six credentials to exist, compares role catalog values privately in memory,
rejects a mismatched password and source override, validates metadata-only admission, and
checks denied direct ledger/password access and owner escalation. It never resets data.

## 5. Native tests and formatting

Install source-built services and operational commands with
`make native-install` (Docker artifacts; host clients use their explicit platform workflow)
in the approved build environment. Keep Cargo output outside the checkout.
Scope tests to the package and target you changed:

```bash
scripts/new/native-container.sh test -p ai-platform-tools --test deployment_script_contracts
cargo fmt --manifest-path rust/platform-native/Cargo.toml -p ai-platform-tools -- --check
```

Use the dedicated Make gate for an actual integration target. Ignored PostgreSQL,
Qdrant, provider and platform acceptance tests require their explicit disposable
fixture/authorization settings; an ordinary test run does not execute or certify
them. Historical pytest/Ruff receipts remain archived evidence and are not
commands for the native project.

## 6. Frontend

```bash
pnpm -C web type-check
pnpm -C web test
pnpm -C web lint
pnpm -C web build
pnpm -C web i18n:check
```

`type-check` directly checks both `tsconfig.app.json` and `tsconfig.node.json`;
the old zero-file solution-root defect has been fixed. The Node suite runs in CI.

E2E — **every Playwright spec lives in `web/e2e/`**; see
[`web/e2e/README.md`](../../web/e2e/README.md) for the layout, the five configs, and where run
artifacts go. `make harness-check` fails when a spec, a config, or a screenshot lands elsewhere.

- `pnpm -C web e2e:opensource` — route smoke used in CI, no live provider needed.
- `pnpm -C web e2e` — targets the current singleton local stack via
  `scripts/dev/start_e2e_stack.sh`; the script does not yet prove Compose owner or source SHA, so run
  the integration preflight and do not call it a trusted release gate until ARC-00 fixes that gap.
- `pnpm -C web e2e:headed` — same, with a visible browser.

SDK SSE contract:

- `make sdk-sse-contract` — runs the shared Rust client and CLI SSE fixture contract; also runs Java
  and Dart native tests when Maven or Dart is installed, otherwise prints an explicit skip.

### Native ignored-test ledger

`make hygiene-check` selects the individually reviewed native ledger at
`docs/architecture/baselines/2026-10-native/skip-baseline.json`. The historical
Python-era ledger remains intact. Named PostgreSQL/Qdrant/model-artifact and
conditional browser-receipt prerequisites are omissions from ordinary offline runs,
not successful acceptance. Required live release gates must execute their contracts
and reject skipped or zero-test runs.

## 7. Gates

This is a command catalog, not a per-edit checklist. For daily development, use the
[task harness](README.md) to choose the existing checks that prove the requested behavior.
Run them once per integrated batch with one owner. Reuse applicable results; do not combine
an alias, its underlying target and a larger suite covering the same assertion without a
specific additional need. Module handoff is not a reason to repeat tests.

For a complete CI candidate, `make affected-gates BASE_SHA=<verified-base>` selects the declared
CI/release coverage and `ci-gate-enforcement` checks the required job outcomes. This selection
does not schedule local agent actions and is not recomputed after every edit. Do not fabricate
Git metadata for archives. Record actual passed, failed, skipped and unrun work; selection is
not execution and source/fixture checks are not live acceptance.

| You changed | Gate |
| --- | --- |
| Harness files, docs contract and recurring platform findings | `make harness-check` |
| Recurring source/deployment fitness, without runtime acceptance | `make platform-fitness-gate` |
| Offline CI enforcement and recurring-finding rejection cases | `make ci-gate-enforcement-selftest` |
| Native Helm public-fixture lint/render and security contracts | `make helm-native-gate` (Helm required; local `HELM_EVIDENCE_DIR` may point outside the repository; no cluster operations) |
| Split Valkey source TLS/ACL/durability and single-project deployment policy | `make valkey-deployment-gate` (offline only); `platform-tools valkey-image-check <digest-reference> <uid> <gid>` checks input shape; actual registry digest and UID/GID verification remain separate |
| Agent Runtime source source/schema/SBOM/image lock | `make agent-runtime-source-contract` |
| Agent Rust Runtime image identity | `make agent-runtime-contract` |
| Agent Rust Runtime Docker lifecycle | `AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-smoke` |
| Agent Runtime real Qwen text path | `ENV_FILE=/path/to/.env AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-text-gate` |
| Agent Runtime read-only capability bridge | `make agent-runtime-readonly-gate` |
| Agent Runtime tool side-effect safety | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-write-gate` |
| Rust cutover offline/sub-evidence bundle (not a platform release gate) | `make agent-runtime-release-gate` |
| Digest-pinned current→frozen→current rollback | `make agent-runtime-rollback-rehearsal` |
| Legacy Agent ThreadStore fresh/idempotent/live contract (Docker live migration pending; do not execute locally yet) | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/path/to/fork make agent-thread-store-contract` |
| Agent runtime compatibility surface | `make verify-assistant-runtime-dev` |
| Eval / trace pipeline | `make eval-e1-gate`, then `make verify-eval-dev` for the wider branch gate |
| Agent core / subagents | `make agent-eval-core-gate` |
| Agent Studio | `make verify-agent-studio` |
| Service boundaries, Agent API surface | `make architecture-boundary-gate` and `make core-boundary-gate` for their declared import/allowlist scopes; `make test-isolation` is legacy/live with Docker run wiring pending (do not execute locally yet); API/runtime contract gates remain separate |
| Native source-owned OpenAPI | `make verify-openapi-contract` (alias of the full native gate; missing independent candidate fails) |
| Native full published OpenAPI | `make verify-native-openapi-contract NATIVE_OPENAPI_CANDIDATE=/path/to/independent-native-export.json` (missing candidate fails) |
| RAG retrieval | `make rag-eval-fixture-contract` is fixture-only evidence; implementation/live quality require separate ARC-00 gates |
| Knowledge service (native production-module tests) | `make kb-unit-gate` |
| Legacy native full-chain/ledger/roles, 97/112, lock/crash and 101 dump/restore (Docker live migration pending; do not execute locally yet) | `KB_MIGRATION_TEST_ALLOW_CREATE_DATABASE=1 KB_MIGRATION_TEST_ADMIN_DSN=<disposable-admin-dsn> make kb-migration-gate` |
| KB development fixture structure / manifest hashes / seed drift (`tests/fixtures/eval/rag/golden/**`; not a release claim) | `make kb-golden-gate` |
| KB T0 release evidence (200–400 reviewed cases, source mix, manifest/pointer/reviewed real-corpus baseline binding) | `make kb-release-evidence-gate` |
| Project a golden JSONL into the versioned Postgres store (PRD T0-#2; `--dry-run` first) | `platform-knowledge-ops import-kb-eval-golden <jsonl> --dry-run` |
| Legacy Qdrant integration smoke (Docker live migration pending; do not execute locally yet) | `make kb-integration-smoke` |
| Real-corpus retrieval baseline candidate (live KS; records distribution only, does not approve it) | `make kb-baseline-record KB_BASELINE_DATASET_ID=<id>` |
| RAG eval fixtures / native `eval_rag` | `make rag-eval-fixture-contract` |
| Deploy scripts, `.env.example`, Compose | `make validate-example-config` |
| Shared/Java/Dart SDK streaming change | `SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract` (plain command is optional-tool sub-evidence) |
| Independent CLI config, launcher, provider adapter, package | `make independent-cli-gate` |
| Independent native CLI composed-source artifact | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-cli-build-local` |

The consolidated KB migration target does not certify every historical embedding
or artifact assertion. Those specialized tests remain unverified obligations;
the native gate prints its exact coverage before execution.

The existing platform-fitness validators also run inside harness-check. Their
production rejection fixtures run inside ci-gate-enforcement --selftest and the
existing platform_fitness unit-test batch; no separate policy registry is used.
They cover the recurring checks in
[platform engineering §4.5](../plans/platform-upgrade-2026-10/08-platform-engineering.md#45-harness-docs-and-architecture-fitness-u-pe-02-u-pe-04-u-pe-40),
including Compose overlays and composite-action references. Source checks use
public configuration only and never resolve a private env. A rejected current
configuration remains a failing gate even when its negative self-tests pass.
Rendered image/source pairing, actual role identity, TLS, live deployment and
release admission retain their separate existing gates.

## 8. What CI runs

`.github/workflows/ci.yml` on push/PR to `main` and `dev` currently runs:

1. **Script Contracts** — shell syntax + focused script tests.
2. **Compose and Harness** — `make validate-example-config`, Compose render, harness contract.
3. **Frontend** — type-check, lint, build, open-source route smoke.
4. **Eval Contract Gates** — `make eval-e1-gate`, `make agent-eval-core-gate`, `make kb-unit-gate`, the KB development-fixture `make kb-golden-gate`, and supporting units. `make kb-release-evidence-gate` stays blocked until reviewed release evidence exists and is an explicit release gate, not a CI structure check.
5. **KB Migration Gate** — `make kb-migration-gate` against a CI Postgres service.
6. **Rust Changed Crate** — locked-toolchain fmt/check/changed-crate tests on the hosted runner.
7. **Release Readiness** — public docs present, demo seed dry run.

Run the applicable task checks or reuse their valid results before claiming the task verified.
CI's complete gate list is not a required local replay. Native checks use the shared Docker
workflow locally or hosted CI; Runtime/Worker retain their controlled workflow, not host Cargo.

This is not yet a complete product release gate: normal Gateway/Knowledge/DB suites and Web Node
units are not comprehensively wired, the RAG gate is fixture replay rather than live execution,
and the current OpenAPI isolation check can skip when Gateway is unavailable. Run the direct gate,
state its evidence tier and skip count, and do not infer unexecuted coverage from CI green.

### Migration 101 is restore-required

Migration 101 replaces the segment conflict identity
`(document_id, position)` with `(document_id, content_type, position)`. An N-1
writer therefore cannot write safely after 101: its old `ON CONFLICT` target no
longer has a matching unique constraint.

Before the serial release window, run `make backup`, verify that the backup
artifact is non-empty/readable, and retain it outside the database being
upgraded. Apply 101 only with the matching application version. Returning to
N-1 requires stopping the new application, restoring the pre-101 dump into a
replacement database, verifying the restored N-1 constraint and retained
rows, and only then starting the old image against that replacement database.
Starting an old image against the post-101 database is not a rollback path.

`make kb-migration-gate` proves both sides with real PostgreSQL: the N-1 upsert
works before 101, fails after 101, and works again only after a real
`pg_dump`/`pg_restore` round trip into a new temporary database. The supported
central-chain entrypoints are `make migrate` and
`database-authority migrate`. Historical Python migration wrappers and single-file
runners are archived references. The native authority executes the complete
validated migration plan; it does not select a single version or service.

### OpenAPI ownership and export

`sdk/openapi.json` remains the published SDK contract. Current executable
commands use the native producer and comparator; archived Python/FastAPI profile
receipts describe the previous architecture and are not active export instructions.

- `make verify-openapi-contract` aliases `make verify-native-openapi-contract`.
- Export a source-owned candidate with `make snapshot-native-openapi
  NATIVE_OPENAPI_PRODUCER=/external/target/debug/ai-platform-openapi
  OPENAPI_OUTPUT=/external/native-only.json
  OPENAPI_COVERAGE_OUTPUT=/external/native-coverage.json`.
- Check the full published contract with `make verify-native-openapi-contract
  NATIVE_OPENAPI_CANDIDATE=/external/independent-native-export.json`.
  Missing candidates and incomplete native coverage fail closed. The bounded
  producer's coverage report is authoritative; its output is not a complete
  release claim. Do not copy or merge the publication into a candidate.
- `make compare-openapi-candidate OPENAPI_CANDIDATE=/external/candidate.json`
  is only artifact compatibility analysis, not independent-source or live proof.
- `make live-openapi-contract-gate` performs a real GET `/openapi.json` and the
  original published-path/required-request-field checks. Unavailable stack or
  snapshot yields explicit SKIPPED evidence, never PASS. Set `GATEWAY_BASE_URL`
  to select the intended authorized stack.

Native handler/DTO source evidence and publication/profile data remain distinct.
Historical mixed-baseline-versus-Python failures remain archived evidence, not a
reason to suppress current native mismatches. Offline producer/comparator tests:
`scripts/new/native-container.sh test -p ai-platform-openapi`.

#### Response alternative diagnostics

The comparator preserves the existing top-level response property-declaration
check, including optional properties. It combines `allOf` declarations and only
credits a `oneOf`/`anyOf` property when every alternative declares it, recursively
through local references and nested compositions. OpenAPI 3.1 response-level
reference siblings are conjunctive; 3.0 reference siblings stay ignored. This is declaration coverage,
not enforcement of `required` response fields. Unresolved/cyclic alternative
branches supply no property evidence; a field in just one branch is insufficient.

Value shape is checked separately for supported explicit top-level field JSON
types: expanding an array declaration to array-or-string remains a contract
violation even when every branch declares the field. The bounded type projection
unions alternatives, intersects `allOf`, and treats integer as a subset of number.
Unknown types, cyclic/unresolved references, validation siblings of references,
overlapping value-level `oneOf`, enum/const and conditional/negated schemas are
not inferred. Enclosing conditional, negated, enum/const and unsupported object
constraints likewise supply no field-type proof. Nested object fields, array items, constraints and
schema satisfiability are not a general compatibility proof. Existing required
native-producer and runtime evidence requirements remain unchanged.

Offline regressions: `scripts/new/native-container.sh test -p ai-platform-openapi --test response_alternatives`. These diagnostics do not alter the SDK publication or native bodies.

### Bounded native source export (not the full native release gate)

The `ai-platform-openapi` source declares bounded native source-owned operations
from `platform-gateway-foundation/src/native_contract.rs` and its owning declarations.
Use the current producer coverage report and source-bound receipts in
[native enterprise state](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)
for measured counts; do not copy a dated count into this command contract.
The separate `sdk/native-common-contracts.json` evidence manifest does
not rewrite the archived Python ownership declaration or the publication. See
[common native role contracts](../architecture/native-common-role-contract.md).
It does not load the published SDK file, initialize Gateway, read runtime
configuration, connect to storage, or start a listener. Existing native handlers
share request DTO declarations, route constants and validation bounds with the
producer. A small macro shares serde fields/defaults with native-owned schema
expressions; this is not general automatic Rust type/schema inference. Dynamic
validation (future timestamps, byte counts, normalized UUID uniqueness, cursor
pairing and identity/revision binding) is explicit in native extensions.

Build only in an approved Rust environment, following the runtime/build policy:
`scripts/new/native-container.sh build -p ai-platform-openapi`.
Keep the target directory outside the repository. Then run:

```bash
make snapshot-native-openapi \
  NATIVE_OPENAPI_PRODUCER=/external/target/debug/ai-platform-openapi \
  OPENAPI_OUTPUT=/external/native-only.json \
  OPENAPI_COVERAGE_OUTPUT=/external/native-coverage.json
```

The wrapper executes the producer twice, checks byte determinism and its embedded
source digest, checks exact explicit native ownership/operation IDs and current
handler/shared-constant evidence, and emits a separate coverage report. Outputs
must be distinct external files, cannot be symlinks/hardlinks or alias the producer,
and cannot overwrite the mixed SDK publication. `NATIVE_OPENAPI_PREVIOUS` may name
a prior bounded source export; any change in paths, parameters, request constraints,
security or components fails for explicit review (even compatible additions).

A bounded export pass is only source/export evidence. Success response envelopes,
plugin DTOs and notification SQL JSON projections now own their schema declarations.
Dynamic database rows remain open; error bodies are unconstrained and listed
statuses are observed handler branches, with an explicit undeclared default for
other middleware, extractor and collaborator outcomes. The full published contract
still contains operations outside this bounded export. The header for bound API-key authentication is
configurable in deployment; `x-api-key` describes the default, not every deployment.
Full/native `make verify-native-openapi-contract` stays required and fails with
this partial document. Do not merge the published schema into this document,
interpret unconstrained responses as support, or relabel this as CL-01–06/live
acceptance. The complete published file is retained; Python/hybrid exports are
archived migration evidence, not a runnable backend.

Offline regressions: `scripts/new/native-container.sh test -p ai-platform-openapi`. Native serde/export tests live in the lightweight
producer crate; Python private-export tests are archived implementation references. Compile the consuming Gateway library when changing shared DTOs.


#### Native-only response schema evidence

`native_responses.rs` owns the actual shared success-envelope builders, existing
plugin serialization DTOs and notification SQL JSON projection keys/expressions.
The producer now exports useful 200-response fields for all 15 native-only
operations. Optional plugin lifecycle fields and acknowledgement timestamps remain
present with null values. Database `to_jsonb` rows are not narrowed to invented
closed models; no response is deserialized, filtered or newly rejected.

The SQL projections preserve the existing SELECT/RETURNING expressions, tenant
filters, ordering, bind parameters and null behavior. Offline tests compare them
against captured preimage SQL ignoring whitespace outside literals, and compare
representative new/old plugin DTO and envelope wire bytes. This is structural
source evidence, not executed database validation.

Error status declarations are source observations. Their JSON bodies stay `{}`
because middleware/extractors/collaborators can emit different bodies under the
same status. An explicit default remains undeclared. `responses_declared: false`
therefore remains intentional even though `success_responses_declared: true` is
now set. Coverage reports list all bounded-response operations and remaining
default-response gaps separately. The wrapper checks independent digests for both
request/route and response source modules.

Security metadata now names JWT bearer format and uses the same `X-API-Key`
constant as native runtime defaults. Plugin endpoints still accept JWT only;
scheduling/notifications use JWT or owner-bound keys; configuration-audit access
requires authenticated administrator admission. Source evidence binds the actual
credential selection and response construction. The published audit operation's
missing security declaration remains an explicit comparator mismatch; metadata
differences are not suppressed or copied from the old publication to obtain green.


### Common Services lifecycle source declarations

The six common Services operations retain their published contract. Their former
Python ownership declaration is archived architecture history; current service
execution and source declarations are Rust. The source-owned route/method inventory and production
mutation/list/schema builders are shared with the bounded exporter. List success
is an array with registered-service and virtual-assistant variants; only this
operation has the verifier's array-envelope exception. Dynamic dictionary
requests, metadata, schemas, availability values and detail rows stay open.
See [the Services contract boundary](../architecture/native-common-service-contract.md)
for exact semantics and verification requirements. Existing whole-native gate
failures, including authentication differences, are not suppressed.

The control/sandbox image wrappers require public, independently verified exact
base-image references: `CAPABILITY_RUST_BUILD_IMAGE` and
`CAPABILITY_WORKER_RUNTIME_IMAGE`, plus `PYTHON_SANDBOX_IMAGE` for the sandbox.
Each must end with `@sha256:<64 lowercase hex digits>`; no digest is invented or
inferred from a mutable family tag. The integration owner selects/measures these
pins before an authorized build. The sandbox context includes the tracked
`deploy/agent-runtime-source/python/` directory as `python/` beside `codex-rs`,
LICENSE and NOTICE. Both wrappers archive the pinned upstream, apply the verified
overlay, validate source/overlay/schema/artifact/binary labels and record only the
matching artifact (`capability_worker` or `python_sandbox`). Direct wrapper calls
re-enter the existing shared build preflight/lock. No builder is created: an
absent/incompatible existing builder fails closed.

### Generated platform source projections

| Purpose | Command |
| --- | --- |
| Verify shared workload TLS/data-connection projections | `make source-workload-check` |
| Update those two projections from their native authority | `make source-workload-sync` |
| Verify composed Quiz/core compatibility projections | `make source-plugin-check` |
| Update those two projections from their plugin authority | `make source-plugin-sync` |

All four targets first build `platform-tools` through `scripts/new/native-container.sh`
in the existing serialized native Docker cache, then run that exact cache binary
through the same helper. No installed host `platform-tools` shim is needed. The
existing compiler image/cache and retained native lock controller must already be
prepared; these targets never fetch dependencies, create a builder or start services.

The check commands retain read-only source mounts. Only these exact binary
arguments may request writable generated inputs:

```text
scripts/new/native-container.sh run --binary platform-tools -- source-lock sync-workload-source --repo-root /source --apply
scripts/new/native-container.sh run --binary platform-tools -- source-lock sync-plugin-source --repo-root /source --apply
```

`--repo-root` may alternatively be the same canonical absolute host checkout root;
the launcher maps it to `/source`. Relative roots, alternate roots, extra output
options, reordered/duplicate flags and other `source-lock --apply` commands are
rejected. Other application commands gain no source write mounts. Workload sync
binds only `ai-platform-capability-contract/src/shared_workload_identity` and
`ai-platform-capability-contract/src/shared_data_connection`; plugin sync binds only
`ai-platform-plugin-quiz` and `ai-platform-python-compat`, all below
`rust/agent-runtime-overlay/kernel-rs/`.
These four public generated directories must already exist, belong to the
checkout owner, and have no symlink leaf or parent. A missing directory fails;
no parent or entire checkout is made writable to create it. Canonical upstream,
source authorities and other build inputs retain their read-only mounts.

The two plugin targets already have read-only binds in the base Compose file.
Plugin apply alone loads `deploy/platform-native/dev/compose.source-plugin-sync.yml`,
which explicitly sets `read_only: false` and `create_host_path: false` for those
same two targets. It does not rely on a short-volume override clearing an inherited
read-only option. Workload apply retains its two more-specific child binds. Both
writer paths check the effective directories are writable before invoking the tool;
checks, ordinary builds and unrelated commands never load the plugin override.

A source writer uses the checkout owner's UID/GID so the compiler role's separate
cache UID cannot cause permission failures on host projections; it does not
chmod/chown the checkout or add container capabilities. The existing no-network,
read-only container root and explicit public environment remain. Compose uses
`--env-file /dev/null` with automatic `.env` loading disabled. A successful sync
updates controlled source inputs only; source-lock/image/runtime gates still apply.

Offline launcher checks need no Cargo or Docker:

```text
rustc +1.99.0 --edition=2024 --test -C debuginfo=0 scripts/new/native-container.rs -o /external/launcher-tests
/external/launcher-tests --test-threads=1
make -n COMPOSE=docker SHELL=/usr/bin/false source-workload-check source-workload-sync source-plugin-check source-plugin-sync
```

The explicit `SHELL=/usr/bin/false` prevents Make's parse-time Compose version
probe from invoking Docker during this dry run; the inert `COMPOSE` value is unused
by these four source targets. Keep the test executable and scratch outside the checkout and
shared Cargo cache. These source/argv tests do not claim a Docker sync was run.

### Standalone SDK engine checks

Use `scripts/new/native-container.sh engine-build -p ai-platform-sdk-agent-engine`
and `scripts/new/native-container.sh engine-test -p ai-platform-sdk-agent-engine -- --test-threads=1`.
Both select the fixed engine manifest, retain the existing serialized Rust build
fence and run offline against the same Docker Cargo cache. `engine-fetch` is an
explicit public-registry preparation step; tests never enable network or forward
runtime credentials. These checks do not establish a live SDK Worker journey.

The compiled Quiz plugin has its own core/Gateway workspace. Run
`scripts/new/native-container.sh plugin-test --workspace --features executor -- --test-threads=1`
to execute both crates' internal tests in the shared native Docker cache. Native
Gateway compilation alone does not execute dependency-crate unit tests. The
composed Worker uses generated sibling manifests and its own controlled lock.

### Package already-built native development executables

Use `scripts/new/native-container.sh package --component gateway|knowledge|migrate|tools
--output NEW_ABSOLUTE_EXTERNAL_DIRECTORY --tag agentforge-native-local:NAME --build`
to package an existing `/native-cache/target/debug` binary from the one Docker cache.
No Cargo/Cargo-chef runs and no service starts. Application images contain the selected
binary and its runtime dependencies, without compiler, private env or Rust source. Omit `--build` to export only. Create the external parent first; existing
exports and nonlocal image namespaces are rejected. The same retained resource controller
serializes packaging. Successful builds emit public `image.env` using the existing
`GATEWAY_IMAGE`, `KNOWLEDGE_IMAGE`, `MIGRATE_IMAGE`, or `LOCAL_FRESH_TOOLS_IMAGE` variable.
See [the native development packaging workflow](../../deploy/platform-native/dev/README.md#package-the-existing-development-binaries-without-recompiling)
for dependency closure, receipts, source provenance limits and the unchanged runtime
hardening/production build boundary.


### Local frontend from an already verified production artifact

`web/Dockerfile.artifact` is a local integration packaging path. The normal
`web/Dockerfile` remains the production source/dependency build. The packager uses
only Node 26 standard libraries; it never runs Docker, installs dependencies,
rebuilds Vite, loads private env files, or starts services.

First inspect an external dist directory previously produced and tested by the
pinned frontend toolchain:

```sh
node scripts/new/package-frontend-artifact.mjs --dist /external/verified-dist
```

Review its `artifactSha256` and `checkoutSha256`, then pass those exact values to
create a new external output directory (its parent must already exist):

```sh
node scripts/new/package-frontend-artifact.mjs \
  --dist /external/verified-dist --out /external/new-frontend-package \
  --expected-dist-sha256 VERIFIED_DIST_SHA256 \
  --expected-source-sha256 REVIEWED_CHECKOUT_SHA256
node scripts/new/package-frontend-artifact.mjs \
  --verify-context /external/new-frontend-package
```

Verification checks the Vite manifest graph and HTML entry, all referenced files,
public-file equality, and every emitted Brotli/zstd/gzip representation against
its final bytes. Negotiated `/assets` files require all three representations.
Unknown files, symlinks, source maps, source-map references, deployment settings
baked into runtime JSON and private env inputs fail closed. The minimal context
contains compiled assets plus byte-identical current nginx and startup security
files; Vite's source-path manifest and source inventory remain outside the image.
No source tree or node_modules directory is copied. Existing output directories
are never replaced.

The output `image.env` contains the single standard interface
`FRONTEND_IMAGE=agentforge-web-dev:<context identity>`. `artifact-receipt.json`
records the complete expected context inventory, source/SDK/runtime identities,
and exact `dockerBuildArgv`. The checkout snapshot and dist identity are measured
independently; this does not retroactively certify a deterministic source rebuild.
An image ID exists only after the separately authorized Docker build.

The runtime stage retains `USER 101:101`, the current unprivileged nginx base,
port 8080, health check, `/tmp/runtime-config.json`, startup JSON escaping and CSP
header generation. It does not modify the original production Dockerfile. If an
explicit existing local nginx base must be selected, supply `--base-image IMAGE`
at inspection and packaging. The base reference participates in the context
identity.

The runtime owner checks existing Docker project labels and base-image
availability, executes the receipt's Docker argv (`--pull=false`), admits the
resulting exact image ID through the local profile's normal workflow, and exports
`FRONTEND_IMAGE` for Compose. A missing base must not trigger an unapproved pull.
The packager itself grants no deployment/startup authority and does not edit the
local environment or admission records.

Focused offline verification:

```sh
node --test scripts/new/package-frontend-artifact.test.mjs
```
