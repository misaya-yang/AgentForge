# syntax=docker/dockerfile:1.7

FROM rust:1.95.0-bookworm AS builder

ARG AI_PLATFORM_AGENT_RUNTIME_UPSTREAM_SHA
ARG AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256
ARG CARGO_BUILD_JOBS=2

WORKDIR /source/codex-rs
COPY codex-rs/ ./

RUN --mount=type=cache,id=ai-platform-codex-cargo-registry-v1,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ai-platform-codex-cargo-git-v1,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=ai-platform-codex-target-rust-1-95-dev-small-v1,target=/source/codex-rs/target,sharing=locked \
    CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS}" CARGO_INCREMENTAL=0 \
    cargo build --locked --profile dev-small -p codex-app-server --bin codex-app-server \
    && cp target/dev-small/codex-app-server /tmp/codex-app-server

FROM rust:1.95.0-bookworm AS runtime

ARG AI_PLATFORM_AGENT_RUNTIME_UPSTREAM_SHA
ARG AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256

LABEL org.opencontainers.image.title="AI Platform Agent Runtime Source" \
      org.opencontainers.image.source="https://github.com/openai/codex" \
      org.opencontainers.image.revision="${AI_PLATFORM_AGENT_RUNTIME_UPSTREAM_SHA}" \
      com.misaya.ai-platform.agent-runtime.schema-sha256="${AI_PLATFORM_AGENT_RUNTIME_SCHEMA_SHA256}" \
      com.misaya.ai-platform.agent-runtime.artifact="app_server" \
      com.misaya.ai-platform.agent-runtime.binary="codex-app-server"

RUN useradd --create-home --home-dir /var/lib/ai-platform-agent-runtime --uid 10001 agent-runtime

COPY LICENSE NOTICE /opt/ai-platform-agent-runtime/
COPY --from=builder /tmp/codex-app-server /usr/local/bin/codex-app-server

ENV CODEX_HOME=/var/lib/ai-platform-agent-runtime \
    RUST_BACKTRACE=1

USER 10001:10001
WORKDIR /workspace

ENTRYPOINT ["codex-app-server"]
CMD ["--listen", "stdio://"]
