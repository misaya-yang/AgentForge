# Command Catalog

> The canonical way to do every routine thing in this repo. Agents run these commands rather than
> inventing equivalents, so results are comparable across machines and sessions.
> `harness.yml` holds the machine-readable subset; `make harness-check` verifies they still exist.

**Schema:** `harness/commands/v2`

---

## 0. Toolchain

| Tool | Required version / policy | Notes |
| --- | --- | --- |
| Python | ≥ 3.10 (CI uses 3.12) | Managed by `uv`; the workspace is defined in `pyproject.toml` |
| uv | 0.13.0 | `uv run --all-packages` resolves the whole workspace |
| Node | 26.11.1 | Current stable release; build/CI toolchain |
| pnpm | 12.11.2 | `npm install --global pnpm@12.11.2 --allow-scripts=pnpm` |
| Docker | Compose v2 | `docker compose`, not `docker-compose`, wherever possible |

Never mix application package managers: **pnpm for `web/`, uv for Python.**
Node 26 does not bundle Corepack. The global npm command above only bootstraps the
pinned pnpm executable; never use npm to install web dependencies or bare pip for
Python dependencies. `web/pnpm-workspace.yaml` preserves the existing security
overrides and refuses a mismatched pnpm executable. Use
`pnpm -C web install --frozen-lockfile`. pnpm 12 adds a separate package-manager
lock document; the existing application dependency document remains unchanged.
New toolchain releases
should replace these tested pins together after the same gates pass.

Node 26.11.1 is the latest Current release verified on 2026-10-12; Node 24.21.0 is
the latest LTS. Node is only the frontend builder/CLI test host here, and the
frontend runtime remains Nginx. The SDK CLI keeps its existing Node 22-compatible
output/API baseline while CI runs on the current toolchain.

