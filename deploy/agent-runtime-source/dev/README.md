# Local development Runtime / Worker artifact packaging

This path packages the **actual successful controlled Rust1.95 run**, without
Cargo, rustc, dependency resolution, apt, downloads, a second target or registry.
The sole `desktop-linux` builder reads
`ai-platform-capability-target-rust-1-95-v1` at `/cargo-target/debug` readonly.
The compiler base remains the exact existing cached `rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1`.
No daemon image copy of that base is required.

The actual source-unit SHA, every real command/stream receipt, the final
source-bound schema/metadata commands, unchanged source inventories and the exact
eleven binary hashes/sizes are mandatory. Legacy v1 requires its original 22+2
commands. The separate [v2 batch protocol](batch/README.md) requires six actual
batched commands plus two final commands and a successful native batch admission;
it cannot claim that the old 24-command protocol executed. An unsuccessful or partial run
is rejected; it cannot authorize packaging old target-cache artifacts. The
verification RUN uses a fresh public nonce to prevent a cached hash-check result
from standing in for a current target-cache check. Both paired targets share that
single verified artifact stage. The binaries are never exported onto the host. Donor closure stages bind the
verified artifact payload readonly, avoiding two complete duplicate ELF copies.

Run only after the compiler owner has completed a successful immutable run
package; `service-wave-execution-source-r3` and `service-wave-execution-source-r4`
contain preserved real Worker compiler failures and **must fail this preparation**.
The superseded d6 r5 run is partial/interrupted and must also be rejected. Select the subsequent real
successful package explicitly. Root keeps the one existing rust-build lock for
all actual Buildx commands.

```sh
# Public Noto CJK font + its redistribution license must already exist locally.
# FONT_SHA256 and LICENSE_SHA256 must be the expected hashes from that source.
deploy/agent-runtime-source/dev/package.sh --prepare \
  "$SUCCESSFUL_EXECUTION_PACKAGE" "$NEW_EXTERNAL_PACKAGE" \
  "$EXISTING_PUBLIC_NOTO_CJK_TTC" "$FONT_SHA256" \
  "$EXISTING_FONT_LICENSE" "$LICENSE_SHA256" local-development-only

AI_PLATFORM_RUST_BUILD_LOCK_HELD=1 \
  "$NEW_EXTERNAL_PACKAGE/package.sh" --build "$NEW_EXTERNAL_PACKAGE"
```

The only build-context files are selected public receipts, license/notice,
platform entrypoint, packaging controls, Python package catalog and explicit
licensed Office font. Private local env/DSNs/PKI never enter this context.
The actual immutable slim Runtime donor is checked against
`sha256:51ba8952a57c632b5358128f42186f9c636f6d7d349122cd26922a401fe2bde9`.
Scratch images copy its fixed system tool/library/CA closure; its old application,
source/release labels and configured environment do not propagate. Missing ELF
libraries, canonical shell tools, fonts or donor identity stop the build.
Office rendering keeps the compiled embedded fonts plus the real external Noto
CJK collection; no empty placeholder or silent font omission is allowed.

Output sits **outside** the frozen context at `${NEW_EXTERNAL_PACKAGE}-pair-results`:
actual build metadata/logs, immutable image identities, actual version smoke
output, empty first-start Runtime home guard and `paired-images.json`. Every image is labeled local-development-only,
production-admitted=false and generated-release-identity=false. Real images are
named `agentforge-execution-dev:runtime-<unit12>-<receipt12>` and
`agentforge-execution-dev:worker-<unit12>-<receipt12>`. Source locks, admission,
release history/N-1, escape tests, scans and production acceptance remain intact.
Fresh development startup uses the real `gateway_dev_fresh` schema reader,
without manufacturing old upgrade history.

Packaging records a real baseline and stops its own Buildx client on disk/time
limits. Growth allowance is `5 × actual sum of all 11 ELF sizes + 1GiB`, capped at
8GiB, with a 10GiB free-space reserve and 30-minute cap per role. This is a warm
incremental copy/load budget, not a disk quota or a blanket 22GiB start threshold.
Nothing is pruned or removed. Attempts/results are never overwritten. Runtime
version smoke uses ephemeral uid10001 home mounts: upstream arg0 creates helper
aliases before Clap exits, so version checks must not populate the first-start
home in an image layer. The required version check runs after image loading; the
image build validates tools/ELF linkage without writing that home.

