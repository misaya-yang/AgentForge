# AI Gateway deployment footprint observation

Observation: 2026-10-12T06:24:23.422754+00:00 to 2026-10-12T06:24:51.598606+00:00.

## Result

**The historical 13 GB compiler target is not the installed product requirement. A production minimum has not yet been measured.** The current evidence is a set of host-built executable and earlier packaging-snapshot sizes. These are not a complete, same-version release and must not be added into a product total.

No source or Dockerfile changes, builds, downloads, installs, deletions, deployments, service starts or secret reads were performed. Files were staged only beside this report. The root-owned Runtime link continued independently.

## Measured executable payloads

Bytes are exact logical file sizes. MiB means 1,048,576 bytes. Every executable listed has two hardlinks; the second path in Cargo `debug/deps` is the same inode, not a second payload.

| Artifact | Bytes | MiB | Evidence boundary |
| --- | ---: | ---: | --- |
| ai-platform-capability-worker | 176,151,328 | 167.99 | Current host Worker candidate; debug-zero, unstripped |
| ai-platform-capability-worker-health | 492,072 | 0.47 | Current host Worker candidate; debug-zero, unstripped |
| ai-platform-plugin-packs | 13,521,632 | 12.90 | Current host Worker candidate; debug-zero, unstripped |
| ai-platform-python-supervisor | 9,792,584 | 9.34 | Current host Worker candidate; debug-zero, unstripped |
| **worker_candidate subtotal** | **199,957,616** | **190.69** | Executables only |
| gateway | 178,609,728 | 170.34 | Historical native26; current native30 not rebuilt |
| knowledge-service | 87,641,448 | 83.58 | Historical native26; current native30 not rebuilt |
| platformctl | 16,370,608 | 15.61 | Historical native26; current native30 not rebuilt |
| ai-platform-desktop | 71,062,328 | 67.77 | Historical native26; current native30 not rebuilt |
| **native26_historical subtotal** | **353,684,112** | **337.30** | Executables only |

- Runtime: successful build receipt was absent at measurement time; size intentionally left null. No incomplete linker file was counted.
- Worker build receipt exited 0 at 2026-10-12 06:18:05 UTC. The 12 offline Worker probes are separate bounded behavior evidence, not full product acceptance.
- Native26 hashes match the preserved ledger artifacts. The subsequent native30 all-target checks and tests do not refresh these old executables.
- Docker uses `dev-small` with `strip = "symbols"`; the measured host build uses the ordinary dev profile with debug information disabled and retains `.symtab`/`.strtab`. Its bytes are not an already-produced stripped release size. No stripping was performed or projected.

## Existing packaging payload observations

| Payload | Logical file bytes | MiB | Qualification |
| --- | ---: | ---: | --- |
| Frontend static dist | 6,698,813 | 6.39 | Earlier toolchain snapshot; 13 selected source paths now differ; excludes Nginx image |
| Optional SDK amd64 runtime venv | 312,199,896 | 297.74 | Frozen production dependency set plus earlier local wheel; 6 worker source files differ |
| Optional SDK arm64 dependencies | 299,640,664 | 285.76 | Alternate architecture; local worker wheel not installed; no arm64 execution |
| SDK build-tools venv | 1,103,651 | 1.05 | Build-only; not copied into SDK final image |
| Gateway acceptance fixture venv | 17,178,838 | 16.38 | Limited test environment; NOT Gateway/Knowledge production dependencies |

The amd64 Claude SDK bundles an actual CLI executable of 252,755,128 bytes (241.05 MiB). This is part of the SDK runtime dependency payload, not a compiler cache. Removing it would affect Claude execution. The venv has interpreter symlinks to this machine; the Python interpreter, OS libraries and final image are outside its measured file bytes.

`pyproject.toml` and `uv.lock` in the measured SDK snapshot still match the current project, but six local Python source files do not. Therefore the dependency environment is useful evidence while the local worker wheel is stale. Frontend source freshness was compared across 434 selected files and SDK across 13; this limited comparison is not full build attestation.

## Development storage, explicitly excluded from installation size

| Directory | Unique logical regular-file bytes | Allocated bytes, including directories | Allocated GiB |
| --- | ---: | ---: | ---: |
| Native Cargo target, after authorized prior cleanup | 7,215,295,991 | 7,273,308,160 | 6.774 |
| Worker/Runtime Cargo target, active build | 5,885,721,170 | 5,951,877,120 | 5.543 |
| Cargo home, registry/git/tools | 2,699,689,202 | 3,066,654,720 | 2.856 |
| Rust toolchains | 1,653,806,422 | 1,655,177,216 | 1.542 |
| Frontend node_modules | 342,137,133 | 479,424,512 | 0.446 |

Target directories contain their executable payloads as well as intermediate objects, metadata, test binaries and caches. Do not add target sizes to their binary subtotals. The native30 target path is a symlink to the same native target and must not be counted again. Unique inode counting removes 359 duplicate regular-file paths in the native target and 130 in the Worker/Runtime target at this observation. Equal-content copies on different inodes remain separately allocated. Runtime compilation was active, so target/free-space numbers are timestamped observations rather than a frozen build peak.

## What the deployment actually carries

