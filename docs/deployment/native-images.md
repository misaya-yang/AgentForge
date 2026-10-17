# Native service and frontend images

The native build authority is `deploy/platform-native/Dockerfile`, using the repository
root as its context. Do not recreate per-service copies. Existing Compose image identities
and operator image overrides remain authoritative. Runtime/Capability source images keep
the separate controlled assembly workflow.

| Target | Binary / assets | Ceiling (MiB) |
| --- | --- | ---: |
| `runtime` (default) | Gateway | 384 |
| `knowledge` | Knowledge HTTP/gRPC service and worker roles | 384 |
| `migrate` | Database authority | 192 |
| `sdk-worker` | SDK worker plus SDK agent engine | 384 |
| `local-node` | Local node | 192 |
| `tools` | Operational tools and test fixture server | 256 |
| `release` | Release operations | 192 |
| `frontend` | Web assets, built with `web/Dockerfile --target runtime` | 128 |
| `code-interpreter` | Optional Python sandbox, existing separate Dockerfile | 1024 |

These initial engineering ceilings are encoded in
`deploy/platform-native/image-size-budgets.json`, apply to both amd64 and arm64,
and are not measured acceptance results. Changes require an explicit policy review;
a failed build must not automatically raise the limit. The metric is Docker image
inspect `Size`, the local uncompressed image bytes, compared without rounding.
Compressed transfer bytes, vulnerability scans, health, and runtime security are
separate verification layers.

The native Dockerfile pins cargo-chef 0.1.71 and uses planner recipes plus a cook
layer for each selected binary. Only that binary's workspace dependencies are cooked
and its binary is copied into the runtime. The contract crate outside the native
workspace and its owning Cargo manifest are copied before cook, because this chef
version records workspace members rather than external path crates. Native source
copies occur after cook. Builder toolchain, target cache and chef do not ship in the
Debian 13 slim runtime. Gateway and Knowledge retain Poppler because both currently
support parser paths; removing the Gateway's optional parser is a separate change.

The CI workflow `.github/workflows/image-size.yml` builds all nine budgeted targets
on matching native amd64/arm64 runners, with no QEMU or publication, and runs the gate
before recording a passing receipt. Its policy job includes unit and real CLI negative
cases: a one-byte overrun, malformed/missing/duplicate images, wrong platform/ID,
missing ceilings, duplicate JSON keys and invalid or repeated CLI options fail closed.

The release publisher calls this reusable workflow in `policy-only` mode, then builds
its five existing image repositories independently on `ubuntu-24.04` (amd64) and
`ubuntu-24.04-arm` (arm64). Each build pushes only by immutable digest, retaining
original OCI config labels, `mode=max` provenance and SBOM attestations. The same
pushed digest is pulled on its matching native runner and must pass its own size
gate; a second build is not used as a proxy measurement. Only admitted digests and
public size evidence are passed to the merge job. A failed gate may leave an untagged
digest upload; it cannot promote release tags and does not trigger registry cleanup.

The merge job requires both architecture records from the same run/source/policy,
then dry-runs manifest creation to check exactly one Linux amd64 and arm64 image,
each with its referenced attestation descriptor, rejecting omitted, duplicate,
orphan or unexpected descriptors. It finally applies the existing semver and full
SHA tags through `imagetools create`, retaining those attestation descriptors.
The policy job exercises this actual jq filter with positive and negative fixtures.
No Rust build uses QEMU. Docker documents these matching native runner labels in
[distributed multi-platform builds](https://docs.docker.com/build/ci/github-actions/multi-platform/#distribute-build-across-multiple-runners).
Workflow validation does not establish successful registry execution; the distributed
build, pulls, attestation preservation and manifest promotion require hosted execution.

After a build has loaded one image, capture only public metadata and bind the gate
to the image ID produced by that build, not a mutable tag's later lookup:

```sh
docker image inspect --format '[{"Id":{{json .Id}},"Size":{{json .Size}},"Os":{{json .Os}},"Architecture":{{json .Architecture}}}]' "$IMAGE_REF" > "$INSPECT_PATH"
platform-tools image-size-check --repo-root . --target "$IMAGE_TARGET" \
  --platform "$IMAGE_PLATFORM" --image-id "$BUILT_IMAGE_ID" \
  --inspect "$INSPECT_PATH" --evidence-out "$RECEIPT_PATH"
```

`IMAGE_TARGET` selects one row above, `IMAGE_PLATFORM` is `linux/amd64` or
`linux/arm64`, and `BUILT_IMAGE_ID` must be `sha256:` plus 64 lowercase hexadecimal
characters. A gate receipt identifies image ID, policy digest, platform, bytes and
ceiling, and explicitly leaves runtime security and compressed bytes unverified.
Full image inspect output can contain environment secrets and must not be uploaded.

For local work, read `docs/harness/runtime-and-secrets.md` first and reuse the
owner-selected Compose project/builder, preserving volumes. Native image-input lock
regeneration follows a reviewed input change and is owned by the integration writer.
Compilation and the policy tests do not establish image execution acceptance.

The frontend defaults to nginx 1.30 unprivileged, UID/GID 101, and internal HTTP 8080.
Host ports and the Helm Service port stay unchanged. Compose and Helm configure a
read-only root filesystem, dropped capabilities, no privilege escalation and memory
scratch at `/tmp`. nginx PID, request/proxy/FastCGI/uWSGI/SCGI temporary paths and
`runtime-config.js` live there. Static files stay immutable; the exact config route
reads `/tmp` with existing cache/security headers. Do not override the runtime-config
output path without aligning nginx's config route. Existing API/WS/SSE/upload and
embed proxy behavior is preserved.

To run the built-image embed header smoke, first build/load the canonical `tools`
target as `ai-platform/tools:rust-local` or supply `NATIVE_FIXTURE_IMAGE`. That smoke
exercises the frontend's non-root/read-only flags and retains all existing assertions.
Real same-project HTTP, WS, SSE, multipart upload, runtime environment changes,
image sizes, scans and read-only/non-root startup must still be measured for each
accepted source/image combination.