## Role boundaries

| Target | Exact source-bound executables / execution role |
| --- | --- |
| `runtime` | Runtime, history gate; uid10001, platform home entrypoint, Bash/Git/bwrap/search/archive/SSH/process tools, public CA/OpenSSL libraries |
| `worker` | Worker, Worker health, plugin packs, pool controller, attestor, Python-sandbox health; Rust control plane + Office CJK font; **no Python, supervisor or sandbox daemon** |
| `sandbox-pool` / `sandbox-attestor` | Same verified control payload, distinct entrypoint, health disabled for the command role; actual deployment/runtimeclass/measurement admission remains mandatory |
| `python-sandbox` | Python sandbox + health + supervisor + measurement, Python3.14 with exact existing locked-package catalog; credential-free data plane |
| `sandbox-artifacts` | Export-only four sandbox executables and proof; never an executable sandbox image |

Optional roles reuse the same context/cache and separate immutable results:

```sh
AI_PLATFORM_RUST_BUILD_LOCK_HELD=1 "$NEW_EXTERNAL_PACKAGE/package.sh" \
  --build "$NEW_EXTERNAL_PACKAGE" sandbox-pool
AI_PLATFORM_RUST_BUILD_LOCK_HELD=1 "$NEW_EXTERNAL_PACKAGE/package.sh" \
  --build "$NEW_EXTERNAL_PACKAGE" sandbox-attestor

# Must name an ALREADY LOCAL Python3.14 canonical sandbox/package donor and its
# expected immutable ID. No Python3.11/system-Python/empty-wheel fallback.
AI_PLATFORM_RUST_BUILD_LOCK_HELD=1 "$NEW_EXTERNAL_PACKAGE/package.sh" \
  --build "$NEW_EXTERNAL_PACKAGE" python-sandbox \
  "$EXISTING_PYTHON314_PACKAGE_DONOR" "$EXPECTED_PYTHON_DONOR_IMAGE_ID"
```

The optional Python stage rejects catalog differences, wrong Python minor,
missing/exact-version package imports, installers/toolchains and missing ELF
closures. Its scratch final image inherits no donor environment or control-plane
credentials. Credentials never cross from control to sandbox. A real configured
sandbox still needs its public verifier, private job journal, delegated cgroup,
actual runtime isolation and attestation. Pool/attestor do not imply hardware
proof. Local image packaging does not authorize a host-process fallback.

## Start integration

This workflow builds and performs network-free `--version` smoke only. It does
not start a service, migrate SQL, change HBA, rewrite Compose or create a project.
Root consumes the two actual IDs/tags through the existing local project's
`AI_PLATFORM_AGENT_RUNTIME_IMAGE` and `AGENT_CAPABILITY_WORKER_IMAGE` inputs.
Do not substitute the optional Python image for the control Worker.

Keep uid/gid10001 writable Runtime home/workspace and Worker workspace/socket
mounts. Supply `DATA_RUNTIME_*` / `DATA_CAPABILITY_WORKER_*` file credentials and
DB mTLS for `gateway_dev_fresh` readable by uid10001, plus the current workload-identity file mappings
and private TLS origins. Required mode keeps HTTP8094/8095 for unauthenticated
health/readiness **only** and uses explicit `AI_PLATFORM_WORKLOAD_TLS_BIND` for
business TLS; ports/certificate SANs/peer allowlists must match root's integration.
No shared bearer, provider secret or fixture mode is introduced by these images.
The existing old local Compose file still has legacy token/Python dependencies;
root's fresh-start override must use the current file/TLS integration rather than
pretending this packaging step repaired that configuration. Python capability
admission must stay closed until its optional role is genuinely ready.

Static rejection tests (output retained externally):

```sh
deploy/agent-runtime-source/dev/test-evidence.sh "$NEW_EXTERNAL_TEST_DIR"
deploy/agent-runtime-source/dev/test-batch-evidence.sh "$ANOTHER_NEW_EXTERNAL_TEST_DIR"
```

These synthetic verifier fixtures prove rejection behavior only. They do not
prove actual compilation, image build, Office rendering, runtime isolation,
service readiness or product acceptance.
