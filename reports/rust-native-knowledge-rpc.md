# Rust-native Knowledge RPC upgrade — 2026-10-15

The owner authorized upgrading the internal transport rather than mechanically
retaining the former service architecture. `ai-platform-rpc` now defines a checked-in
versioned protobuf KnowledgeRetrieval service and generated tonic client/server API.
External HTTP routes and database schemas remain unchanged.

## Implemented foundation

- Pinned tonic/tonic-prost/tonic-prost-build 0.14.6 and prost/prost-build 0.14.4,
  verified against official docs.rs package documentation. Build-time protoc is
  pinned registry-vendored 3.3.0; no Python or system protoc requirement.
- Typed dataset/query/top-k/mode/document/language/source/metadata-filter request
  fields, validated against the retained byte-exact signed body. Advanced options
  remain intact. Typed results contain identity, score, content, metadata and image
  fields. Raw response bytes retain external error and extension semantics.
- Existing signed POST path `/api/v1/knowledge/{dataset_id}/retrieve` is retained.
  Only gateway HMAC and canonical actor metadata are propagated as authority.
  Duplicates fail closed; no RPC field itself creates an authenticated actor.
- W3C traceparent/tracestate and x-request-id are propagated under a separate 2KiB
  diagnostic bound. Retry-After is explicit in the response contract.
- Multiplexed reusable channel, 3s connection deadline, 30s end-to-end/request
  metadata deadline, cancellation by dropping the in-flight RPC, 32 global shared
  client permits, bounded Tower channel buffer and immediate overload rejection.
  Defaults bound whole protobuf messages to 1MiB request/16MiB response. HTTPS
  verifies WebPKI roots. There are no transparent signed-request retries.

Knowledge service owner is wiring the generated server through the same in-process
signed ingress/ACL/replay path. Gateway owner is wiring the configured client into
the existing external HTTP facade. Those owners document their concrete callsites
and lifecycle separately; this report does not substitute scaffolding for cutover.

## Verification status

Four unit tests cover typed-body agreement, metadata allowlisting/duplicates/trace
propagation, typed response projection and resource bounds. One loopback HTTP/2
transport test exercises generated stubs, shared client capacity, cancellation and
permit release without databases, providers or real credentials.

The first root-serialized `rpc-tests29` attempt was stopped by the disk guard during
dependency compilation before this crate compiled. No passing test claim at this
checkpoint. Root owns rerun, lockfile and build serialization. No deployment,
PostgreSQL/Redis execution or provider calls performed for this upgrade.

Official dependency references:
- https://docs.rs/tonic/0.14.6/tonic/
- https://docs.rs/tonic-prost-build/0.14.6/tonic_prost_build/
- https://docs.rs/prost/0.14.4/prost/
- https://docs.rs/protoc-bin-vendored/3.3.0/protoc_bin_vendored/

## Verified checkpoint — rpc-tests30

Root's serialized rerun completed successfully: 4 unit tests and the real loopback
HTTP/2 transport test passed (5 total, 0 failures, 0 ignored). The transport test
completed in 0.05s. Evidence: `rpc-tests30.log` in the external verification output.
This supersedes the earlier disk-interrupted attempt, but does not establish
integrated Gateway/Knowledge database retrieval or deployed acceptance.
