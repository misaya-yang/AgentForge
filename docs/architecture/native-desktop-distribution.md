# Native desktop distribution

## Scope and present evidence

The native Rust desktop and `platformctl` are thin Gateway clients. The desktop
uses `eframe` with the native OpenGL renderer and X11 support on Linux. It does
not require a webview, embed the Gateway/Agent runtime or install a service.
Public Gateway authentication and server-side authorization remain unchanged.

`deploy/release/desktop-targets.json` is the desktop candidate target authority.
The separate CLI authority remains `deploy/release/platformctl-targets.json`.
They currently enumerate the same five architecture/OS combinations. Having a
matrix is not evidence that every target compiles, launches or is supported.

The existing Linux x64 development binary has been linked and its sign-in window
rendered in the cloud display. That limited launch observation does not establish
authenticated UI acceptance, release-profile packaging or Windows/macOS readiness.
The active native-enterprise-upgrade program records verification status.

| Candidate | Native build runner | Archive layout | Declared build baseline |
| --- | --- | --- | --- |
| Linux x64 | ubuntu-24.04 | tar.gz, executable plus documents and optional launcher template | Ubuntu 24.04 / glibc 2.39 |
| Linux arm64 | ubuntu-24.04-arm | tar.gz, executable plus documents and optional launcher template | Ubuntu 24.04 / glibc 2.39 |
| macOS Intel | macos-15-intel | tar.gz containing AI Platform.app | macOS deployment target 14.0 |
| macOS Apple Silicon | macos-14 | tar.gz containing AI Platform.app | macOS deployment target 14.0 |
| Windows x64 | windows-2022 | Portable ZIP containing ai-platform-desktop.exe | MSVC; static C runtime requested |