- Base Compose still selects Python Gateway and Knowledge images. Its native Gateway/Knowledge replacements are not selected merely because host binaries exist. Keep the existing deploy profile distinct from the intended native product candidate.
- Gateway final image copies `/opt/venv`, `src`, `database`, `config` and the capability catalog into a Python base with curl/libgl/libglib/libgomp. The installed wheel may duplicate source files also copied under `/app`; no image measurement exists to quantify this.
- Knowledge final image copies `/opt/venv` and adds Poppler/curl to a Python base. API and Knowledge worker use the same image and `kb-data`; their shared layers/volume must not be counted twice as independent downloads.
- Frontend copies only `dist` plus configuration into Nginx. Node, pnpm and `node_modules` stay in its builder stage.
- Worker copies four executables into Python 3.11 slim and installs CJK fonts. Runtime copies its executable/entrypoint/notices into **`rust:1.95.0-bookworm`**, which retains that base and its toolchain. Cargo target cache mounts are not copied from either builder.
- Optional SDK Worker carries `/opt/venv` in Python 3.12 slim; its build venv and uv builder are not copied. It has an independently selected `sdk-agents` profile.
- Postgres, Redis, Qdrant, migration runner and optional Tempo contribute image and fresh-state bytes. Persisted uploads, vectors, indexes, run history, memory, workspaces, plugins, logs and SDK state grow with usage. These have no measured fresh or retention-bounded total here.

## Runtime dependencies and capability preservation

**No base-image slimming patch is proposed in this report.** The historical August plan flags the Rust base but its predicted 50–100 MB outcome is not current evidence and is not reused.

1. Worker ELF NEEDED entries: libssl.so.3, libcrypto.so.3, libgcc_s.so.1, libm.so.6, libc.so.6 and ld-linux-x86-64.so.2. Its helper binaries require the GNU C/runtime loader subset. Native26 executables require libgcc/libm/libc/loader; desktop also has graphics libraries loaded at runtime, so ELF NEEDED alone is incomplete.
2. The measured host Worker and native Gateway have an ELF `GLIBC_2.39` version need with `Flags: none`. The associated pidfd symbols are weak, but the version-need entry is not marked weak. These host artifacts are not accepted as drop-in Bookworm image binaries; build and execute against the declared target base before claiming ABI support.
3. Worker source requires `/usr/local/bin/python3`, `/usr/bin/unshare`, the Python supervisor, `/bin/sh`, `mount` and `/usr/bin/prlimit` for its existing isolated Python paths, plus permitted kernel namespace behavior. CJK font paths are explicit in its final image. A small image that loses any of these cannot pass full capability acceptance.
4. Runtime entrypoint requires `/bin/sh`, mkdir and find; upstream Git operations can invoke git, and the tool policy preserves native command approval/sandbox behavior. No direct Cargo/rustc startup requirement was found in the Runtime overlay. However arbitrary coding commands can require compilers and other tools. Absence of a direct startup call does not prove that removing the Rust toolchain preserves enterprise functionality.
5. Current Runtime initializes `EnvironmentManager::without_environments`; upstream implements that as no configured execution environments. The exact execution routing and supported coding-tool environment must be established by live acceptance. Do not treat the mere presence of tools in the base as proof those tools are usable, or silently change routing to justify a smaller image.
6. Any later packaging-only change should first freeze the supported tool inventory and test each affected capability in the final image. Preserve shell/git/compiler tools when the supported deployment depends on them. Keep this work outside the current linking/source-identity operation.

## Smallest durable acceptance gate and remaining work

`gate-proposal.json` describes a fail-closed gate. It proposes no arbitrary GB limit and does not claim implementation or acceptance.

1. When the root-owned Runtime link completes, rerun the read-only measurement script against its successful receipt. Refresh native, frontend and SDK local artifacts from the final accepted source under the root’s existing build plan; record profile, architecture, hashes and source identity.
2. On the designated supported image-build/acceptance environment, record exact service/profile selection and immutable image digests. Measure compressed transfer separately from unpacked bytes; deduplicate shared image layers and shared service images. Verify tools, shared-library ABI, Python, fonts and entrypoints from the actual images.
3. Use an authorized fresh isolated deployment, without Cargo targets/source checkout/dev node_modules. Record filesystem use before pull, after pull, after migration/health-ready startup, and peak across the representative UI/provider/Worker/Knowledge/SDK journeys. Measure per-volume empty/fresh state and controlled workload growth separately.
4. Add declared KB/user-data retention, log limits, backups, simultaneous old/new image upgrade/rollback space and explicit safety headroom. Report the smallest **tested allocation for that specified profile and workload**, not a universal unlimited-data minimum. No number is defensible until these inputs exist.

## Reproduction and evidence

Run `PYTHONDONTWRITEBYTECODE=1 python measure.py > measurement-NEW.json` from this staging directory. The script only reads files and invokes readelf; its stdout redirection creates the report. Never overwrite a previous observation when retaining a time series.

- `measurement.json`: exact sizes, inode/hardlink identities, executable SHA256, ELF dependencies/versions, selected source differences, input hashes and explicit unknowns.
- `measure.py`: inode-aware measurement collector; no application execution/build/deploy path.
- `selftest.py` and `selftest.json`: 6/6 checks passed for hardlink deduplication, separate path sum, symlink handling, missing-path status and hashing. Test fixtures remain staged; no cleanup was performed.
- Source anchors: `deploy/agent-runtime-source/Dockerfile.runtime:79-108`, `Dockerfile.capability-worker:31-42,112-125`, `web/Dockerfile:56-71`, current base Compose service/images and volumes; `ai-platform-agent-runtime/src/main.rs:134`, `src/tool_policy.rs:58-66`; `ai-platform-capability-worker/src/python_code_execution.rs:225,803-822` and `src/python_supervisor.rs:39-52`.
- Existing build evidence: `runtime-worker-build-next/worker-binaries.json`; native `enterprise-source-sha256-20261012-26.json`; `toolchain-modernization-next/verification/results.json` and `integration-receipt.json`.

**Current acceptance state: partial measurement; release/install/fresh-run disk gate remains open.**
