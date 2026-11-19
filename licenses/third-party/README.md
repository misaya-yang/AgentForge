# Third-party legal materials

The files in `materials/` are verbatim upstream legal files, named by SHA-256.
Their original copyrights, grants, restrictions and notices remain in force.
They are not AgentForge-owned Apache-2.0 contributions.

[The source index](../../docs/security/third-party-license-sources.json) binds each
supplement to the exact Cargo/npm component, declared terms and package-manifest
hash. Cargo VCS revisions, npm registry `gitHead` values, or a byte-for-byte match
to a published source file bind it to an immutable upstream commit. Each entry
records the original repository, commit, path, URL and text hash. Ancestor license
files are used for packages published from a monorepo without its root licenses.
No dependency source, executable, model or image is downloaded by generation.

For the explicit MPL-2.0 source-header reference and CC0-1.0 original Cargo
declaration, the index also binds the original published grant file and its
hash to the verbatim matching standard text from the pinned SPDX license-list
release. The generator rechecks that grant; it does not create a copyright
notice. This narrow path does not accept an MIT copyright-holder template or
an inferred/unknown grant, and it grants no cargo-deny exception.

The protobuf license accompanies `protoc-bin-vendored`'s wrapper license because
the wrapper also contains upstream protoc binaries and `.proto` files. Its exact
upstream version is recorded in the wrapper's committed `version.txt`. This
source dependency inventory is not a binary dependency SBOM or permission to omit
other distribution notices.

Run the Rust notice generator described in
[licensing.md](../../docs/security/licensing.md) against the actual current Cargo
metadata and installed JavaScript sources. Include its `THIRD_PARTY_NOTICES.txt`,
`inventory.json`, and the root `LICENSE`/`NOTICE` in the applicable distribution.
The output remains an inventory, never a license approval. `--require-zero`
refuses a complete bundle when any declaration/text is missing; cargo-deny and
any source, font, certificate or other fulfillment obligations remain separate
release gates. Missing upstream material must remain a blocker, not a substitute
license template or a deleted inventory row.
