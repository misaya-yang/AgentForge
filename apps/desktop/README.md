# Desktop shell candidate (U-CL-01 / P-ADR-8)

Source implementation of the [Tauri shell recommendation](../../docs/architecture/ADR-013-desktop-console-tauri-shell.md).
The existing egui application remains available. This candidate has no measured
GA/parity acceptance, signed installer or enabled update distribution.

`src-tauri` is an isolated Cargo package and does not change the native workspace.
Its new Tauri dependencies require owner resolution, exact lockfile review and
an explicitly authorized native client build. Do not run a host server Cargo build
or download the Tauri toolchain to exercise these source files. No native build was
run for this task. `package.json` has no dependency installation/build side effects.

## Running the available checks

The std-only Node tools need the repository's Node runtime and no npm install:

```sh
AGENTFORGE_DESKTOP_TEST_SCRATCH=/absolute/external/scratch \
  node --test apps/desktop/tests/*.test.mjs
```

The scratch directory must exist. Tests generate **synthetic** Ed25519 keys and
Console fixture files only, and retain them in external scratch for inspection.
They never read a real OS vault, connect to a Gateway, invoke a publisher tool,
start the application or publish an artifact. Rust unit tests cover trusted-window
policy, exact origins, signed policy tampering/expiry/rollback and rollout decisions;
they remain unrun until the client owner performs the native build.

## Candidate assembly and launch contract

1. The Web owner produces its ordinary reviewed Console build. Stage it without
   modification: `node apps/desktop/scripts/assets.mjs /absolute/web-dist
   /absolute/new-staged-assets`. Existing output is refused. Stage outside source;
   record `asset-receipt.json` together with the Console build/version receipt.
2. The client owner resolves/pins a supported patched Tauri 2 version and reviews
   a standalone lock. Configure Tauri's `build.frontendDist` to the staged absolute
   directory through an **external** configuration overlay. The checked-in
   `../dist` default deliberately contains no placeholder compiled Console.
3. Use the owning native-client build/release workflow and external target/cache.
   Generate an SBOM and review WebView2/WebKitGTK/macOS system WebView requirements.
   No shared native workspace, CI entrypoint or dependency lock is modified here.
4. Optional `AGENTFORGE_LOCAL_NODE_CONFIG=/absolute/operator.json` at native process
   startup selects the existing secret-free, operator-reviewed Local Node config.
   The companion cannot pick arbitrary paths through IPC. It does not start the
   headless Local Node or change its state directory/grants/key policy.
5. Set an HTTPS Gateway origin in the companion. A native dialog displays the exact
   destination. The online Console opens at that origin with **no native IPC**;
   Console authentication stays in its existing cookie/CSRF/Trusted Types path.
   Native device sign-in uses the existing OS vault and currently registered
   `agentforge-cli` ID. It shares that origin's native session with platformctl.
   Console cookies and native sessions have separate sign-out controls.
6. Use the offline Console button to inspect the packaged build. Its capability
   and CSP forbid a Gateway network bridge or queued mutations. Cached private
   Gateway data is not copied into the package. A failed reachability probe is
   explicitly `offline_or_unreachable`, not proof of revoked credentials.

Gateway redirects to a different origin, including an external IdP, are denied in
the Console window. Device approval can be completed in the user's system browser
at the displayed verification URI; there is no arbitrary native opener command.
If a future Console OIDC flow requires cross-origin top-level navigation, the Web
and identity owners must supply a reviewed navigation contract without granting
that remote origin IPC. This is a release integration gate, not an allow-all toggle.

## Release and updater contract

The checked-in `release-policy.json` has `enabled:false` and the Tauri bundle has
`active:false`. Absent signing credentials/public keys cannot produce a promoted
release or enable updater network requests. There is no publishing command.

Supply an external six-field **public** policy via
`AGENTFORGE_DESKTOP_RELEASE_POLICY` for a separately authorized build. Fields are
`enabled`, `channel` (`stable`/`beta`), `policy_url`, `download_origin`,
`policy_public_key` (Base64 raw Ed25519 public key) and `updater_public_key` (official
Tauri updater public key). Build-time validation refuses unknown fields, links,
oversized files, missing keys and non-HTTPS/cross-origin update policy URLs.
Never put a private key, device token or publisher credential in this JSON.

