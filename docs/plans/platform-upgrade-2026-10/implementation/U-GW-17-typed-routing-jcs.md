# U-GW-17 typed catalog, routing, failover and canonical JSON v2

Implementation is present; native compilation, regression execution and live
acceptance remain pending. U-GW-01 price book implementation is independently
unaccepted; this package does not imply its migration or accounting acceptance.

The `provider_routing` domain uses `CatalogProvider`, `CatalogModel`, `ModelRef`,
`RoutePolicy`, `FailoverPolicy`, `RouteTarget` and `FailoverPlan`. Domain signatures
contain no JSON `Value`. Selection checks exact tenant/provider/model identities,
provider/model enablement, credential availability and runtime provider support in
the existing error order. Catalog records contain credential availability only,
never secret material. Provider omission remains ambiguous even if one matching
model is disabled. Fallbacks retain configured order; failed candidates emit typed
warnings, successful candidates are deduplicated, and a valid chain remains
reachable with the existing bounded-attempt behavior. Primary failure is fatal.
This plans candidates; it does not execute retries or expand retry safety policy.

Existing public JSON APIs remain reexports of explicit `provider_routing_wire`
and `model_failover_wire` compatibility modules. Historical scalar conversion,
provider URL heuristics, environment credential rules, credential fingerprints,
metadata projection and server-only payload extraction remain at those wire
boundaries. The actual builders call the typed catalog/plan; this is a shared
implementation rather than a parallel unused schema. Existing Gateway live
candidate resolution therefore also reaches the typed selector without changing
its public request shape or changing provider traffic/retry behavior. The
resolved-service adapter still refuses DB-row inference when runtime fields are
missing. Runtime payload Debug remains redacted and payloads remain non-Serialize.

`RuntimeModelConfig::route_fingerprint_v2` and the corresponding resolved config
method expose the selected route plan's RFC 8785 SHA256 identity. It binds the
schema, tenant, ordered provider/model/runtime-provider candidates, failover flag
and attempt bound. It intentionally does not hash credentials, sampling or request
content; existing credential cache discriminators remain separate and continue
to change on rotation. This content identity is not an authorization proof, and
no legacy lease/signature/cache key is silently replaced by it.

`canonical_json_v2` provides strict raw-JSON parsing plus a typed canonical value
builder. Parsing rejects duplicate property names (including escaped duplicates),
unpaired surrogates/invalid UTF-8, invalid/nonfinite number input and trailing
content. The builder's finite number wrapper rejects NaN/Infinity before they can
be coerced to JSON null. Object keys sort by UTF-16 code units, strings use JCS
escaping without Unicode normalization, and binary64 numbers use ECMAScript
shortest formatting via exact dependency `ryu-js = "=1.0.2"`. Negative zero becomes
zero. Numeric identifiers outside binary64 precision must be strings. Encoding
and parsing have explicit depth/byte bounds.

The existing `python_json` source and owning legacy proof/verifier paths are
unchanged. `verify_json_hash` requires an explicit `CanonicalVersion`: Python v1
with the owning protocol's historical ASCII/spacing options, or JCS v2. Unknown
wire version names fail; there is no try-v1-on-v2-failure path. Only content hashes
are compared here. Authorities must still verify signatures at their existing
boundaries and must explicitly negotiate a new envelope version before signing
or accepting new JCS proofs.

Supplied verification source: typed tenant/provider ambiguity, credential and
primary-failure tests; fallback ordering/duplicate/warning tests; compatible wire
projection and redaction tests; credential-rotation discriminator tests; RFC 8785
sample, 22 Appendix B binary64 vectors, UTF-16 sorting/normalization tests,
duplicate/Unicode/nonfinite rejection, SHA256 vector and frozen-v1/version tests.
Lightweight checks passed: targeted rustfmt parsing/style and diff whitespace;
all 22 numeric expected strings and the sample expected bytes independently match
local ECMAScript JSON.stringify. Rust tests and runtime compatibility gates must
be executed by the primary build owner before acceptance. Required follow-up:
affected foundation tests and existing Gateway model/lease/launch contracts,
provider failover tenant isolation, and explicit v1 proof verification regression.
