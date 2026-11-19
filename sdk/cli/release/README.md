# Client release source

This directory contains reviewed-policy inputs for offline candidate creation.
It does not contain a production private key, an invented trusted signer, signed
native binaries, an installer, or a published release. Empty `trusted-keys.json`
intentionally prevents a release launch until maintainers pin the actual Ed25519
release public key and rebuild the main package.

The five optional npm packages are `@agentforge/cli-{linux-x64,linux-arm64,
darwin-x64,darwin-arm64,win32-x64}`. Node 24 or newer is required. Each package
contains the composed native CLI and the portable `platformctl` vault helper,
their separate signed receipts, and all lock-pinned license/SBOM/source materials.
The independent launcher verifies both binaries against the main package's trust
pins and version; it never trusts a key supplied by an optional package.

On the matching release OS, sign both binaries with the actual release identity.
For Apple, sign with Developer ID and notarize/staple through the normal release
account; the staging gate requires codesign's Apple chain/pinned team requirement
and Gatekeeper assessment. For Windows, Authenticode status and the pinned signer
certificate thumbprint must match. For Linux, provide each binary's Sigstore
bundle; a SHA-256-pinned local cosign verifies the exact certificate identity and
OIDC issuer offline. Copy `os-signing-policy.example.json` outside the repository
and replace its example values with reviewed real identities and verifier hash.
No helper downloads tools or signs an artifact on the user's behalf.

```text
node sdk/cli/scripts/stage-native-release.mjs \
  --target darwin-arm64 --binary /release/codex \
  --platformctl /release/platformctl --materials /release/locked-materials \
  --output /release/new-cli-darwin-arm64 \
  --signing-key-file /private/release-key.pem --key-id reviewed-release-key \
  --os-signing-policy-file /private/os-signing-policy.json
```

Linux additionally requires `--binary-bundle /release/codex.bundle` and
`--platformctl-bundle /release/platformctl.bundle`. An alternate reviewed public
pin file may be passed through `--trusted-keys-file`; the same pins must be built
into the main package. The output directory must be external and new. All source,
material, architecture, OS-signature and signing-key validations finish before
output creation; replacement or deletion of an existing output is refused.

Homebrew and winget metadata can then be generated into another new directory:

```text
node sdk/cli/scripts/generate-distribution-manifests.mjs \
  /release/reviewed-assets.json /release/new-distribution-metadata
```

The input has `version`, `cli_tarball`, `native_archives` with all four Unix target
names, and `windows_msi`; each asset has an immutable HTTPS `url` and lowercase
`sha256`. The main npm tarball must contain built `dist/`, including its trust
pins. Native tarballs must have the staged target directory's contents at their
root. The Homebrew formula installs the dependency-free main package offline,
then copies the matching signed native resource. The winget candidate references
an independently built and Authenticode-signed user-scope x64 MSI, with Node LTS
as a dependency. This generator does not create or certify that MSI. Review the
real installer metadata and validate with the distribution tools before submitting.

`cli-client-candidates.yml` runs source/fixture checks on five hosted targets.
Fixture binaries and ephemeral test keys establish format and tamper rejection,
not release installation, notarization, TPM/Keychain availability, or an editor
session. Real install/launch on all targets, signed MSI creation, Homebrew/winget
validation and actual publication remain release acceptance steps.