Official releases: [Node.js](https://nodejs.org/dist/index.json),
[pnpm 12.11.2](https://github.com/pnpm/pnpm/releases/tag/v12.11.2),
[uv 0.13.0](https://github.com/astral-sh/uv/releases/tag/0.13.0).

## 1. Machine and stack

| Purpose | Command |
| --- | --- |
| Preflight the machine before anything else | `make doctor` |
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
| Apply changed Python source without rebuilding | `make hot-update` |
| Preview / load local demo data | `make seed-demo` / `make seed-demo-apply` |

`*-build-local` means the Docker daemon is local; Cargo runs only in the multi-stage Docker builder.
`make agent-runtime-recovery-tests` runs Runtime/Worker library tests and the
scoped Core original-turn recovery/suspension regressions inside that builder,
using `AI_PLATFORM_AGENT_RUNTIME_SOURCE` for the clean pinned upstream. It is
local Docker evidence, separate from the hosted CI gate.
Do not run host Cargo/Rust commands. `make rust-changed-crate-gate` is a hosted-CI entrypoint on this
machine, not a local acceptance command.

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

`python -m database.authority migrate` is the only schema writer; `make migrate`,
`scripts/new/migrate.sh`, `database/cli.py`, and
`database/migrate_per_service.py` are compatibility delegates to that command.
The authority recognizes historical filename, numeric, and per-service ledgers
only as adoption input, then freezes them after the baseline marker. Ambiguous
duplicate numeric revisions fail closed with a reconciliation receipt.

The authority acquires one PostgreSQL session advisory lock before bootstrap,
discovery, or DDL. A concurrent runner waits, then reloads the authoritative
ledger and exits as a no-op; connection loss releases the lock.

Python Gateway startup runs the authority's read-only `startup-check` as the
`gateway` principal against the configured application DSN before opening its
shared-storage pool, usage flusher or password bootstrap. Its supported epoch
window remains **2..10**; epoch 17 requires the native Gateway and is not made
compatible by retaining Python Knowledge/SDK services. An incompatible schema,
unavailable database or failed pool/bootstrap aborts startup; any partially
opened pool is closed, so `SELECT 1` cannot make that failed startup ready.
Gateway never migrates or retries under a different database identity.

`make migrate-init` maps to the authority's `init-fresh` command and therefore
requires a truly empty database plus a complete frozen baseline. It never
falls back to directly piping `database/schema.sql`.

Restoring over shared data requires a current backup and explicit approval.

## 4. Development

For a restored checkout, `make local-bootstrap` installs the existing pinned
toolchain and frozen dependencies in isolated local directories. Then generate
local configuration with `bash scripts/new/init-env.sh --env .env --if-missing`
and run `make local-baseline`. See [local development](local-development.md) for
the project/recovery boundary, actual results and external prerequisites. These
entry points do not start services. Prefix any existing command with
`bash scripts/dev/with-local-toolchain.sh` to use the same tools and frozen locks.

| Purpose | Command |
| --- | --- |
| One-shot local dev environment | `make dev-setup` |
| Start / stop / reset / inspect dev containers | `make dev-start` / `make dev-stop` / `make dev-reset` / `make dev-status` |
| Run backend services from mounted source with reload | `make dev-compose` |
| Follow mounted-source logs | `make dev-compose-logs` |

## 5. Python tests and lint

Always scope to the paths you touched — the full suite is long.

```bash
uv run --all-packages --extra test pytest -q --no-cov tests/<area>/test_<file>.py
```

```bash
uv run --all-packages --extra dev ruff check <paths>
```

Notes:

- `--no-cov` is required for scoped runs; the project sets `--cov-fail-under=25` globally.
- `asyncio_mode = auto` — do not add `@pytest.mark.asyncio`.
- Tests marked `integration` need a live stack; state clearly when you skip them.

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

- `make sdk-sse-contract` — runs the shared Python and CLI SSE fixture contract; also runs Java
  and Dart native tests when Maven or Dart is installed, otherwise prints an explicit skip.

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
| Python/hybrid OpenAPI | `make verify-openapi-contract` (profile-scoped; native release requires the next gate) |
| Native full published OpenAPI | `make verify-native-openapi-contract NATIVE_OPENAPI_CANDIDATE=/path/to/independent-native-export.json` (missing candidate fails) |
| RAG retrieval | `make rag-eval-regression-gate` is fixture-only evidence; implementation/live quality require separate ARC-00 gates |
| Knowledge service (real KB suite, imports `knowledge_service`) | `make kb-unit-gate` |
| KB migrations 100–112 + public/main-N-1/split full-chain ledger replay, 097 late-table handoff, 101 dump/restore boundary, and durable embedding-action jobs (needs Postgres with temporary-database privilege plus `pg_dump`/`pg_restore`) | `make kb-migration-gate` |
| KB development fixture structure / manifest hashes / seed drift (`tests/fixtures/eval/rag/golden/**`; not a release claim) | `make kb-golden-gate` |
| KB T0 release evidence (200–400 reviewed cases, source mix, manifest/pointer/reviewed real-corpus baseline binding) | `make kb-release-evidence-gate` |
| Project a golden JSONL into the versioned Postgres store (PRD T0-#2; `--dry-run` first) | `uv run --all-packages python scripts/import_kb_eval_golden.py <jsonl> --dry-run` |
| Qdrant integration smoke (needs `docker-compose.kbms.yml` up; skips when down) | `make kb-integration-smoke` |
| Real-corpus retrieval baseline candidate (live KS; records distribution only, does not approve it) | `make kb-baseline-record KB_BASELINE_DATASET_ID=<id>` |
| RAG eval fixtures / `scripts/eval_rag.py` | `make rag-eval-regression-gate` |
| Deploy scripts, `.env.example`, Compose | `make validate-example-config` |
| Shared/Java/Dart SDK streaming change | `SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract` (plain command is optional-tool sub-evidence) |
| Independent CLI config, launcher, provider adapter, package | `make independent-cli-gate` |
| Independent native CLI composed-source artifact | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-cli-build-local` |

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
`python -m database.authority migrate`; `database/run_migration.py` is retired
and always refuses single-file execution before reading credentials. The
historical `database/cli.py` and `database/migrate_per_service.py` names are
thin complete-plan delegates and cannot select a version or service.

### OpenAPI profile ownership and export

`sdk/openapi.json` remains the complete published SDK contract, byte-for-byte
unchanged by a Python export. `sdk/openapi-profiles.json` records the exact
all Python/common paths and native-only operations, operation IDs, native auth schemes and owning Rust
route/dispatcher evidence. Ownership is explicit, not inferred from missing
routes. Stale IDs, source evidence, duplicate ownership and Python/native
security or route overlap fail closed. Every published path must belong to the
explicit partition. Reclassifying a common path requires reviewing the existing
Python ownership declaration too; adding a native exclusion alone is rejected. Common operations stay in the Python
baseline; new unknown removals are never automatically excluded.

- Python/hybrid: `make verify-openapi-contract` exports the actual FastAPI app
  and compares against the published baseline minus explicitly native-only
  operations/schemes. Fresh hash-seeded processes verify operation IDs.
- Candidate export: `make snapshot-gateway-openapi OPENAPI_OUTPUT=/tmp/python-openapi.json`.
  The equivalent script requires `--profile python-hybrid --output PATH` and
  refuses to overwrite the mixed publication, including symlink/hardlink aliases.
  Publication updates require review against the full previous SDK contract.
- Native: `make verify-native-openapi-contract NATIVE_OPENAPI_CANDIDATE=/tmp/native-openapi.json`
  compares an independently produced full native Gateway document against the
  entire published contract, including common operations and native auth.
  The bounded native producer described below covers only explicitly declared
  source-owned operations; current coverage and missing-operation counts belong
  to its source-bound coverage report and the active program's receipts. It does
  not yet fully declare the published contract or host/error responses. A complete independent export remains an open
  release prerequisite. Even a matching supplied candidate
  returns exit 2 for the required native gate until a supported independent
  producer/export evidence path exists. `comparison_result` records diagnostic
  compatibility separately from the failed required verification. Do not copy the publication as a
  candidate. Missing candidates fail with exit 2 and error evidence. The baseline
  file itself is rejected as a candidate. Artifact comparison cannot certify
  native implementation fidelity, determinism, or live runtime behavior.

For a comparison-only diagnostic use
`make compare-openapi-candidate OPENAPI_CANDIDATE=/tmp/candidate.json`; evidence
labels `result_kind: artifact-comparison-only`. This is not a required native gate.

Direct gate invocation accepts `--profile python-hybrid|native|published`;
`published` (the default) is an artifact-comparison diagnostic requiring a full
candidate; its pass is not independent native producer or release proof.
Evidence names the profile, full-publication SHA-256, compared counts and actual
hash-seed checks. A Python-profile pass is not a complete/native release pass.
The historical mixed-baseline-vs-Python failure remains valid historical evidence.
No downstream wildcard paths or native endpoints are hand-added to Python output.

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

Offline regressions: `uv run --all-packages --extra test pytest -q --no-cov tests/contract/test_openapi_response_alternatives.py` and the comparator's
`--selftest`. These diagnostics do not alter the SDK publication or native bodies.

### Bounded native source export (not the full native release gate)

The `ai-platform-openapi` source declares bounded native source-owned operations
from `platform-gateway-foundation/src/native_contract.rs` and its owning declarations.
Use the current producer coverage report and source-bound receipts in
[native enterprise state](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)
for measured counts; do not copy a dated count into this command contract.
The separate `sdk/native-common-contracts.json` evidence manifest does
not change Python-hybrid ownership or the publication. See
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
acceptance. Python/hybrid export and the complete published file are retained.

Offline regressions: `pytest -q --no-cov tests/contract/test_native_openapi_export.py`
through the normal uv command; native serde/export unit tests are in the lightweight
producer crate. Compile the consuming Gateway library when changing shared DTOs.


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

The six existing common Services operations retain their Python ownership and
published contract. The source-owned route/method inventory and production
mutation/list/schema builders are shared with the bounded exporter. List success
is an array with registered-service and virtual-assistant variants; only this
operation has the verifier's array-envelope exception. Dynamic dictionary
requests, metadata, schemas, availability values and detail rows stay open.
See [the Services contract boundary](../architecture/native-common-service-contract.md)
for exact semantics and verification requirements. Existing whole-native gate
failures, including authentication differences, are not suppressed.
