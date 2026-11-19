# Native repository operations

`platform-tools` is a Rust binary. It never executes a Python interpreter or
imports a Python implementation. Failed integrity checks exit 1; topology errors
exit 2, matching the deployment helper contract.

## Existing command replacements

| Existing operation | Native command |
| --- | --- |
| `scripts/harness/check_harness.py` | `platform-tools harness-check --repo-root ROOT` |
| Rust context boundary validation | `platform-tools architecture-boundary --repo-root ROOT` |
| Operational interpreter/source inventory | `platform-tools python-inventory --repo-root ROOT [--require-zero]` |
| `agent_runtime_supply_chain.py validate` | `platform-tools source-lock validate --repo-root ROOT --lock FILE [--require-runnable] [--require-artifact ID]` |
| `agent_runtime_supply_chain.py verify-local-images` | `platform-tools source-lock verify-local-images --repo-root ROOT --lock FILE [--runtime-image IMAGE] [--worker-image IMAGE]` |
| Source bundle/overlay identity | `platform-tools bundle-identity --directory DIR [--overlay]` |
| `agent_runtime_source_candidate.py` | `platform-tools source-candidate verify --package PACKAGE --upstream CHECKOUT --repo-root ROOT [--composed-source DIR]` |
| Verified source assembly | `platform-tools source-candidate assemble --package PACKAGE --upstream CHECKOUT --repo-root ROOT --output NEW_DIR` |
| Recovery ZIP integrity | `platform-tools recovery-archive verify --archive FILE [--require-zero-python]` |
| `scripts/deploy/topology_modes.py` | `platform-tools topology --repo-root ROOT --mode full [--check-all] [--service-ids all\|app\|infrastructure] [--service-replicas ID]` |
| Inline JSON receipt readers | `platform-tools json-get FILE dotted.path` |
| File digest readers | `platform-tools sha256-file FILE` |

Topology accepts `--manifest FILE` for relocating the unchanged topology JSON.
Without it, the retained `src/core/data/service_topology.json` location is used.
`json-get`, `sha256-file`, and topology service-list/replica forms print plain
values, suitable for existing shell command substitution. Other commands emit
JSON. Missing fields and unsafe paths fail closed.

## Guarantees

- Harness validation covers required docs, line budgets, followed relative links,
  workspace hygiene, Make entrypoints, gate schema, outcome-preserving CI wiring
  and per-schema runbook contracts. Evidence retains the existing gate JSON shape.
- Architecture validation uses actual Cargo dependency declarations including
  target/build/dev dependencies. It rejects links across hosted service contexts;
  shared contract crates are not mislabeled as service implementations. The exact
  transitional Knowledge → Gateway domain-library edge is permitted only by the
  explicit, dated `native-boundary-exceptions.json` entry and reported separately.
  The gate does not claim these two crates are fully decoupled.
- Source lock validation checks source/manifest/overlay/lock/schema/SBOM/license
  identity and immutable OCI release state. Source edits invalidate old locks;
  this command does not silently regenerate release evidence.
- Candidate validation preserves payload inventory, modes, SHA-256, compiler
  evidence, metadata, Git upstream blob identity, live preimages and composed
  inventory. Assembly first verifies all preimages and refuses an existing output
  directory, then verifies the complete result. No live pins are changed and no
  runnable/release admission is granted.
- Recovery ZIP validation rejects unsafe/duplicate names and symlinks, checks
  exact manifest membership, member SHA-256/length and ZIP CRC. ZIP size remains
  bounded to 122 MiB; expanded data is additionally bounded to 1 GiB and each
  member to 256 MiB. It validates without extraction or overwriting a checkout.

## Operational scope

Native source generation and refresh, image recording, resource locks, packaging,
performance measurement and deployment-script contracts are implemented here.
Live text acceptance is a Gateway binary; recovery and database authority tools
are in the migration-authority crate. Evaluation and release commands live in
their dedicated native crates. Implemented live operations still need their
explicit environment acceptance runs; offline tests do not certify a deployment.
Unknown commands fail rather than returning fabricated success.

## Additional native CI/fixture helpers

- `platform-tools resolve-path PATH`: home expansion, real symlink resolution and
  lexical nonexistent suffix, for external backup destination guards.
- `platform-tools embed-header-check --repo-root ROOT`
- `platform-tools embed-fixture-server [--bind 0.0.0.0:8080]`
- `platform-tools affected-gates --base SHA [--harness FILE] [--evidence-out FILE]`
- `platform-tools ci-gate-enforcement [--selection FILE] [--job-results-json JSON]`
  reads `CI_JOB_RESULTS_JSON` by default.
- `platform-tools rust-changed-crate-gate --base SHA [--source CHECKOUT |
  --fetch-public-source NEW_DIR] [--cargo EXE] [--rustc EXE]`
  requires hosted CI, pinned tools, Git and coreutils `timeout`; native workspace
  changes run directly and sparse overlay changes run only after verified
  upstream composition. Lock changes conservatively select the whole overlay.

These three CI commands also support `--selftest`. Affected-selection and Rust
execution input errors exit 2, test or coverage failures exit 1. Evidence JSON
uses the existing paths under `tmp/gate-evidence`.

