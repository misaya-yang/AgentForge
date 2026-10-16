# syntax=docker/dockerfile:1.7
# Native service build. All application processes below are Rust executables.
FROM rust:1.99.0-bookworm AS builder
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY rust/agent-runtime-overlay/kernel-rs/Cargo.toml rust/agent-runtime-overlay/kernel-rs/Cargo.toml
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/ rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/
COPY database/ database/
WORKDIR /source/rust/platform-native
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-gateway --bin gateway \
      -p ai-platform-knowledge-service --bin platform-knowledge-service \
      -p ai-platform-migration-authority --bin database-authority \
      -p ai-platform-sdk-agent-worker --bin sdk-agent-worker \
      -p ai-platform-local-node --bin local-node \
      -p ai-platform-tools --bin platform-tools \
      -p ai-platform-release --bin platform-release \
    && install -d /output \
    && install target/release/gateway target/release/platform-knowledge-service \
      target/release/database-authority target/release/sdk-agent-worker target/release/local-node target/release/platform-tools target/release/platform-release /output/

FROM rust:1.99.0-bookworm AS engine-builder
WORKDIR /engine
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/ ./
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-engine-target,target=/engine/target,sharing=locked \
    cargo build --locked --release \
    && install -D target/release/ai-platform-sdk-agent-engine /output/ai-platform-sdk-agent-engine

FROM debian:bookworm-slim AS native-base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl poppler-utils \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home gateway \
    && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin sdk-worker \
    && install -d -o 1000 -g 1000 /app/data /app/logs /app/agent-memory /app/uploads /app/data/pdf-scratch \
    && install -d -m 0700 -o 10001 -g 10001 /var/lib/sdk-agent
COPY --from=builder /output/ /usr/local/bin/
ARG VCS_REF=unknown
LABEL org.opencontainers.image.revision="$VCS_REF" com.misaya.ai-platform.artifact="native-services"
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database
WORKDIR /app
USER 1000:1000

FROM native-base AS knowledge
ENTRYPOINT ["/usr/local/bin/platform-knowledge-service"]

FROM native-base AS migrate
COPY database/ /app/database/
ENV AI_GATEWAY_DATABASE_DIR=/app/database
ENTRYPOINT ["/usr/local/bin/database-authority"]
CMD ["migrate"]

FROM native-base AS sdk-worker
COPY --from=engine-builder /output/ai-platform-sdk-agent-engine /usr/local/bin/
ENV HOME=/tmp SDK_AGENT_STATE_ROOT=/var/lib/sdk-agent
USER 10001:10001
WORKDIR /var/lib/sdk-agent
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 CMD ["/usr/local/bin/sdk-agent-worker", "health"]
ENTRYPOINT ["/usr/local/bin/sdk-agent-worker"]
CMD ["serve"]

FROM native-base AS runtime
ENTRYPOINT ["/usr/local/bin/gateway"]
CMD ["--config-env"]

FROM migrate AS final
