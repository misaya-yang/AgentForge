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
