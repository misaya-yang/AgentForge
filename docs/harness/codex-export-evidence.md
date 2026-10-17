# Docker Codex source-export evidence

This is the input contract for the existing native `source-candidate` and
`source-lock generate-composed` verifiers. It does not replace the program
ledger or record execution. The JSON below is a template: every hash, count,
path and output must come from the actual controlled run.

## Full source inventory

Before and after the export commands, inventory the entire composed source
root mounted at `/source`, with Cargo targets and exports outside that root.
Prune only `.git`, matching the existing source-unit verifier. An extra source
file, changed mode, missing file or changed symlink invalidates the inventory.

Write identical JSON bytes to `source-inventory-before.json` and
`source-inventory-after.json`. Schema:

```json
{
  "schema": "ai-platform/source-export-inventory/v1",
  "records": [
    {"path": "codex-rs/Cargo.lock", "file_mode": "100644", "sha256": "<file SHA-256>"},
    {"path": "codex-rs/vendor/bubblewrap/LICENSE", "file_mode": "120000", "sha256": "<link target bytes SHA-256>"}
  ]
}
```

The records contain **all** source files, ordered by repository-relative path
using Unix byte order. A regular file is `100755` if any executable bit is set,
otherwise `100644`. A symlink is `120000`; hash its link target bytes without
following it. Hash regular file bytes directly. Host verification recomputes
this complete inventory after verifying upstream+overlay source composition.
`source_inventory_sha256` below is the SHA-256 of the inventory JSON file bytes,
not an aggregate of file hashes.

## Composed export v2

`--export-evidence` points to the following JSON. All `files` paths are relative
to that JSON's directory and must have actual SHA-256 values. The existing
schema-bundle identity includes `sha256`, `file_count` and `total_bytes`.

```json
{
  "schema": "ai-platform/composed-source-export/v2",
  "target_commit": "<40-character target commit>",
  "source_unit_sha256_before": "<final source-unit.json SHA-256>",
  "source_unit_sha256_after": "<same SHA-256>",
  "resolved_lock_sha256": "<composed Cargo.lock SHA-256>",
  "schema_bundle": {"sha256": "<schema identity>", "file_count": 0, "total_bytes": 0},
  "execution": {
    "kind": "docker",
    "host_source_root": "<canonical --composed path>",
    "host_cwd": "<canonical --composed path>/codex-rs",
    "container_source_root": "/source",
    "container_cwd": "/source/codex-rs",
    "container_schema_dir": "/export/schema",
    "source_binding": {
      "target_commit": "<target commit>",
      "target_tree": "<target Git tree>",
      "source_unit_sha256": "<source-unit.json SHA-256>",
      "resolved_lock_sha256": "<Cargo.lock SHA-256>",
      "source_inventory_sha256": "<source-inventory-before.json SHA-256>"
    }
  },
  "files": {
    "source_inventory_before": {"path": "source-inventory-before.json", "sha256": "<actual hash>"},
    "source_inventory_after": {"path": "source-inventory-after.json", "sha256": "<same hash>"},
    "schema_stdout": {"path": "schema.stdout", "sha256": "<actual raw stdout hash>"},
    "schema_command_receipt": {"path": "schema-command.json", "sha256": "<actual hash>"},
    "metadata_command_receipt": {"path": "metadata-command.json", "sha256": "<actual hash>"},
    "cargo_metadata": {"path": "metadata.json", "sha256": "<actual raw stdout hash>"},
    "cargo_lock": {"path": "Cargo.lock", "sha256": "<resolved lock hash>"},
    "cargo_version": {"path": "cargo-version.txt", "sha256": "<actual hash>"},
    "rustc_version": {"path": "rustc-version.txt", "sha256": "<actual hash>"}
  }
}
```

Schema command receipt:

