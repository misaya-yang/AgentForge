# Strong sandbox pool

The optional `sandbox.strongPool` Helm template runs a Rust controller and a
separate node attestor. It creates fresh digest-pinned gVisor pods, consumes one
pod per sealed execution, and uses only a node-local Unix socket for job bytes.
The ordinary Worker points `AI_PLATFORM_SANDBOX_SOCKET` at the controller's
`control/control.sock`; it does not send its database, broker or platform lease
credentials into sandbox pods. The controller's API credential and the
attestor's independent signing key stay in their respective control containers.

The controller reserves `job ID -> slot + digest` durably and fsyncs the directory
before forwarding. A lost acknowledgement, restart or deleted pod never assigns
that ID to a fresh sandbox. Tombstones persist; the controller stops admission
at its bounded journal/socket-directory capacity. Exact terminal or unknown reply
bytes are fsynced before a completed pod is deleted with a Pod UID precondition;
duplicates replay the same receipt across controller restarts. Receipts remain
until the original lease plus the configured retention interval; tombstones stay.
A durable cancel intent prevents a delayed original dispatch from starting and
allows UID-fenced pod destruction without fabricating a terminal reply. Admission
reserves worst-case reply space within a 256 MiB receipt budget before dispatch.
User artifact data is persisted by the existing Gateway broker. Idle warm pods
expire after five minutes; signed evidence is never renewed in a used sandbox.

The attestor requires a live CRI sandbox with `io.containerd.runsc.v1`, the
expected runtime handler, exact approved launch arguments/mounts, actual image
references, and a running executable inode whose SHA-256 matches the operator's
pinned runsc binary. Its actual argv must include the measured sandbox ID and
`--network=none`. Only then does it sign the trusted image's boot/hostname probe,
binding the Pod UID, node, runtime, image and a 15 minute expiry. This is software
runtime measurement anchored in the trusted node, **not hardware attestation**.
Labels, RuntimeClass objects, files supplied by an untrusted program, a missing
CRI field, or a hash mismatch never authorize execution. Unsupported CRI output
requires an explicitly reviewed adapter; the code has no unsigned fallback.

Before enabling the template, the operator must provision the dedicated runtime
handler and node, the read-only `crictl` binary/socket, public attestation keyring,
an independent private attestor key Secret, the existing public sandbox-job key,
and a delegated cgroup CSI driver. Prepare `sharedHostPath/{control,slots,journal}`
owned by UID/GID 10001 with mode 0750. The private attestor key file must be 0400;
only its public keyring enters the sandbox namespace. The chart requires explicit
image digests, API-server CIDR, node, RuntimeClass and seccomp settings. It does
not install a runtime, CSI driver, CNI or signing root, and never enables a weaker
tier if these inputs are absent.

Both the strong runtime measurement and the existing namespace, Landlock,
seccomp and delegated-cgroup probes must pass. A gVisor/kernel/CSI combination
that lacks those APIs remains unavailable. No nested-isolation probe is waived
merely because a RuntimeClass exists. Runtime support, escape probes, actual
one-job destruction, credential-negative tests, rotation, cold/warm latency and
Helm install/upgrade/rollback are acceptance gates; these source files do not
claim they ran. The standalone RuntimeClass and containerd/runsc files are
reviewable examples, not an executed deployment.

Root packaging must include `ai-platform-sandbox-pool` and
`ai-platform-sandbox-attestor` in the control image and
`ai-platform-sandbox-measurement` in the credential-free sandbox image. All are
native binaries of the existing Worker package. `Dockerfile.control` consumes the
source-locked binary image; it does not build or download services at startup.