A signed policy's schema is represented by `src-tauri/src/updates.rs::Policy` and
`scripts/release.mjs`. For each target, include a standard **static Tauri manifest**
with `version` and `platforms[target].url/signature`. Targets are
`darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`, `linux-aarch64`, `windows-x86_64`.
The matching artifact must already carry its official updater signature. Bind its
exact byte size in `artifact_size`. Sign each target policy with a monotonic positive
`sequence`, TTL <= seven days, matching channel/target/version and
`rollout_basis_points` in 0..10000. A rollout expansion needs a new sequence and
new signature, including if version/artifact bytes stay unchanged.

```sh
node apps/desktop/scripts/release.mjs /absolute/policy.json \
  /absolute/public-release-config.json /absolute/private-policy-ed25519.pem \
  /absolute/new-signed-envelope.json
```

This offline command signs only the policy and self-verifies exact bytes; it does
not create Tauri artifact signatures, forge publisher signatures or upload files.
The public configuration must pin the private key's corresponding public key.
Private key files must be bounded regular files, not links, and private on Unix.
For an authorized publisher verification run, `publisher-gate.mjs ARTIFACT` requires
`TAURI_SIGNING_PRIVATE_KEY_PATH`, `AGENTFORGE_DESKTOP_RELEASE_POLICY` and
`AGENTFORGE_DESKTOP_PUBLISHER_ID` (Apple Team ID or Windows certificate thumbprint).
It verifies existing publisher signatures/notarization only; it never creates them.
Linux additionally requires explicit `AGENTFORGE_DESKTOP_LINUX_PUBLISHER_PUBLIC_KEY_FILE`
and `AGENTFORGE_DESKTOP_LINUX_PUBLISHER_SIGNATURE_FILE`; the publisher ID must equal
the SHA-256 of the exact public key file, and installed `minisign` verifies its
detached signature. Missing keys/tools fail closed. Tests do not execute publisher
verification tools.

Policy checks bind version/target/origin, sequence and payload hash across restarts,
expiry, staged cohort, full manifest equality and official artifact signature.
Native install requires a confirmation and rechecks expiry. A successful install
reports restart required; no automatic publication, recurring update poll,
background download, forced restart or mutation replay occurs.

## Owning handoff and acceptance

- Root/client owner: reviewed Tauri dependency pins and lock; controlled native
  client workflow; package identity/publisher ownership; external staged asset
  configuration; public release-policy provisioning; supported install formats,
  notices/SBOM, signed updater artifacts and independent signature verification.
- Gateway/Web owners: exact Console release identity alignment; retain HttpOnly
  cookie, CSRF and Trusted Types enforcement; review cross-origin IdP navigation
  requirements before distribution. No cookie-to-native-token or generic IPC
  request bridge is requested.
- Client/Local Node owners: optional separate desktop device-client registration;
  OS-vault prompts/access policy; configured keychain/hardware pairing contract;
  Windows filesystem grant support and headless-service lifecycle remain theirs.
- Test owner: run the per-target criteria in the ADR and populate
  `acceptance-matrix.json` with exact artifacts/evidence. Keep egui until complete
  authenticated parity, native accessibility/security and release measurements.

Observed L0 for this source: **11 Node tests passed** (including synthetic signature
verification, archive safety, unsigned-gate denial and hostile text rendering);
Rust parsed/formatted;
JSON/TOML/entitlement structure checked. Native compilation, installer size/memory,
startup, screen readers, real IPC, OS-vault/pairing, authenticated Console parity,
offline WebView rendering, updates, publisher signing/notarization and actual
multi-OS installation are **not run**. No GA or accepted-ADR claim follows.

For a separately authorized signed-candidate assembly, run `scripts/assemble.mjs`
with the existing signed update artifact, official Tauri `.sig` file, a new output
directory and external verification scratch directory. It first runs the native
publisher gate, then installed `minisign` against the decoded official updater
public key/signature, copies exact verified bytes and creates a SHA-256 receipt.
It refuses missing tools/credentials or existing output, does not build/sign/upload,
and still marks GA and parity unaccepted. macOS `.app.tar.gz` artifacts are boundedly
extracted into a new private verification directory, with exactly one `.app` root,
no traversal, duplicate names, links or special entries, before independent app
signature/notary checks. A package containing symlinks needs an explicitly reviewed
safe-format expansion; the candidate rejects it rather than weakening extraction.
The updater archive itself is independently verified with its official signature.
Verified archive bytes are copied from private staging and rehashed, closing the
input-file replacement window. The system Python standard library performs explicit regular-file extraction;
no archive ownership, links or special permission bits are restored.
Windows verifies the existing signed installer; Linux uses its pinned publisher
signature. None of these checks turn a candidate into a GA/parity acceptance.
