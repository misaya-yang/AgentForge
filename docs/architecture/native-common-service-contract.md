# Source-owned common native Services contract

The bounded native exporter declares six existing common Services operations:

- POST and GET `/api/v1/services`
- GET, PUT and DELETE `/api/v1/services/{service_id}`
- GET `/api/v1/services/{service_id}/schema`

Their operation IDs come from the native route inventory. The common-native
implementation manifest independently checks publication IDs, compiled source
hashes and actual Gateway router/bootstrap evidence. These operations retain
Python/common ownership. Neither `sdk/openapi.json` nor its ownership profile is
an input to the Rust producer or changed by this increment.

## Production ownership and semantics

`native_services_contract.rs` supplies the route/method inventory consumed by the
actual Services router, dictionary shape check, mutable-field allowlist, and
production mutation/list/schema response builders. `services_api.rs` keeps
identity resolution, permissions, model checks, visibility, registry calls,
masking and error handling. `ServiceDefinition` remains the real domain DTO;
`service_from_value` remains the converter. There is no new strict request DTO,
response validator, store, database schema, client behavior or endpoint.

Important preserved behavior:

- Legacy POST registration upserts. Native-only `/service-registrations` retains
  its existing atomic-create contract and is not reclassified as common.
- Request dictionaries accept unknown fields. Update ignores fields outside the
  existing allowlist, rejects a changed `service_id`, and rejects patches without
  mutable fields. Values, including null, enter the existing conversion rules.
- List returns a JSON array. The virtual assistant precedes registered rows and
  omits the three availability fields. Registered rows include those fields,
  commonly as null. Dynamic metadata and availability values remain open.
- The last `service_type` query value wins, repeated `tags` remain in order, and
  unknown query keys are ignored. The assistant filter also includes registered
  rows. Tags do not filter the virtual assistant. Unknown service types retain
  their existing internal-error behavior.
- List/detail intersect API-key and tenant visibility. Exact `admin` bypasses
  only user policy and sees ordinary connector secrets. SDK provider credential
  fields remain write-only even for admin. Schema lookup retains its existing
  lack of the detail route's visibility lookup.
- Writes require ServiceConfigWrite and the existing explicit platform-admin
  role/permission test. The declaration does not claim an additional database
  platform-admin recheck beyond existing admission behavior.
- Storage may have changed before cache/proxy invalidation fails. Cancellation
  does not undo completed writes. Exact-ID readback is needed after uncertainty;
  an error is not evidence that replaying a mutation is safe.

The detail schema intentionally describes only a bounded identity/dynamic-field
subset of the real serde projection. Other domain fields and optional
`service_config` remain open. Schema values are not assumed to be objects.
The source binding includes the actual protocol, content-type enum, registry,
Services handler and shared error-renderer sources. Production fixtures compare
real builder/serializer output and exercise actual handlers with the real memory
registry store. Router construction and its production operation selector are
covered without starting an HTTP listener.

## Bounded verification and remaining gaps

This increment declares 27 paths and 34 operations: 21 native-only and 13 common.
The complete publication contains 452 operations; 418 remain absent. The verifier
permits array success only for GET `/api/v1/services` and requires both nonempty,
closed item-envelope declarations. Empty or missing item schemas, malformed
variants and attempts to reuse the exception on another operation fail closed.
This does not narrow dynamic values within the item envelopes.

The freshly executed full-native gate fails with 419 violations, including
seven Roles and six Services authentication differences. The previous gate failed
with 416: declaring three missing Services paths exposes six actual authentication
differences. Native authentication declarations are retained rather than erased
to conceal publication differences. Source declarations or a bounded export are not full
native compatibility, runtime identity or release acceptance.

Required offline checks, in the approved serialized environment:

- `cargo test --locked --offline -p ai-platform-openapi`
- `cargo test --locked --offline -p ai-platform-gateway-foundation --lib contract_tests`
- `cargo test --locked --offline -p ai-platform-gateway-foundation --lib atomic_registration_tests`
- `uv run --all-packages --extra test pytest -q --no-cov tests/contract/test_native_openapi_export.py tests/contract/test_openapi_profiles.py`
- Fresh independent `snapshot-native-openapi` export, deterministic process output,
  current embedded hashes, compiler dependency/source binding and required
  `verify-native-openapi-contract` comparison

These are required commands, not claims they ran. No actual PostgreSQL,
authenticated HTTP listener, provider, browser, deployment or complete CL-01–06
acceptance is established by these fixtures.
