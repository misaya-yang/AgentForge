# Codex source adaptation receipt — 2026-10-17

This receipt records the source-only rebase from `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`
to `ce0323f2f2d3b25e82806cd555237da3db4e1897`. Both read-only upstream checkouts
were clean. No checkout HEAD, source release lock, shared Cargo manifest/lock,
image or running service was changed by this task.

- [receipt.json](receipt.json): 35 upstream replacements replayed, 9 conflicting
  files/13 conflict regions resolved, and hashes for 37 owned source/test files.
- [upstream-preimages.json](upstream-preimages.json): exact new upstream Git blob
  identities for the observed 179-file overlay snapshot. Concurrent workers
  were implementing Runtime/Worker changes; the integration writer must
  recapture the final full inventory before updating the shared manifest.
- [Upgrade procedure](../../../docs/harness/codex-upstream-upgrade.md): the source
  workflow and compatibility decisions. The existing program ledger remains
  the sole execution-status authority.

Passed: Rust 1.95.0 `rustfmt --check --edition 2024 --config skip_children=true`
for the 37 recorded paths; scoped `git diff --check`; YAML parsing and existence
of 19 required documents; 37 relative documentation links and 37 proposed file
SHA-256 identities. These checks do not type-check or execute Rust code.

Added, but not executed here: metadata replacement-plus-patch recovery against
an actual mocked model request; typed read-only fragment text, classification
and provenance; absent read-only context and empty tool registration. Existing
recovery/suspension and upstream Guardian coverage was preserved.

Not run: Cargo checks/tests and compiled inventories, schema/metadata exports,
source candidate/lock admission, native harness-check, Docker builds/probes,
providers, database/browser/tenant acceptance and rollback. The integration
writer owns those serialized actions. The Docker recovery test stage must run
the complete `durable_recovery` target to include the new regression.
