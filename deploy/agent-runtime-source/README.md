# Agent Runtime source lock and native build

The source lock and build tooling are maintained in this repository. The native
CLI is the `platform-tools` binary from
[`rust/platform-native/crates/platform-tools`](../../rust/platform-native/crates/platform-tools/README.md).
Run it from this existing repository checkout; do not create another project
checkout or tool environment for these operations.

If `platform-tools` is installed, invoke it directly. To run the same parser
from the current checkout without installing it:

```bash
cargo run --locked \
  --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-tools --bin platform-tools -- \
  source-lock validate --repo-root . \
  --lock deploy/agent-runtime-source/lock.json
```

The current CLI operations are:

| Operation | Native command | Effect |
| --- | --- | --- |
| Validate source and release identity | `platform-tools source-lock validate --repo-root . --lock deploy/agent-runtime-source/lock.json [--require-runnable] [--require-artifact ARTIFACT]` | Read-only validation. `--require-runnable` requires the App Server artifact; `ARTIFACT` is `app_server`, `agent_runtime`, or `capability_worker`. |
| Verify the Runtime and Worker local images | `platform-tools source-lock verify-local-images --repo-root . --lock deploy/agent-runtime-source/lock.json [--runtime-image IMAGE] [--worker-image IMAGE]` | Inspects the selected Docker images and compares their immutable identities with the lock. |
| Verify the platform overlay | `platform-tools source-lock verify-overlay --repo-root . [--overlay-dir DIR] [--upstream-sha SHA] [--source-revision SHA] [--source CHECKOUT]` | Verifies the overlay inventory and optional source/preimage bindings. |
| Generate source receipt and SBOM | `platform-tools source-lock generate --fork CHECKOUT --schema-dir DIR --receipt FILE --sbom FILE` | Requires the existing controlled fork to be clean and a generated schema bundle. |
| Generate evidence for a composed candidate | `platform-tools source-lock generate-composed --package DIR --upstream CHECKOUT --composed DIR --repo-root . --export-evidence FILE --schema-dir DIR --output NEW_DIR` | Validates the candidate and export evidence, then writes evidence to a new output directory. |
| Refresh the source lock | `platform-tools source-lock refresh-source-lock --repo-root . --lock deploy/agent-runtime-source/lock.json` | Writes the lock from its source receipt and invalidates all prior image identities. |
| Refresh overlay identities | `platform-tools source-lock refresh-overlay --repo-root . --lock deploy/agent-runtime-source/lock.json --cargo-workspace DIR [--cargo-metadata FILE]` | Writes the overlay and Worker dependency identities from the exact composed Rust workspace. |
| Verify a source candidate | `platform-tools source-candidate verify --package DIR --upstream CHECKOUT --repo-root . [--composed-source DIR]` | Read-only validation; does not advance the live lock or admit an image. |
| Assemble a verified source candidate | `platform-tools source-candidate assemble --package DIR --upstream CHECKOUT --repo-root . --output NEW_DIR` | Writes a new composition directory after validating the source package. |
| Record a local image | `platform-tools source-lock record-local-image --repo-root . --lock deploy/agent-runtime-source/lock.json --artifact ARTIFACT --image IMAGE` | Inspects the image, then updates the lock with its immutable local identity. `ARTIFACT` is `app_server`, `agent_runtime`, or `capability_worker`. |

These command names and options are implemented by the `source-lock` and
`source-candidate` branches in `platform-tools/src/main.rs`. For the complete
native command inventory, see the linked `platform-tools` README. The old
`agent_runtime_supply_chain.py` and `agent_runtime_source_candidate.py` entry
points are not part of the current workflow.

Build the private Rust Runtime and capability Worker through the existing
serialized builder:

```bash
AI_PLATFORM_AGENT_RUNTIME_SOURCE=/path/to/existing-controlled-fork \
  scripts/rust/build-update.sh --artifact all
```

The script accepts `runtime`, `worker`, `app-server`, or `all`; `all` builds the
Runtime and Worker serially and checks both image identities against the lock.
The source checkout is the already controlled fork selected by the operator;
the build does not publish images. `make agent-runtime-contract` and
`make agent-runtime-source-contract` run the native source-lock gate and require
a runnable Runtime artifact. Source-only lock validation does not authorize
Runtime or Worker startup.

Deployment and source refresh use the same lock and native image checks. Runtime
and capability Worker are separate artifacts: both selected images must match
the locked source, schema, binary and overlay identities before the application
services are changed. A source update therefore requires rebuilding and
verifying the affected Rust image pair; container hot-update does not replace
the native Runtime or Worker binaries. Image verification, startup, provider
acceptance, rollback and publication are separate evidence and are not inferred
from a passing source-lock command.

Source synchronization policy and the kernel decision are recorded in
[`ADR-006`](../../docs/architecture/ADR-006-agent-runtime-single-kernel.md);
model, state and capability boundaries are in
[`ADR-007`](../../docs/architecture/ADR-007-agent-runtime-data-boundaries.md).
