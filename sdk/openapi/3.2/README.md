# Pinned official OpenAPI 3.2 validation resources

These files are unmodified official resources, retrieved by the primary integrator from
`https://spec.openapis.org/` on 2026-10-17. No network retrieval is permitted by the Rust
validator. JSON Schema 2020-12 core resources are supplied by jsonschema 0.33.0's built-in
registry; all OpenAPI resources (including dynamic anchors) are registered by their `$id`.

| File | Official URL | SHA-256 |
| --- | --- | --- |
| schema-2026-08-30.json | https://spec.openapis.org/oas/3.2/schema/2026-08-30 | ec53db6e114b768ff2d273b232bda210f51b70c5e57d57ac8daf7e8496a6e073 |
| schema-base-2026-08-30.json | https://spec.openapis.org/oas/3.2/schema-base/2026-08-30 | 9df52b98a92075a917125364d23a5f70825d6c292c7aeb15c31b822e95f031e9 |
| meta-2026-02-26.json | https://spec.openapis.org/oas/3.2/meta/2026-02-26 | f444d02ef7d36f30293983fad8118c823a3c70a49a8e5aa6f1afe21215e35417 |
| dialect-2026-02-26.json | https://spec.openapis.org/oas/3.2/dialect/2026-02-26 | 9375b7042aa0b01848403f0a9ebf12a3425e01a9e24337ce12624f10fea73833 |

The official schema validates document/schema syntax. It does not prove implementation
coverage, resolve every application `$ref`, or validate JSON held inside SSE strings.
The release diff resolves local operation references and blocks external/unresolved ones;
the stream replay helper explicitly decodes JSON content and validates it separately.

Compatibility binding for referencing 0.33: when a dynamic anchor overrides a subschema
without its own `$id`, that library can retain the originating resource's resolver URI.
Before compilation, the validator binds fragment-only `$ref` and `$dynamicRef` values to
their original lexical resource `$id` (for example schema-base's `#/$defs/dialect` becomes
`https://spec.openapis.org/oas/3.2/schema-base/2026-08-30#/$defs/dialect`). These references
denote the same targets; dynamic references remain dynamic, and all original assertions
remain present. The on-disk official resources above are unchanged. Tests require a valid
positive control before accepting negative cases, so initialization failures cannot count
as successful invalid-document rejection.
