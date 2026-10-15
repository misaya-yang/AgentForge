# syntax=docker/dockerfile:1.7
# Explicit candidate build only; legacy images/default quickstart are unchanged.
FROM rust:1.99.0-bookworm AS builder
WORKDIR /source
COPY rust/platform-native/ rust/platform-native/
COPY packages/ai-gateway-core/src/ai_gateway_core/image/assets/watermark.png packages/ai-gateway-core/src/ai_gateway_core/image/assets/watermark.png
WORKDIR /source/rust/platform-native
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN --mount=type=cache,id=platform-native-registry-rust-1-99,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=platform-native-target-rust-1-99,target=/source/rust/platform-native/target,sharing=locked \
    cargo build --locked --release -p ai-platform-gateway --bin gateway --bin knowledge-service \
    && install -D target/release/gateway /output/gateway \
    && install -D target/release/knowledge-service /output/knowledge-service

FROM python:3.12-slim-bookworm AS runtime
# Retain the existing Debian/Python base pin and existing PDF/health tools.
# Python Knowledge and migration authority still run in their separate images.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl poppler-utils \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home gateway \
    && install -d -o gateway -g gateway /app/data /app/logs /app/agent-memory /app/uploads
COPY --from=builder /output/gateway /output/knowledge-service /usr/local/bin/
ARG VCS_REF=unknown
LABEL org.opencontainers.image.revision="$VCS_REF" \
      com.misaya.ai-platform.artifact="native-candidate"
WORKDIR /app
USER 1000:1000
# The explicit runtime-only JSON mount is never part of the image context.
ENTRYPOINT ["/bin/sh", "-ec", "exec /usr/local/bin/gateway \"$@\" < /run/secrets/native-gateway.json", "gateway"]
CMD ["--config-stdin"]
