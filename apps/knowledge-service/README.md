# Native Knowledge Service

The Knowledge service is a Rust/Axum/Tokio application. Its implementation is in
[`rust/platform-native/crates/platform-knowledge-service`](../../rust/platform-native/crates/platform-knowledge-service).
It provides document ingestion, chunking, embedding, Qdrant indexing, retrieval,
QA and Confluence source management. This directory retains deployment files and
historical SQL; application startup no longer uses Python.

## Build and run

Run these commands from the repository root with the pinned Rust toolchain.
Keep Cargo build output outside the checkout and serialize builds:

```bash
export CARGO_TARGET_DIR=/absolute/external/build-directory
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo +1.99.0 build --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-knowledge-service --bin platform-knowledge-service

# Configure the process environment before starting the binary.
KNOWLEDGE_RUNTIME_ROLE=all \
  "$CARGO_TARGET_DIR/debug/platform-knowledge-service"
```

The binary reads environment variables; it does not automatically load `.env`.
Use your environment manager or the container `--env-file` option. For the
source-built development stack, use `make dev-compose` as described in the
[command catalog](../../docs/harness/commands.md).

Startup requires the configured PostgreSQL schema, Redis where durable replay
and idempotency are enabled, and the configured service dependencies. Schema
admission runs before readiness and worker startup. The service does not apply
migrations itself; use the native database authority commands in the command
catalog. `/health/live` is liveness, not proof that dependencies are ready;
`/health/ready` reports readiness.

## Configuration and roles

Most service settings use `KNOWLEDGE_` with `__` for nested fields. Shared
identity and infrastructure settings also use names such as
`AI_PLATFORM_INTERNAL_TOKEN`, `AI_PLATFORM_CAPABILITY_PROOF_SECRET`,
`INTERNAL_COMM_STATE_BACKEND`, `INTERNAL_IDEMPOTENCY_BACKEND` and
`INTERNAL_COMM_REDIS_URL`. Gateway and Knowledge must have matching internal
signing configuration. Do not expose unsigned retrieval or enable anonymous
access as a substitute for configuring it. Shared deployments require durable
Redis replay/idempotency; memory backends are restricted to explicit local/test
profiles.

[`.env.example`](.env.example) lists legacy-compatible settings. The authoritative
native projection is [`src/process.rs`](../../rust/platform-native/crates/platform-knowledge-service/src/process.rs).
Important settings include:

- `KNOWLEDGE_APP__HOST` / `KNOWLEDGE_APP__PORT`: HTTP bind, default `0.0.0.0:8092`.
- `KNOWLEDGE_RUNTIME_ROLE`: `api`, `worker` or `all` (default). Worker mode serves
  health/metrics but does not mount the API or public release route.
- `KNOWLEDGE_DATABASE__DSN` and `KNOWLEDGE_QDRANT__URL`: persistence endpoints.
- `KNOWLEDGE_QDRANT__PREFER_GRPC`: selects the native Qdrant gRPC transport when
  enabled; this is separate from the internal Knowledge RPC listener.
- `KNOWLEDGE_HTTP__MAX_CONCURRENT_REQUESTS`: default 1024.
- `KNOWLEDGE_HTTP__RESPONSE_HEADER_TIMEOUT_MS` and
  `KNOWLEDGE_HTTP__RESPONSE_BODY_IDLE_TIMEOUT_MS`: default 120000; explicit `null`
  disables the respective timeout. Streaming bodies have an idle deadline,
  not a total SSE lifetime limit.

### Internal retrieval RPC

The external HTTP contract remains available. Native deployments can select the
internal typed `platform.knowledge.v1.KnowledgeRetrieval/Retrieve` gRPC path:

- `KNOWLEDGE_GRPC__BIND_ADDRESS`: absent disables it; source-build Compose uses
  `0.0.0.0:8093` on the private network, without publishing this port to the host.
- `KNOWLEDGE_GRPC__MAX_CONCURRENT_REQUESTS`: default 32, range 1–256.
- `KNOWLEDGE_GRPC__DEADLINE_MS`: default 30000, range 100–120000.

Set the listener only on `api` or `all`; worker-only mode rejects it. Gateway
selects RPC with `GATEWAY_KNOWLEDGE_RPC_URL` (source-build Compose defaults to
`http://knowledge-service:8093`); its timeout and capacity settings are
`GATEWAY_KNOWLEDGE_RPC_TIMEOUT_MS` and `GATEWAY_KNOWLEDGE_RPC_MAX_IN_FLIGHT`. The protocol checks typed request fields
against the exact signed body and uses the existing authentication, replay,
tenant/dataset ACL and generation checks. Complete protobuf messages are bounded
to 1 MiB for requests and 16 MiB for responses, including typed mirrors. HTTP and
RPC share graceful shutdown. Authentication failures do not trigger an automatic
HTTP fallback.

## Container image

The Docker build context must be the repository root, not this directory:

```bash
docker build -f apps/knowledge-service/Dockerfile --target knowledge \
  -t knowledge-service .
docker run --rm --env-file /path/to/knowledge.env \
  -p 8092:8092 knowledge-service
```

The image entrypoint is `/usr/local/bin/platform-knowledge-service`. The image
includes Poppler for PDF processing. Optional standalone OCR requires a native
Tesseract installation and configuration. Do not publish the internal RPC port
as a public API.

## Inference and parsing boundaries

Native CPU loaders implement the default cross-encoder and supported BGE models;
weights are not downloaded automatically. Configure verified local artifacts
using `KNOWLEDGE_NATIVE__CROSS_ENCODER_MODEL_DIRECTORY`,
`KNOWLEDGE_NATIVE__BGE_MODEL_DIRECTORY` and
`KNOWLEDGE_NATIVE__BGE_LARGE_MODEL_DIRECTORY`. Arbitrary sentence-transformers
architectures and the advertised Gemma classification path are not implied to
work. Compilation and synthetic model tests do not establish real-weight
inference quality or performance.

PDF decoding uses native Poppler tools. Decoder/layout output can differ from
historical engines; fixture acceptance remains necessary. Provider calls,
production data integration, model-weight inference and live Qdrant acceptance
were not run as part of the latest transport verification. See the
[migration report](../../reports/rust-migration-knowledge.md) for exact evidence
and remaining limits.

## Parent-child retrieval

The native fold is in
[`migration_knowledge_query/parents.rs`](../../rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_query/parents.rs).
It groups duplicate parent references and reads each distinct parent through the
scoped store API, retaining best-child scoring and missing-parent fallbacks.
The current implementation performs these parent reads sequentially; it does
not claim the historical fixed eight-worker implementation or measured
throughput gains.

## Verification

Use the native package/target tests, with the same external build directory and
single-job policy as above. The focused internal-RPC tests require only local
loopback networking, not a database or provider:

```bash
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo +1.99.0 test --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-knowledge-service --lib rpc::tests
```

The latest recorded run passed all four RPC tests, including actual HTTP/2,
signed-router authentication, replay refusal and graceful shutdown. Broader
checks are `make native-check` and the affected native test targets. The
`qdrant_integration_smoke` target is explicitly ignored by default and requires
a deliberately selected live Qdrant when run with `--ignored`; it creates and
cleans only disposable scope-tagged collections. Do not treat an ignored live
test or successful compilation as live integration acceptance.