- `platform-tools evidence validate|status|cleanup [--policy FILE] [--path REL]...`
  preserves dry-run defaults; apply requires `--apply --authorization FILE
  --quarantine EXTERNAL_EXISTING_DIR`. Validates Git provenance, durable hashes,
  oracle references and exact authorization. Quarantine is reversible and uses a
  new run directory; rejected auth/raw/tracked/external candidates are not read.
- `platform-tools topology-inventory [--manifest FILE] [--source-sha SHA]
  [--output FILE]` derives the current inventory from topology, six live Compose
  files, OpenAPI and native route registration source. It labels the Rust static
  route count explicitly instead of calling it a Python decorator count.
- `platform-tools single-instance-guard [--manifest FILE] [--source-sha SHA]`
  independently pins Gateway/Agent Runtime single-instance semantics and checks
  live Helm defaults/production/templates. `--selftest` is also supported.

Topology inventory normally derives identity from actual Git HEAD. A recovered
source-only archive may supply an explicit verified `--source-sha`; absence of
both is an error, never an invented Git revision.

- `platform-tools loc-no-growth [--baseline FILE] [--selftest]`: immutable
  Git/blob-bound oversized Rust/TypeScript ledger, Rust800 and TS500 caps.
  A historical Python-only ledger fails until explicitly migrated.
- `platform-tools hygiene-check [--allowlist FILE] [--skip-baseline FILE]
  [--selftest]`: real syn Rust AST, TypeScript tests, skip-drift and expired/stale
  scoped exceptions. Missing Rust baseline markers mean no approved Rust skips.

- `platform-tools known-candidates`: validates the four explicitly scoped
  candidate IDs, original provenance and native repair mappings. Repair tests
  must invoke proxy transport and assert the required status plus slot release;
  shared called assertion helpers are inspected. Dead symbols must be absent
  from actual Rust AST. Missing mappings fail closed.
- `platform-tools resource-lock status`
- `platform-tools resource-lock run --resource rust-build --timeout-seconds 7200
  --heartbeat-seconds 10 --expected-end-condition TEXT [--runtime-identity-json
  JSON] -- COMMAND ARG...`
- `platform-tools resource-lock force-release --resource RESOURCE --owner-token
  TOKEN --reason TEXT [--allow-cross-host] [--allow-live-owner]`

Resource locking retains shared Git-common-dir mutual exclusion, mode0600
fsynced atomic receipts, bounded/redacted runtime identity, same-host dead PID
plus elapsed heartbeat timeout stale proof, explicit force-release audit and
unknown-cleanup fences. Child execution receives original arguments; receipts
receive redacted copies. Native process groups are terminated and proven gone
before ownership is released. Run propagates child status; lock errors exit2.

Operational equivalents:
- `source-lock refresh-source-lock --repo-root ROOT --lock LOCK`
- `source-lock refresh-overlay --repo-root ROOT --lock LOCK --cargo-workspace DIR [--cargo-metadata FILE]`
- `runtime-smoke-payloads` reads the bounded SMOKE_* fixture variables and emits the five original canonical payload/hash lines
- `capability-worker-smoke basic|recovery` uses CAPABILITY_SMOKE_* and the production Rust lease signer; intended only for the isolated loopback smoke worker
- `rollback-helper env-value FILE KEY`, `image-field BUNDLE SERVICE FIELD`, `api-fingerprint CREDENTIAL_FILE`, and `report` preserve the rollback shell evidence contract

These commands perform actual requests/build metadata inspection when invoked. Merely compiling them does not establish live rehearsal or provider acceptance.

Native fact inventory generation:
- `platform-tools inventory-generate --repo-root ROOT` verifies the six native baselines by default.
- `--kind service-topology|data-access-inventory|loc-baseline|dependency-baseline|skip-baseline|contract-freeze` selects one producer.
- `--kind core-consumption|persistence-sql|domain-ownership` selects the native core-boundary fact inventories.
- A reviewed refresh requires `--write --source-rev FULL_CLEAN_HEAD`; `--output-dir DIR` selects a separate output directory.

The default output is `docs/architecture/baselines/native-rust-v2`. Historical
`2026-08-post-rag` artifacts cannot be refreshed through this command. Native v2
facts explicitly replace Python AST/import units with Rust syntax/Cargo/source
units; old counts are not silently reinterpreted. The runner reads immutable Git
blobs, proves a clean stable HEAD and source-input identity before and after the
batch, and serializes all output before writing. No wall-clock fields affect
baseline bytes. Missing Git identity or mixed source revisions fail closed.

Data-access scans include Rust literal SQL and literal `include_str!` queries,
DDL/grants, SERIAL sequences and SQL function bodies. This remains static source
evidence, not runtime least-privilege proof. Dynamic SQL/macros and dynamic route
registration require separate review. Contract freeze retains original artifact
hashes and marks live proofs as pending; the full live OpenAPI verification
command remains unregistered rather than being replaced by a bounded check.

`platform-tools live-openapi-contract --repo-root ROOT [--base-url URL]
[--evidence-out FILE]` performs the original live GET `/openapi.json` assertions:
published paths remain and required JSON-body properties (including component
references) are retained. It emits explicit `skipped` if the snapshot or running
stack is unavailable, preserving the source skip contract; this is never a pass
receipt. It does not claim complete schema equivalence. HTTP uses a 5-second
bound, no environment proxy, no redirects and a 32 MiB response limit.
