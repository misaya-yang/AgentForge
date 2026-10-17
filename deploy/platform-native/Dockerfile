# syntax=docker/dockerfile:1.7
# Native service build. All application processes below are Rust executables.
FROM rust:1.99.0-bookworm AS builder
# Pin the dependency-layer tool independently of the application lockfile.
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN cargo install cargo-chef --version 0.1.71 --locked

FROM builder AS planner
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN cargo chef prepare --recipe-path /recipe.json

FROM builder AS cook-base
# cargo-chef 0.1.71 recipes include workspace members, not external path crates.
# Keep the shared contract and its owning workspace manifest before cook so this
# dependency is real and its changes invalidate the dependency layer correctly.
WORKDIR /source
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
WORKDIR /source/rust/platform-native
COPY --from=planner /recipe.json /recipe.json
# Recipe changes invalidate dependency layers; application source changes do not.

FROM cook-base AS deps-gateway
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-gateway --bin gateway

FROM deps-gateway AS build-gateway
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-gateway --bin gateway \
    && install -D target/release/gateway /output/gateway

FROM cook-base AS deps-knowledge
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-knowledge-service --bin platform-knowledge-service

FROM deps-knowledge AS build-knowledge
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-knowledge-service --bin platform-knowledge-service \
    && install -D target/release/platform-knowledge-service /output/platform-knowledge-service

FROM cook-base AS deps-migrate
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-migration-authority --bin database-authority

FROM deps-migrate AS build-migrate
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-migration-authority --bin database-authority \
    && install -D target/release/database-authority /output/database-authority

FROM cook-base AS deps-sdk-worker
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-sdk-agent-worker --bin sdk-agent-worker

FROM deps-sdk-worker AS build-sdk-worker
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-sdk-agent-worker --bin sdk-agent-worker \
    && install -D target/release/sdk-agent-worker /output/sdk-agent-worker

FROM cook-base AS deps-local-node
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-local-node --bin local-node

FROM deps-local-node AS build-local-node
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-local-node --bin local-node \
    && install -D target/release/local-node /output/local-node

FROM cook-base AS deps-tools
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-tools --bin platform-tools

FROM deps-tools AS build-tools
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-tools --bin platform-tools \
    && install -D target/release/platform-tools /output/platform-tools

FROM cook-base AS deps-release
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path /recipe.json -p ai-platform-release --bin platform-release

FROM deps-release AS build-release
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-release --bin platform-release \
    && install -D target/release/platform-release /output/platform-release

FROM rust:1.99.0-bookworm AS engine-builder
WORKDIR /engine
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/ ./
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-engine-target,target=/engine/target,sharing=locked \
    cargo build --locked --release \
    && install -D target/release/ai-platform-sdk-agent-engine /output/ai-platform-sdk-agent-engine

# Debian 13 runtime; compiler and Cargo caches never enter shipped layers.
FROM debian:trixie-slim AS native-base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home gateway \
    && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin sdk-worker \
    && install -d -o 1000 -g 1000 /app/data /app/logs /app/agent-memory /app/uploads /app/data/pdf-scratch \
    && install -d -m 0700 -o 10001 -g 10001 /var/lib/sdk-agent
ARG VCS_REF=unknown
LABEL org.opencontainers.image.revision="$VCS_REF" com.misaya.ai-platform.artifact="native-services"
WORKDIR /app
USER 1000:1000

# Both Knowledge and the Gateway's supported optional parser use Poppler.
# Keep that compatibility until the Gateway parser path is explicitly retired.
FROM native-base AS parser-base
USER root
RUN apt-get update && apt-get install -y --no-install-recommends poppler-utils \
    && rm -rf /var/lib/apt/lists/*
USER 1000:1000

FROM parser-base AS runtime
COPY --from=build-gateway /output/gateway /usr/local/bin/gateway
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database AI_GATEWAY_AUTHORITY_SOURCE_MODE=distribution
ENTRYPOINT ["/usr/local/bin/gateway"]
CMD ["--config-env"]

FROM parser-base AS knowledge
COPY --from=build-knowledge /output/platform-knowledge-service /usr/local/bin/platform-knowledge-service
ENTRYPOINT ["/usr/local/bin/platform-knowledge-service"]

FROM native-base AS migrate
COPY --from=build-migrate /output/database-authority /usr/local/bin/database-authority
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database AI_GATEWAY_AUTHORITY_SOURCE_MODE=distribution
ENTRYPOINT ["/usr/local/bin/database-authority"]
CMD ["migrate"]

FROM native-base AS sdk-worker
COPY --from=build-sdk-worker /output/sdk-agent-worker /usr/local/bin/sdk-agent-worker
COPY --from=engine-builder /output/ai-platform-sdk-agent-engine /usr/local/bin/
ENV HOME=/tmp SDK_AGENT_STATE_ROOT=/var/lib/sdk-agent
USER 10001:10001
WORKDIR /var/lib/sdk-agent
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 CMD ["/usr/local/bin/sdk-agent-worker", "health"]
ENTRYPOINT ["/usr/local/bin/sdk-agent-worker"]
CMD ["serve"]

FROM native-base AS local-node
COPY --from=build-local-node /output/local-node /usr/local/bin/local-node
ENTRYPOINT ["/usr/local/bin/local-node"]

FROM native-base AS tools
COPY --from=build-tools /output/platform-tools /usr/local/bin/platform-tools
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database AI_GATEWAY_AUTHORITY_SOURCE_MODE=distribution
ENTRYPOINT ["/usr/local/bin/platform-tools"]

FROM native-base AS release
COPY --from=build-release /output/platform-release /usr/local/bin/platform-release
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database AI_GATEWAY_AUTHORITY_SOURCE_MODE=distribution
ENTRYPOINT ["/usr/local/bin/platform-release"]

# Preserve the root Dockerfile's default Gateway entrypoint.
FROM runtime AS final
