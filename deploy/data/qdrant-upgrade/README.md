# Qdrant 1.18.2 → 1.19.2 opt-in upgrade

This is a native operations workflow, an isolated Compose overlay, and an independent Helm rehearsal chart. It does not change the base Compose/Helm pin, switch traffic, enable Knowledge quality options, download an image/model, or activate a database proposal.

Official stable releases and OCI index bytes were checked at 2026-10-18 03:16 UTC (2026-10-17 New York). Both indices contain Linux amd64 and arm64 manifests. `reviewed-pins.json` contains the full index/platform digests and official provenance:

- [Qdrant v1.18.2](https://github.com/qdrant/qdrant/releases/tag/v1.18.2), index `sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c`.
- [Qdrant v1.19.2](https://github.com/qdrant/qdrant/releases/tag/v1.19.2), index `sha256:b7b0444c4c351c970b98e90a6f89c2ee4287c65b44e52b4cb503fa5b2aa927ad`.
- [Official upgrade sequence](https://qdrant.tech/documentation/upgrades/) requires intermediate minor versions; this workflow admits only the fixed adjacent 1.18.2 → 1.19.2 step.
- [Official snapshot workflow](https://qdrant.tech/documentation/tutorials-operations/create-snapshot/), [v1.19.2 snapshot API](https://github.com/qdrant/qdrant/blob/v1.19.2/src/actix/api/snapshot_api.rs). The native uploader streams multipart chunks and supplies the SHA-256 checksum; it does not load a snapshot into memory or ask Qdrant to fetch an arbitrary remote URL.

The checked-in pin observation is **not** a snapshot/restore/golden acceptance receipt. All those runtime checks remain `not_run` until the execution owner runs them. Live `metadata` reads official GitHub and Docker Hub metadata again, checks raw OCI bytes against both the registry header and these reviewed immutable digests, checks both architectures, and rejects tag changes. It fetches no image layers. Evidence expires after seven days; use a fresh run rather than editing receipts.

## Root CLI integration

Root owns the shared entry point and manifests. Add:

```rust,ignore
// platform-knowledge-ops/src/lib.rs
pub mod qdrant_upgrade;
// in run's existing command match:
"qdrant-upgrade" => qdrant_upgrade::run(&args[1..]).await,
```

Add `futures-util = "0.3"` to the existing ops dependencies for the bounded snapshot upload stream; its version is already in the workspace lock. No other new dependency is required. The module reuses existing reqwest, Tokio, serde, SHA-256 and chrono dependencies. Reconcile the local-package lock edge through the serialized native build workflow.

## Inputs and controlled preparation

Copy the appropriate example spec to a private external directory, replace every placeholder, and set the spec and golden files to 0600 and the artifact directory to 0700. Paths must be absolute; artifacts must be outside the repository. Every run uses a new `run_id`. Existing artifact files are never overwritten. Specs, Compose/chart/values/env content, frozen golden bytes, official metadata, predecessor receipts and snapshot bytes are hash-bound across stages.

- `source`, `canary`, and `rollback` must be three distinct **private HTTPS origins**, with correctly named server certificates, explicit CA files and private writer-key files. Qdrant API keys are sensitive headers, never CLI arguments, URLs, stdout or evidence. No redirect, proxy discovery, retry, invalid-certificate fallback or public trust-root fallback is used for node requests.
- The source is single-node 1.18.2. Distributed/shard-by-shard upgrade is deliberately rejected; it needs its own HA execution plan. The source remains in place and unchanged by deployment commands.
- The execution owner must preload both reviewed image digests through its approved image/mirror workflow. All tool-controlled Compose runs use `--pull never --no-build`; the Helm chart uses `imagePullPolicy: Never`.
- Compose uses the existing owned project and base file plus `deploy/compose/docker-compose.qdrant-upgrade.yml`. Native preparation first checks source container labels/checkout/storage, refuses reused/attached rehearsal volumes, then creates two run-specific volumes. A temporary networkless, credential-free invocation of the reviewed old image initializes only their root ownership to UID 1000. It never removes a data volume. Preparation and upgrade start only the two explicit rehearsal services, serially.
- Helm uses `deploy/helm/qdrant-upgrade-canary`, a distinct rehearsal release, an explicit context/namespace, existing API-key/TLS Secrets and two pre-provisioned separate fresh PVCs containing the run ID. `canary_statefulset` and `rollback_statefulset` must equal `<release>-canary` and `<release>-rollback`. The chart is disabled by default, has no public service/host port, and admits only the configured operations/API/worker pod labels. It denies egress. Set `clientPodLabels` to the actual permitted pods, and issue a TLS certificate covering the chosen internal service names. Root owns PVC provisioning, cluster image preload, resource budgets and production network policy.
- Source and rehearsal storage names/claims must all differ. The original volume/PVC is never used by a candidate or old-binary rollback. An old binary is **never** booted on a volume already opened by 1.19.2.

Before runtime execution, inspect ownership, current health and memory/disk according to `docs/harness/runtime-and-secrets.md`. The repository metadata check performed during implementation did not start Docker or Kubernetes.

## Frozen retrieval corpus

Supply an operator-reviewed `qdrant-retrieval-golden/v1` artifact, with `corpus_kind=production_export`, its corpus identity, model revision and index generation. Cases contain frozen numeric dense/sparse query vectors, optional existing named-vector/search parameters, exact tenant/dataset filters, expected ordered point IDs, and graded relevance (keys are plain UUID strings or numeric-ID decimal strings). No provider/model inference runs during this workflow.

The corpus must cover **every collection in the source inventory**, with a cross-tenant/empty negative per collection and a positive per nonempty collection. Empty collections need only a negative. Requests use `limit:20` and `with_payload:true`; nested prefetch/cross-collection lookup and inferred queries are refused. The implementation currently bounds the corpus to 1,000 cases and 16 MiB. Export a suitable frozen upgrade corpus before starting; do not substitute empty cases or change a frozen corpus after snapshotting.

`golden.fixture.json` is a synthetic format example. It contains no real measured result and cannot emit deployment admission. Its vectors/IDs are not a product corpus. The native report does not claim Knowledge end-to-end actor/ACL acceptance or quality-feature promotion from raw Qdrant parity. Those remain separate gates.

## Native stage commands

The following is an execution-owner sequence, not evidence that it ran. `<ops>` is the approved compiled native binary. No step executes Python, pulls images or invokes a model.

```sh
<ops> qdrant-upgrade plan --spec /private/run/spec.json
<ops> qdrant-upgrade metadata --spec /private/run/spec.json
<ops> qdrant-upgrade prepare --spec /private/run/spec.json --apply
# Execution owner now fences/stops every source writer and verifies quiescence.
<ops> qdrant-upgrade snapshot --spec /private/run/spec.json --apply --writes-quiesced
<ops> qdrant-upgrade restore --spec /private/run/spec.json --apply
<ops> qdrant-upgrade upgrade --spec /private/run/spec.json --apply --writes-quiesced
<ops> qdrant-upgrade golden --spec /private/run/spec.json
<ops> qdrant-upgrade rollback --spec /private/run/spec.json --apply
<ops> qdrant-upgrade render --spec /private/run/spec.json --writes-quiesced
```

`snapshot` checks the old runtime version, complete inventory, green status, exact point counts, configuration/index schema and frozen baseline results. It creates and streams every collection snapshot to a private external file, computes its SHA-256, and rechecks the source afterward. Alias metadata is recorded separately.

`restore` uploads those exact snapshots into the empty old-version canary, restores aliases, waits for green collections, checks exact count/config/index parity, and runs the complete frozen corpus. It refuses an existing collection rather than overwriting data. `upgrade` verifies the restore chain and source quiescence receipt, checks deployment ownership and preloaded digest, and restarts only that canary with the reviewed 1.19.2 digest. It enforces one minor step.

`golden` checks the new runtime, configuration/counts, every ordered expected ID, score tolerance (absolute/relative 1e-6), payload identity, and unchanged nDCG@20 on positive graded cases. Negative-case nDCG is null. Any cross-tenant/dataset payload, unexpected result, missing case or stale/corrupt evidence fails. It never manufactures a gain.

`rollback` uses a **separate empty 1.18.2 node/PVC**, restores the pre-upgrade snapshots, and reruns configuration/count/alias/golden checks. It rehearses rollback without downgrading the canary's upgraded disk or touching source traffic.

`render` requires the complete rollback chain, a non-fixture reviewed corpus, current source/canary parity and continued write quiescence. It emits `admission.json` plus `compose.verified.json` or `helm.verified.values.json` for review. It does not edit defaults or execute cutover. The Helm values use the main chart's existing `data.qdrant.url`, `port` and `peer` fields to select the verified canary; existing key/CA Secret bindings stay with the release owner. The Compose overlay selects the exact verified external canary volume and preserves the base TLS mounts. Root must review its fully merged config before release.

Quiescence is an explicit operator assertion, not a claim that this tool can fence every application writer. Keep writers stopped until all clients use the chosen node and cutover has been accepted. Once new writes are admitted, returning to a pre-upgrade snapshot can lose those writes: it requires a fresh recovery decision. The rehearsal receipt does not authorize automatic post-cutover data loss or a reverse image swap.

Failures retain partial files and resources for inspection. Start a fresh run after repair; the tool deliberately does not retry ambiguous mutations, overwrite partial snapshots, delete collections, prune Docker data, or auto-clean volumes/PVCs. Storage/optimizer recovery may need more time than the bounded wait; do not edit a failed receipt into a passed one.

## Verification boundary

Implementation checks: rustfmt, scoped diff checks, JSON/YAML parse and independent official OCI hash/platform verification. Native tests were added for version skipping/downgrade, altered OCI bytes/header/platforms, unsafe endpoints/paths, missing tenant filters, unpinned inferred queries, cross-tenant/duplicate hits, score/ID/payload/negative regressions, corrupt snapshots, changed configuration content/receipt chains, shared storage, explicit mutation permission and fixture admission refusal.

Root must run, using `scripts/new/native-container.sh`, its one Cargo job and shared cache:

```sh
cargo test --locked -p ai-platform-knowledge-ops --lib qdrant_upgrade::tests
cargo check --locked -p ai-platform-knowledge-ops --bin platform-knowledge-ops
```

Also render/lint the separate Helm chart in disabled and enabled profiles and resolve the controlled Compose overlay with the existing project. Actual snapshot/restore, one-minor restart, real corpus parity, fresh-volume rollback, TLS/secret/resource/ownership acceptance and production cutover remain **not_run** until recorded by the execution owner.
