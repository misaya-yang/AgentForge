# Versioned native API release contracts

The Rust producer emits OpenAPI 3.2.0 with the pinned OAS 3.2 JSON Schema 2020-12 dialect.
Its existing `--native-only` stdout entry remains available and preserves source-coverage
metadata. New release documents must come from that same actual native producer:

```sh
ai-platform-openapi produce --release-version 1.0.0 --out sdk/releases/1.0.0/openapi.json
ai-platform-openapi validate --candidate sdk/releases/1.0.0/openapi.json
ai-platform-openapi breaking --baseline sdk/releases/1.0.0/openapi.json --candidate /external-cache/openapi-candidate.json
```

`produce` validates against the complete pinned official schema-base with dynamic references
and refuses to overwrite any existing release. Choose the actual release version, rather
than treating the example version as an activated platform release. No release artifact has
been fabricated from the historical `sdk/openapi.json`; the first real producer export and
baseline selection belong to integration after compilation.

Every PR should export the candidate from its own source and compare it against the last
released, immutable document from the protected base revision. Never take the baseline from
an untrusted PR or replace the baseline with the candidate. The command exits 1 for changed
contracts, 2 for invalid inputs, and 0 for compatible contracts. Only an explicit
`--allow-major-breaking` with an increased numeric major version permits breaking changes;
the JSON result still lists them. CI must not set this override for ordinary PRs.

This conservative gate proves unchanged operation contracts (including resolved schemas,
stream `itemSchema`, inherited security/parameters/servers). Additive operations and
annotation edits are accepted. Other changes, even potentially compatible schema changes,
require review and an explicit major release; unknown assertions are never silently ignored.
The existing bounded native source/ownership gate remains a separate coverage check.

`python-hybrid` is retired. Version-1 ownership manifests and their `python_paths` partition
remain readable solely to verify historical artifacts; they do not restore a Python service
or authorize an export profile. Generated Python SDK work remains independent.

SSE `itemSchema` describes each **parsed event**: `data`, `event`, `id` are strings and
`retry` is a nonnegative integer. Invocation data uses `contentMediaType: application/json`
and `contentSchema`; error events retain plain text. Provider passthrough remains opaque,
including provider-defined JSON records, text errors and `[DONE]`; no completion is invented.
JSONL media describe parsed records with `itemSchema`; unknown record payloads remain open.
Wire-body string schemas are retained. `validation::validate_sse_event` checks parsed event
shape and explicitly validates JSON content for typed invocation streams (JSON Schema's
content vocabulary alone does not assert decoded content validity).

Integration verification (not executed by the source author):

```sh
cargo test --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi --all-targets
# Then export, official validate, and compare actual versioned release artifacts above.
```

Recorded production stream replay and all generated SDK compatibility remain acceptance
work; synthetic unit fixtures do not establish those outcomes.
