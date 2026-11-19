# Native internal Knowledge RPC

Versioned `platform.knowledge.v1.KnowledgeRetrieval/Retrieve` runs over tonic HTTP/2.
The public Knowledge HTTP API and database schema are unchanged. Gateway signs
its existing POST `/api/v1/knowledge/{dataset_id}/retrieve` with the original body
and authority metadata. Typed protobuf query/filter mirrors must exactly agree
with the signed bytes. The Knowledge adapter reuses its signed ingress and current
ACL/replay checks; protobuf fields never mint a trusted actor. Typed response hits
coexist with the unchanged HTTP response body to preserve errors and extra fields.

`RetrievalClient` shares one multiplexed channel; clones share a bounded semaphore.
Defaults: 3s connect, 30s end-to-end RPC, 32 in-flight requests, 32 channel buffer,
1MiB protobuf request and 16MiB response. Saturation returns ResourceExhausted;
dropped/cancelled futures release capacity. It sets both grpc-timeout and a local
deadline, and performs no transparent application retry (signed replay tokens are
single-use). HTTPS uses verified WebPKI roots. HTTP is for the operator-selected
internal network, with the existing signed service authentication still required.
Secrets and origins are never included in transport error messages.

Dependencies verified against official crate documentation on 2026-10-15:
- https://docs.rs/tonic/0.14.6/tonic/
- https://docs.rs/tonic-prost-build/0.14.6/tonic_prost_build/
- https://docs.rs/prost/0.14.4/prost/
- https://docs.rs/protoc-bin-vendored/3.3.0/protoc_bin_vendored/

The checked-in proto is compiled with the registry-vendored protoc; no system
installation or download script is needed. Root owns Cargo lock/build serialization.
Tests cover typed/signature consistency, authority allowlisting, typed result
projection, configuration bounds and real loopback HTTP/2 cancellation/backpressure.
They do not claim database/provider integration or deployment acceptance.

W3C traceparent/tracestate and x-request-id are separately allowlisted and bounded
to 2KiB; they are diagnostic context only, never authorization evidence. Retry-After
is an explicit optional response field so the HTTP facade can preserve conflict
backoff semantics. Size limits apply to whole protobuf messages, including typed
mirrors, rather than just the JSON body.