```json
{
  "command": ["cargo", "+1.95.0", "run", "--locked", "--offline", "-p", "codex-app-server-protocol", "--example", "ai_platform_export_schema", "--", "/export/schema"],
  "cwd": "/source/codex-rs",
  "exit_code": 0,
  "stop_reason": null,
  "stdout_sha256": "<raw schema.stdout SHA-256>",
  "source_binding": {
    "target_commit": "<target commit>",
    "target_tree": "<target Git tree>",
    "source_unit_sha256": "<source-unit.json SHA-256>",
    "resolved_lock_sha256": "<Cargo.lock SHA-256>",
    "source_inventory_sha256": "<source-inventory-before.json SHA-256>"
  }
}
```

Metadata receipt uses the same cwd and binding, with exact argv
`["cargo","+1.95.0","metadata","--locked","--offline","--format-version","1"]`.
Its `stdout_sha256` must equal the raw exported `metadata.json` hash; do not
rewrite workspace or manifest paths in that stdout. Source versions remain
actual `cargo +1.95.0 --version` / `rustc +1.95.0 --version` outputs.

Only after every source binding, inventory, command and output hash verifies may
the verifier map `/source/…` metadata manifest paths to the canonical host
source root. It rejects other prefixes, traversal, symlink aliases and files
other than `Cargo.toml`. `workspace_root` must be exactly `/source/codex-rs`.
The schema argv must name `/export/schema`; the host `--schema-dir` is verified
independently against the complete schema bundle. V1 remains a host-path
contract and cannot declare a path alias.

## Complete compiled capability-contract tests

The source package must retain all existing compiler receipts and identity
checks. For a new target, add these files to `source-unit.json`'s `evidence`
SHA-256 map (including the inventory JSON itself):

- `evidence/contract-test-inventory.json`
- `evidence/contract-list.json`
- `evidence/contract-list.stdout`
- `evidence/contract-tests.stdout`

Continue hashing the existing `evidence/contract-tests.json`,
`evidence/resolved-metadata.json` and other package evidence. List and test
receipts must use the metadata's exact `workspace_root` as cwd, and bind:

```json
{
  "target_commit": "<unit.target>",
  "target_tree": "<unit.target_tree>",
  "candidate_source_sha256": "<unit.input_candidate_manifest_sha256>",
  "resolved_lock_sha256": "<unit.resolved_lock_sha256>"
}
```

Use that object as `source_binding` in both command receipts. This binding uses
the pre-lock candidate identity to avoid a circular hash through the final
source unit's evidence map. Collection argv is exactly:

```json
["cargo", "+1.95.0", "test", "--locked", "-p", "ai-platform-capability-contract", "--", "--list"]
```

Execution argv remains the complete existing package command:

```json
["cargo", "+1.95.0", "test", "--locked", "-p", "ai-platform-capability-contract"]
```

Both command receipts require actual `exit_code: 0`, `stop_reason: null`, cwd,
source binding and a `stdout_sha256` matching the unedited raw stdout file.
Do not filter names, skip, ignore, use `--list` as execution, substitute static
`#[test]` counting, or rewrite metadata paths. Disable color through the
execution environment if needed; do not alter the exact argv.

The inventory JSON is:

```json
{
  "schema": "ai-platform/compiled-test-inventory/v1",
  "target_commit": "<unit.target>",
  "target_tree": "<unit.target_tree>",
  "candidate_source_sha256": "<unit.input_candidate_manifest_sha256>",
  "resolved_lock_sha256": "<unit.resolved_lock_sha256>",
  "listed_count": 0,
  "list_command_receipt": "evidence/contract-list.json",
  "list_stdout": "evidence/contract-list.stdout",
  "test_stdout": "evidence/contract-tests.stdout"
}
```

Replace `listed_count` with the actual compiled collection count. The verifier
requires all original thirteen named tests (declared in
`source_test_inventory.rs`) and every additionally collected test to appear
exactly once as a successful execution outcome. Summaries and per-test outcomes
must agree. The existing build receipt must report
`contract_unit_tests: {"passed": N, "failed": 0, "ignored": 0, "filtered_out": 0}`,
where `N` equals the real list and execution counts. No nonzero-only fallback
exists for a new target. Only the original `092d3acd…` historical source unit
without an inventory retains its unchanged exact-thirteen legacy gate.
