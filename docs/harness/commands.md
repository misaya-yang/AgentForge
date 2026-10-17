# Command Catalog

> The canonical way to do every routine thing in this repo. Agents run these commands rather than
> inventing equivalents, so results are comparable across machines and sessions.
> `harness.yml` holds the machine-readable subset; `make harness-check` verifies they still exist.

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
| Check living-document scripts, links, top-level CLI names and review dates (also included in harness) | `platform-tools docs-truth-check --repo-root .` |
| Verify the pinned Agent Runtime source release unit | `make agent-runtime-source-contract` |
| Verify the independent local-Runtime CLI | `make independent-cli-gate` |
| Build the independent native CLI inside Docker | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-cli-build-local` |
| Verify the pinned Rust Agent Runtime image | `make agent-runtime-contract` |
| Verify the Agent PostgreSQL ThreadStore | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/path/to/fork make agent-thread-store-contract` |
| Serial single-kernel release contract | `make agent-runtime-release-gate` |
| Digest-pinned current→frozen→current rollback rehearsal | `make agent-runtime-rollback-rehearsal` |
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
do not substitute host Cargo runs for their Docker evidence. For native services/tools,
current `AGENTS.md` also authorizes pinned host Cargo/Rust
compilation and targeted tests. Those runs must be serialized
(`CARGO_BUILD_JOBS=1`), use an external `CARGO_TARGET_DIR`, and disable debug and
incremental build output. This exception does not authorize deployment or establish
local runtime acceptance. `make rust-changed-crate-gate` remains a hosted-CI entrypoint,
not a replacement local acceptance command.

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
| Start / stop / reset / inspect dev containers | `make dev-start` / `make dev-stop` / `make dev-reset` / `make dev-status` |
| Build and run native backend services from the current source | `make dev-compose` |
| Follow native development-service logs | `make dev-compose-logs` |

## 5. Native tests and formatting

Install source-built services and operational commands with
`CARGO_TARGET_DIR=/absolute/external/build-directory make native-install`
in the approved build environment. Keep Cargo output outside the checkout.
Scope tests to the package and target you changed:

```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-tools --test deployment_script_contracts
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

Run the gate that matches what you touched. With real Git metadata, use
`make affected-gates BASE_SHA=<verified-base>` to select declared gates; the
`ci-gate-enforcement` job checks selected CI results. Selection is not execution
and does not prove complete runtime coverage. A source-only archive cannot use a
fabricated Git base: compare its verified source manifest and explicitly record
changed paths and matching gates. Report exactly which checks ran and their tiers.

| You changed | Gate |
| --- | --- |
| Harness files, docs contract | `make harness-check` |
| Agent Runtime source source/schema/SBOM/image lock | `make agent-runtime-source-contract` |
| Agent Rust Runtime image identity | `make agent-runtime-contract` |
| Agent Rust Runtime Docker lifecycle | `AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-smoke` |
| Agent Runtime real Qwen text path | `ENV_FILE=/path/to/.env AI_PLATFORM_AGENT_RUNTIME_IMAGE=... make agent-runtime-text-gate` |
| Agent Runtime read-only capability bridge | `make agent-runtime-readonly-gate` |
| Agent Runtime tool side-effect safety | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-write-gate` |
| Rust cutover offline/sub-evidence bundle (not a platform release gate) | `make agent-runtime-release-gate` |
| Digest-pinned current→frozen→current rollback | `make agent-runtime-rollback-rehearsal` |
| Agent ThreadStore fresh/idempotent/live contract | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/path/to/fork make agent-thread-store-contract` |
| Agent runtime compatibility surface | `make verify-assistant-runtime-dev` |
| Eval / trace pipeline | `make eval-e1-gate`, then `make verify-eval-dev` for the wider branch gate |
| Agent core / subagents | `make agent-eval-core-gate` |
| Agent Studio | `make verify-agent-studio` |
| Service boundaries, Agent API surface | `make architecture-boundary-gate` and `make core-boundary-gate` for their declared import/allowlist scopes; `make test-isolation` and API/runtime contract gates remain separate |
| Native source-owned OpenAPI | `make verify-openapi-contract` (alias of the full native gate; missing independent candidate fails) |
| Native full published OpenAPI | `make verify-native-openapi-contract NATIVE_OPENAPI_CANDIDATE=/path/to/independent-native-export.json` (missing candidate fails) |
| RAG retrieval | `make rag-eval-fixture-contract` is fixture-only evidence; implementation/live quality require separate ARC-00 gates |
| Knowledge service (native production-module tests) | `make kb-unit-gate` |
| Native full-chain/ledger/roles, 97/112, lock serialization, crash rollback and 101 dump/restore (disposable Postgres and pg_dump/pg_restore) | `KB_MIGRATION_TEST_ALLOW_CREATE_DATABASE=1 KB_MIGRATION_TEST_ADMIN_DSN=<disposable-admin-dsn> make kb-migration-gate` |
| KB development fixture structure / manifest hashes / seed drift (`tests/fixtures/eval/rag/golden/**`; not a release claim) | `make kb-golden-gate` |
| KB T0 release evidence (200–400 reviewed cases, source mix, manifest/pointer/reviewed real-corpus baseline binding) | `make kb-release-evidence-gate` |
| Project a golden JSONL into the versioned Postgres store (PRD T0-#2; `--dry-run` first) | `platform-knowledge-ops import-kb-eval-golden <jsonl> --dry-run` |
| Qdrant integration smoke (explicit live gate; fails when the required stack is down) | `make kb-integration-smoke` |
| Real-corpus retrieval baseline candidate (live KS; records distribution only, does not approve it) | `make kb-baseline-record KB_BASELINE_DATASET_ID=<id>` |
| RAG eval fixtures / native `eval_rag` | `make rag-eval-fixture-contract` |
| Deploy scripts, `.env.example`, Compose | `make validate-example-config` |
| Shared/Java/Dart SDK streaming change | `SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract` (plain command is optional-tool sub-evidence) |
| Independent CLI config, launcher, provider adapter, package | `make independent-cli-gate` |
| Independent native CLI composed-source artifact | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-cli-build-local` |

The consolidated KB migration target does not certify every historical embedding
or artifact assertion. Those specialized tests remain unverified obligations;
the native gate prints its exact coverage before execution.

## 8. What CI runs

`.github/workflows/ci.yml` on push/PR to `main` and `dev` currently runs:

1. **Script Contracts** — shell syntax + focused script tests.
2. **Compose and Harness** — `make validate-example-config`, Compose render, harness contract.
3. **Frontend** — type-check, lint, build, open-source route smoke.
4. **Eval Contract Gates** — `make eval-e1-gate`, `make agent-eval-core-gate`, `make kb-unit-gate`, the KB development-fixture `make kb-golden-gate`, and supporting units. `make kb-release-evidence-gate` stays blocked until reviewed release evidence exists and is an explicit release gate, not a CI structure check.
5. **KB Migration Gate** — `make kb-migration-gate` against a CI Postgres service.
6. **Rust Changed Crate** — locked-toolchain fmt/check/changed-crate tests on the hosted runner.
7. **Release Readiness** — public docs present, demo seed dry run.

Run locally supported gates before saying done. Rust fmt/check/test evidence comes from the hosted CI
job; local acceptance consumes a Docker-built or pulled Runtime/Worker image instead of host Cargo.

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
`cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi`.

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

Offline regressions: `cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi --test response_alternatives`. These diagnostics do not alter the SDK publication or native bodies.

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
`cargo build --locked --offline --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi`.
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

Offline regressions: `cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi`. Native serde/export tests live in the lightweight
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
