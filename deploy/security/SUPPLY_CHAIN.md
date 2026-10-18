# Release supply-chain policy (U-PE-13)

This policy covers AgentForge release images, not database privilege exceptions or
third-party infrastructure approval. `database-bootstrap` remains a separately
invoked admin-only one-time job; it has no application/migration signing or DB
identity exception here. Both general CI and the supply-chain contract workflow
call `make database-identity-gate`; its implementation and Cargo dispatch are owned
by the integration writer.

`docker-publish.yml` retains its native amd64/arm64 builders, repositories, size
budgets, config labels, provenance and SBOMs. Each pushed platform digest must pass
Trivy critical scanning before its digest record is admitted. The merge validates
both records and attestation descriptors, stages a uniquely named pending index,
then scans that exact immutable index separately for each platform. Both scan
predicates are keylessly attested and the index is signed with cosign. The same
verifier used by deployment must pass before semver/SHA tag promotion. Temporary
pending tags/digest uploads can remain on failure; this workflow does not delete
registry content. No signatures, published images or release acceptance have been
created by implementing these sources.

`scan_release_image.sh` scrubs inherited `TRIVY_*` settings and specifies the tracked
config, full vulnerability severities, unfixed findings, remote immutable digest,
platform and an empty ignore file. Scanner errors stop before issuing a predicate.
The native report gate requires schema/image/platform identity, OS-package coverage,
the exact scanner version, valid severities, no critical findings (including
unfixed), and vulnerability DB metadata no older than 72 hours. No VEX waiver or
critical allowlist is supported in this first implementation. A missing/filtered
report, unknown severity, stale/future DB metadata or unknown scanner fails closed.

Deployment requires digest-pinned images from each selected service's approved
repository, a cosign signature and signed scan predicates for **both** native
platforms, under the exact current policy digest. Scan evidence expires after 24
hours, even when the signature remains valid; the owning release workflow must
refresh scans/signatures before redeploying an older release. This freshness window
is an explicit policy bound, not a claim that newly disclosed vulnerabilities are
re-scanned continuously during deployment.

Verification fixes the GitHub OIDC issuer and anchors the certificate identity to
this repository's `docker-publish.yml` at semantic version tags. Forks, branches,
other workflows and mutable tags are rejected. Cosign's transparency and certificate
checks stay enabled; there is no offline/insecure/unsigned escape flag. Inherited
`COSIGN_*`/`SIGSTORE_*` overrides are removed from verification commands. Its pinned
version is checked, and subprocess diagnostics and rendered environment values are
not printed. Verified public digest identities and policy hashes may be recorded.

`scripts/new/deploy.sh` runs this admission before stopping/starting applications
or invoking migrations. Rendered Compose configuration travels directly through a
pipe, never a tracked/logged file. Infrastructure-only scope retains its existing
separate vendor-image responsibility. `--build` does not waive release signatures;
unsigned source-built images belong in the existing development workflow, not a
production deployment. Runtime/Worker/SDK service repositories are constrained by
policy but the current five-image publisher does not yet publish those services;
they remain blocked until their controlled release path produces matching signed
artifacts. Existing source/image locks remain an additional required boundary.

Every external workflow action is pinned to a commit resolved from its official
repository; `official-action-refs.json` records exact refs and public tool source
commits. Trivy 0.76.0, cosign 3.1.3 and zizmor 1.31.1 were confirmed using official
Git refs/source, not invented tags. The SHA-pinned zizmor action audits workflows
without Advanced Security upload/paid features. Workflow permissions default to
contents-read; package write is isolated to publication jobs, and OIDC write to the
merge/sign job. Shared composite-action pins remain integration-owned.

## Integration and verification

The integration writer adds only this shared tools wiring (no new dependency):

```rust
mod supply_chain;
// in command dispatch:
"supply-chain" => supply_chain::run(&args)?,
```

Run serialized affected checks after that wiring:

```sh
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-tools --bin platform-tools supply_chain::tests
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-tools --test supply_chain_gate
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-tools --test publish_workflow
```

Tests include critical/unfixed and malformed scan rejection, DB/scan freshness,
wrong platform/subject/policy, strict DSSE decoding, identity constraints, real CLI
mock-verifier failures, missing-architecture attestations, mutable/unapproved image
rejection before invoking a verifier, source-pinned action contracts, and promotion
ordering. Mock cryptography proves control flow, not real signature acceptance.
Actual hosted execution, scanner DB fetch, Fulcio/Rekor/OIDC, signing, registry
referrers, both platform scans and deployment verification are still required.
No SLSA Build Level 3 claim follows from adding these gates. Full-plan cargo-deny,
licence/SBOM decisions, pnpm audit/lockfile lint, Scorecard and VEX governance are
separate unaccepted U-PE-13 work, not implied successes here.

The SDK npm job now uses GitHub OIDC with `id-token: write` and has no publish-token
fallback. Before an authorized publication, a package maintainer must configure
npm's trusted publisher for `misaya-yang/AgentForge`, workflow `publish-sdk.yml`,
and ensure the package repository URL matches that repository. This external
registry configuration has not been inspected or changed. The local preflight
rejects npm older than 11.5.1, mismatched repository metadata, wrong workflow or
non-SDK-tag context; npm verifies the actual trusted-publisher configuration.
Release Node caches are explicitly disabled using setup-node v6's documented
`package-manager-cache: false`; Maven release cache is omitted. pnpm installation
uses an official commit-pinned action and keeps version 12.11.2/frozen installs.

Local steps and reusable workflows use GitHub's standard `./.github/...` references.
The running checkout supplies local actions; reusable workflows resolve with the calling
repository commit. External actions remain full commit-SHA pinned. The earlier `$/` workaround
and instructions to preserve that spelling are retired. The integration owner must align the
shared workflow-reference linter and tests with this source correction; checked-in workflows
must not be reverted to satisfy an obsolete local parser.

## Remaining release artifact gaps now covered in source

`platform-tools delivery-policy` layers the existing signature/critical-scanning gate with
signed current-policy license predicates, signed SPDX documents bound by canonical content
hash, and signed native release provenance for both platform inputs. `docker-publish.yml`
generates and verifies those artifacts before release tags are assigned. No cargo-deny license
allowlist, exception, VEX waiver, critical ceiling or image byte ceiling is relaxed. GPL utilities
and the documented denied Rust/font/data terms still block their actual distributions until
properly resolved by their owners. Root LICENSE/NOTICE are now included in native images; these
two files do not replace complete distribution-specific notices/source obligations.

OpenSSF Scorecard runs read-only without publishing results or requiring SARIF/Advanced Security.
Its v2.5.0 action commit `fa7117db83c52d787ae5a89f350999701505d78e` was read from the peeled official
tag and its checked-out action metadata; the action's own container uses its upstream versioned
image. This pins action source, and does not claim that every transitive remote tool image has
become immutable. The root action inventory must record this newly used official commit.

See [GitOps admission, canary and Compose include contracts](../gitops/README.md). The candidate
ApplicationSet has an empty element list and no automatic sync. A source-only delivery guard is
not deployment acceptance, SLSA Build Level 3, a license decision, or a real rollback drill.
