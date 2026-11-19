# Agent Runtime image dependencies (U-RT-08)

The final `Dockerfile.runtime` stage uses `debian:trixie-slim` (Debian 13).
Rust 1.95.0 Bookworm remains exclusively in the builder, source-evidence and test
stages. The compiled executable, public CA trust bundle and license/notice files
are the only builder artifacts copied into the final image; no toolchain,
compiler, headers, build cache or upstream source is copied. This is implemented
source; no image build, scan, measured size reduction or live acceptance has run
for this change yet. Worker/Python is unchanged.

## Inventory and purpose

Inventory was checked against the pinned `092d3acd6bec` upstream source and
platform overlay, not inferred from the Rust base image's incidental packages.
Upstream paths below are relative to the composed `codex-rs/` tree.

| Explicit final-stage package | Caller / retained purpose |
| --- | --- |
| `bash`, base `dash`/`sh` | `core/src/shell.rs`, `shell-command/src/shell_detect.rs`; native shell/unified-exec preserve their existing policy and approval checks. The platform entrypoint is POSIX sh. |
| `git`, `openssh-client` | `git-utils/src/info.rs`, `operations.rs`, `worktree.rs`, and `app-server/src/request_processors/git_processor.rs`; metadata, repository operations and SSH transport. No credentials are supplied by the image. |
| `ripgrep` | Native command search and `linux-sandbox/src/bwrap.rs` pre-sandbox executable resolution. |
| `bubblewrap`, `util-linux` | `linux-sandbox/src/launcher.rs` resolves system bwrap before a bundled fallback; preserve Linux sandbox/process utilities (`unshare`, `setsid`). Presence/version is checked here; actual namespace/seccomp behavior needs the original sandbox gate. |
| `coreutils`, `findutils`, `grep`, `sed`, `gawk`, `diffutils`, `patch` | Entry point's `mkdir/find`, Kernel shell execution and repository/search/patch workflows. Includes `cat`, `stat`, `timeout`, `env`, `rm`, `install`, `mktemp`. |
| `tar`, `gzip`, `bzip2`, `xz-utils`, `unzip` | Archive extraction/compression in existing shell/file workflows. Kept explicitly rather than dropping these with the buildpack base. |
| `procps` | Process inspection/cancellation diagnostics (`ps`), retained for execution tooling. |
| `curl`, `openssl`, `ca-certificates` | Existing Compose health, recovery harness readiness, TLS and HTTPS Git transport. Both health helpers invoke `curl` inside the Runtime. |
| `passwd` | Image assembly creates an explicit uid **and gid** 10001; runtime keeps its existing nonroot identity. |
| `libgcc-s1`, `libstdc++6`, `libssl3t64`, `zlib1g`, `libzstd1` | Explicit non-development ELF/TLS/compression compatibility set. Apt resolves runtime dependencies for curl/git/bwrap. Actual composed-binary dependencies are verified with `ldd` at image build and again offline; missing SONAMEs fail rather than guessing that source inventory is sufficient. |

The Bookworm executable's older glibc symbols are expected to work with Debian
13's newer glibc; this remains a build/runtime check, not an asserted result.
The OpenSSL runtime retains the `.so.3` ABI through Debian's `libssl3t64` package.
If `ldd` finds another required runtime library, add that runtime package and
rerun the gates; do not copy the old distribution's entire library directories.

Per-image installed package/version inventory and actual ELF dependency listing
are retained under `/opt/ai-platform-agent-runtime/runtime-packages.txt` and
`runtime-elf-libraries.txt`. The build's HTTPS apt bootstrap copies the public
builder CA bundle, then installs Debian 13's maintained certificate package.
No TLS verification bypass is used. No Python/Node is introduced: Python
capabilities execute in the Worker and optional Code Mode remains governed by
the existing compatibility contract. Native shell policy is unchanged.

## Image contract and validation

The final stage runs `container-image-check.sh` as **10001:10001** during build.
The script rejects compiler executables/packages/toolchain directories, verifies
Debian 13, exact directory ownership/writability, HOME/SHELL, CA bundle, expected
tools and complete ELF resolution. It starts the actual binary with `--version`
(no database/provider boot), and exercises Bash, ripgrep and Git create/add/commit
in an automatically removed temporary directory. `gcc-N-base` is permitted as
runtime metadata; the `gcc-N` compiler is rejected.

Against an already-built **local** image, primary can run:

```sh
AI_PLATFORM_AGENT_RUNTIME_IMAGE=<local-runtime-tag-or-digest> \
  scripts/harness/check_agent_runtime_image.sh
```

This host wrapper inspects the artifact labels, resolves the immutable local image
ID, and runs the same check with `--pull=never`, network disabled, capabilities
dropped and no-new-privileges. It reports actual image ID/size/platform/user and
working directory. It neither builds nor touches any existing application
container, Compose project, builder, database or volume. No private configuration
or secrets are needed.

Primary must still run the controlled serialized Runtime source build (refresh
its overlay/source lock first), source/image admission, scoped/recovery tests,
current database health and the existing real text/tool/approval/cancel/recovery
journeys on the resulting paired services. Verify Linux sandbox behavior under
the actual container's existing security policy; `bwrap --version` alone proves
only availability. Run the selected image scanner against the immutable image
and enforce **zero critical findings**, including unfixed criticals; do not infer
scan cleanliness from the newer base. For example, where an approved scanner is
already available:

```sh
trivy image --severity CRITICAL --exit-code 1 <immutable-runtime-image>
```

Record actual scan database/time, image identity, findings, compressed/uncompressed
size comparison and unchanged required journeys. Compiler absence, linking,
source contract, live behavior and scan cleanliness are separate claims. None of
the image-dependent gates were executed by the implementation agent.