Runner labels follow the [GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
Native rendering follows the [eframe 0.33 contract](https://docs.rs/eframe/0.33.0/eframe/).
These are build baselines, not a promise that older operating systems work or
that every machine matching the baseline is accepted. macOS Intel is built on
macOS 15; separate macOS 14 execution is required before claiming that minimum.
Windows desktop editions require their own clean-machine acceptance; the build
runner is Windows Server 2022. Windows arm64, Linux musl, native Wayland without
XWayland, universal macOS binaries and automatic updates are not covered.

## Candidate CI

`.github/workflows/native-desktop.yml` is an independent candidate workflow:

- Relevant pull requests to `main`/`dev` and explicit manual dispatch select all
  five native targets. This workflow has read-only repository permissions and
  never creates a release, signs an artifact or publishes a package.
- Each runner builds only `ai-platform-desktop`, using the workspace lockfile,
  Rust 1.95.0 and one Cargo job. Linux jobs install the declared native window
  build dependencies. Output is placed under the runner temporary directory.
- `package_desktop.py --build-env --target NAME` refuses untracked/dirty source
  inputs and emits the commit, material fingerprint and target to embed. It also
  sets the macOS deployment target or Windows static C-runtime option. Normal
  developer builds may omit these variables and report `unrecorded`; such
  binaries cannot pass this candidate packager.
- `--package-info` runs before GUI, async-runtime or client initialization. It
  prints only package/version/OS/architecture and compiled-in source metadata.
  `--help` and `--version` also work offline. Unknown arguments exit 2 without
  repeating arbitrary input. Starting with no arguments opens the usual UI.
- Packaging checks native architecture, exact compiled identity and offline help,
  then generates the platform archive, extracts its exact member set, verifies
  bytes/modes and repeats native identity/help from the extracted location.
- The aggregate job fails when a target fails or is skipped. A successful run is
  candidate build/process evidence, never an authenticated or graphical pass.
  The aggregate is advisory to branch protection as currently wired. Do not
  require this path-filtered workflow globally: unrelated PRs would wait for a
  check that never starts. To make it required, first add an always-triggered
  wrapper with job-level change selection and explicit skip handling, and add
  merge-queue event handling if the repository uses a merge queue. This change
  does not configure branch protection. See [GitHub's required-check guidance](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

The existing SDK-tag workflow continues to build `platformctl`. Its other jobs
publish Python/npm packages. Do not create an SDK tag merely to test desktop
candidates; manual dispatch of the independent desktop workflow avoids those
publication side effects. No workflow run is triggered by the packaging tools.

## Local candidate assembly

Use a native machine matching the chosen target and a clean, committed checkout.
Build only when authorized under the active program's compilation rules. The
following Bash example targets Linux x64; use the target manifest and a matching
native machine for another row. Python 3.11+ is required. The packaging commands
themselves do not build, download dependencies, access credentials or launch UI.

```bash
set -euo pipefail
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_TARGET_DIR=/absolute/out/desktop-target
build_env="$(python scripts/harness/package_desktop.py --build-env --target linux-x64)"
while IFS='=' read -r key value; do export "$key=$value"; done <<< "$build_env"
cargo build --locked --release --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-desktop --bin ai-platform-desktop --target x86_64-unknown-linux-gnu
python scripts/harness/package_desktop.py --target linux-x64 \
  --binary "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/ai-platform-desktop" \
  --output /absolute/release-candidates/desktop-linux-x64
```

`--matrix` needs no binary and emits the canonical CI matrix. Packaging output
must be outside the checkout. Existing artifact names are never overwritten.
The workflow shows equivalent Bash commands on macOS and Windows runners.
Use the exact manifest binary filename, including `.exe` on Windows.

## Candidate contents and provenance

Every workflow artifact contains a versioned tar.gz or ZIP, its SHA-256 companion
and a JSON receipt. The archive contains the executable, receipt, launch notes,
capability/distribution docs and the repository license verbatim. macOS also
contains `Info.plist` and copies the receipt/license into the app's Resources.
Its bundle identifier is `org.ai-platform.desktop`; confirm publisher ownership
and the final identifier before signing or enabling updates. Numeric bundle
versions derive from the existing native workspace version. No version/license
is changed by packaging.

The JSON receipt records source commit, material fingerprint and each material
hash, binary path/hash, target, declared build baseline and the offline checks.
The baseline is requested target configuration, not an observed host/runtime
attestation; a local invocation does not prove the machine or compiler flags
matched the workflow. Retain the matching native workflow logs for builder
identity, and verify minimum-OS/dependency behavior separately. Fingerprints
cover client/desktop Rust sources, workspace/crate manifests and lockfile,
packaging scripts, target/workflow definitions and distribution documents.
They do not prove compiler authenticity or replace an SBOM, dependency license
inventory, reproducible-build proof or cryptographic build attestation.
Build metadata is a consistency check, not a publisher trust root.

Receipts explicitly record GUI/authenticated checks as `not_run`, notarization as
`not_performed`, publisher signature as `not_provided` and public-release
eligibility as false. A linker-generated ad-hoc macOS signature is not a
publisher signature; the packager neither creates nor assesses one.
A receipt is emitted only after extracted native smoke and final source/binary
consistency checks. Packaging rejects stale/unidentified builds and dirty inputs.

## Launch, prerequisites and removal

1. Verify the `.sha256` against the complete archive before extracting. A hash
   from the same untrusted download is not proof of publisher authenticity. Use
   a separately trusted release channel for future signed public artifacts.
2. Extract into a trusted, user-owned directory. Keep the documents and receipt
   with the executable; do not run directly inside Windows' ZIP preview.
3. Linux: run `./ai-platform-desktop` in a graphical X11/XWayland session with an
   OpenGL-capable driver. Ubuntu 24.04 supplies glibc 2.39. Build prerequisites
   are listed in the workflow; clean-machine acceptance must inventory runtime
   shared-library requirements. A Wayland-only session needs XWayland because
   this crate's current eframe feature set enables X11 only. The supplied
   `.desktop` file is an optional template, not an installed launcher; its Exec
   and TryExec names require the executable to be deliberately placed on PATH.
4. macOS: open `AI Platform.app` only in an approved candidate environment. If
   Gatekeeper blocks the unsigned/unnotarized candidate, stop and obtain an
   approved signed build or operator review. Do not remove quarantine attributes
   or disable Gatekeeper as a distribution step.
5. Windows: open the extracted `ai-platform-desktop.exe` in an approved candidate
   environment. A console window may accompany this portable binary. The
   workflow requests static CRT linking, but clean-machine DLL and renderer
   validation is still required. Do not disable SmartScreen or security controls.
6. Authenticate only in the normal Gateway sign-in window. `--package-info`
   deliberately accepts no origin/token/password. No credentials are added to
   this archive, an installer, or a command line by the packaging tool.
7. Close the app and remove the extracted directory to remove the candidate.
   It has no updater, service, startup entry, registry installer or automatic
   PATH/launcher change. If the operator installed the optional Linux launcher
   or copied an executable to PATH, remove those explicitly. Local artifact
   downloads chosen by the user and server-side data are separate and retained.

## Mandatory public-distribution gates

Candidate generation is intentionally possible before publisher credentials are
configured. Public promotion must be separately authorized and remains blocked
until all of these are established on the exact candidate source and bytes:

- Successful native builds/extraction/process smoke for every advertised target,
  required dependency inventory and clean-machine launch at advertised minimum
  OS versions. Linux x64 UI launch alone cannot stand in for other targets.
- Graphical and authenticated journeys on each supported OS: login/password
  change, streaming/cancel/reconnect, management/task/schedule views, pagination,
  artifact download, sign-out/expired sessions, owner/tenant denials and no
  unintended replay of uncertain mutations. Include display scaling, keyboard
  accessibility and X11/XWayland behavior where applicable.
- Owner reconciliation of root MIT LICENSE versus native workspace Apache-2.0
  metadata, plus complete third-party license/notice and SBOM review. Neither
  archive generation nor including the MIT file resolves that mismatch.
- An approved macOS publisher identity, reviewed entitlements, Developer ID
  signing, notarization and stapling with independent verification of the final
  app/container. Credentials are never part of this workflow.
- An approved Windows publisher identity and Authenticode signing with trusted
  timestamping and independent signature verification of the final executable.
- Reviewed publisher authenticity/provenance for Linux and all download metadata,
  and an explicitly authorized distribution destination.

Any signing/notarization changes artifact bytes. Produce new final hashes and
receipts for those bytes under a separately reviewed promotion procedure; do not
reuse the unsigned candidate checksum or edit its historical checks into a pass.
This implementation deliberately has no signing/publishing bypass flag.
