# Source-owned common native role contract

The bounded native exporter now covers the seven existing RolesApi operations,
independently of the Python-hybrid producer. This does not change
`sdk/openapi-profiles.json`, its Python projection, or `sdk/openapi.json`.
`/roles` and its three child path templates remain common APIs.

## Production ownership

`native_roles_contract.rs` owns the route inventory consumed by the mounted
RolesApi router, request length constants consumed by native validation, and
response constructors/schema declarations. Existing Python-compatible validation
continues to ignore unknown fields, require nonempty create name/description,
count Unicode characters, default create permissions to an empty array, and
interpret omitted/null update properties as unchanged. Permission-catalog checks,
current platform-admin mutation checks, tenant-scoped reads, and redacted errors
remain in the production handler.

The literal GET `/roles/permissions` is the catalog lookup. Existing PUT/DELETE
semantics at that literal path still target the ordinary role named `permissions`;
this compatibility behavior is described on the exported operations. No new
methods, storage writes, or authentication modes are introduced.

`native-common-contracts.json` records reviewed common-operation IDs and concrete
source/mount evidence. The bounded verifier validates it independently, checks
compiled source hashes, and rejects missing/extra operations, stale IDs, removed
mounts, and attempts to move these operations outside the Python projection.
This file is implementation evidence, not an ownership exclusion list.

## Limits and verification

The source producer covers 24 paths and 28 operations, of which seven are common
native implementations. The published 452-operation contract still has 424
operations absent from this bounded producer. Success envelopes are declared;
dynamic user rows and host/admission/cancellation response possibilities stay
explicitly open. The required full-native release gate is unchanged and remains
required; source export success is not release or live-runtime acceptance.

Offline checks:

- Existing `snapshot-native-openapi` producer/coverage check
- `cargo test --locked --offline -p ai-platform-openapi`
- `cargo test --locked --offline -p ai-platform-gateway-foundation --lib contract_tests`
- `uv run --all-packages --extra test pytest -q --no-cov tests/contract/test_native_openapi_export.py tests/contract/test_openapi_profiles.py`

Use the repository's approved serialized build environment. These are prescribed
commands, not claims that checks have run. No service, database, or provider is
needed for these checks.
