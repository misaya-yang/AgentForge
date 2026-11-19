# First bounded Runtime lifecycle parity batch

Scope: creation and replay of the durable session→runtime assignment. This is the
ownership boundary used before launching a session. No production behavior changed.

`platform-gateway/tests/runtime_migration_parity_assignment.rs` calls the actual
`AgentThreadStore` against canonical migration089, with the existing isolated-schema
fixture. Unlike the old fake dictionary, this exercises real INSERT ON CONFLICT,
scoped SELECTs, the immutable-update trigger and a two-connection race. Fixture
cleanup occurs after errors and assertion panics. No PostgreSQL call was performed.

Exact source assertion mapping is recorded in
`runtime-migration-parity-assignment-map.json` beside this report. Three original
assertion groups now have authored native equivalents, across two tests; a third
native test additionally checks concurrent binding and immutable revision storage.
The two environment-projection tests remain unmapped by this batch. Therefore the
whole original `test_runtime_assignment.py` is **not eligible for retirement yet**.

## Run later, explicitly

Compile through the serialized native workspace all-targets gate. Run only against
an authorized isolated local PostgreSQL fixture using:

`cargo test --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_migration_parity_assignment -- --ignored`

The existing fixture requires local `.env` PostgreSQL settings, connects only to
127.0.0.1 and creates a randomly named schema. Missing settings/database fail; they
do not produce a successful skip. Ordinary test execution reports all three cases
ignored, which is **not a pass**. No production credentials are embedded in tests.

## Connected-scope residuals

The durable-subagent protocol oracle has fifteen source scenario functions. It is
explicitly a deterministic protocol model, not production Assistant integration.
This batch neither substitutes assignment tests for lease/fencing/cancellation
assertions nor marks those functions retired. In particular cancellation ACK versus
terminal commit, disconnect ownership, stale-worker fencing and side-effect replay
need their own production-backed target mapping or test additions.

The assistant integration file contains live code-execution/document/image/quiz and
dialogue scenarios; the Responses and KB integration files likewise involve live
models or capabilities. No live acceptance is claimed or retired by this assignment
batch. Their opt-in replacement coverage must be established separately.

The two missing policy tests should target native configuration projection, including
ignored legacy owner/canary variables and the new kernel revision key, rather than
merely asserting that those strings occur in source code.
