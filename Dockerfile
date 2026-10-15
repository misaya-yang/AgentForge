# syntax=docker/dockerfile:1.7
# =============================================================================
# AI Gateway Backend - Multi-stage Dockerfile
# =============================================================================
# Build: docker build -t ai-gateway:2.0.0 .
# Run:   docker run -p 8080:8080 --env-file .env ai-gateway:2.0.0
#
# 国内构建（使用镜像源）:
#   docker build --build-arg PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple -t ai-gateway:2.0.0 .
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: Builder - Install dependencies
# -----------------------------------------------------------------------------
ARG PYTHON_BASE_IMAGE=python:3.12-slim-bookworm

FROM ${PYTHON_BASE_IMAGE} AS builder

ARG DEBIAN_MIRROR=https://deb.debian.org

# Install build dependencies
RUN sed -i "s|http://deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources && \
    apt-get -o Acquire::Retries=5 update && \
    apt-get -o Acquire::Retries=5 install -y --no-install-recommends build-essential && \
    rm -rf /var/lib/apt/lists/*

# Create virtual environment
RUN python -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"

# Mirror arguments are deliberately declared after the shared OS/venv layers,
# so changing package indexes does not invalidate those expensive layers.
ARG PIP_INDEX_URL=https://pypi.org/simple
ARG PIP_TRUSTED_HOST=""

WORKDIR /app

# Copy project metadata files needed for installation
# Note: README.md is required by pyproject.toml for hatchling build
COPY pyproject.toml README.md ./

# Install dependencies first (better layer caching)
# Use mirror if specified via build args
RUN --mount=type=cache,id=ai-gateway-pip,target=/root/.cache/pip,sharing=locked \
    pip install --upgrade pip \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}} && \
    pip install hatchling \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}}

# Copy source code
COPY src/ ./src/
# The installed ai-gateway-db entrypoint delegates to database.authority; keep
# the full immutable migration carrier in the wheel as well as the runtime.
COPY database/ ./database/
# The wheel force-includes the single catalog owned by the Rust worker. Keep
# the canonical source available to Hatch without checking in a Python copy.
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json \
    ./rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json

# Copy workspace member packages. ``pyproject.toml`` lists both packages with
# ``{ workspace = true }`` — pip doesn't understand uv workspaces, so install
# the local contracts and core packages together before the outer Gateway.
COPY packages/ai-gateway-contracts/ ./packages/ai-gateway-contracts/
COPY packages/ai-gateway-core/ ./packages/ai-gateway-core/
RUN --mount=type=cache,id=ai-gateway-pip,target=/root/.cache/pip,sharing=locked \
    pip install ./packages/ai-gateway-contracts ./packages/ai-gateway-core \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}}

# The Gateway image contains control-plane and shared-domain code only. The
# model/tool loop and capability execution ship in their pinned Rust images.

RUN --mount=type=cache,id=ai-gateway-pip,target=/root/.cache/pip,sharing=locked \
    pip install ".[all]" \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}}

# Note: PaddleOCR is handled by the Knowledge Service container, not here.

# -----------------------------------------------------------------------------
# Stage 2: Runtime - Minimal production image
# -----------------------------------------------------------------------------
FROM ${PYTHON_BASE_IMAGE} AS runtime

ARG APP_VERSION=2.0.0
ARG VCS_REF=unknown
ARG DEBIAN_MIRROR=https://deb.debian.org
LABEL org.opencontainers.image.title="AI Gateway" \
      org.opencontainers.image.source="https://github.com/misaya-yang/AI--Platfform" \
      org.opencontainers.image.version="$APP_VERSION" \
      org.opencontainers.image.revision="$VCS_REF"

WORKDIR /app

# Install only native libraries used by document and image processing.
RUN sed -i "s|http://deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources && \
    apt-get -o Acquire::Retries=5 update && \
    apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    curl \
    libgl1 \
    libglib2.0-0 \
    libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home --shell /bin/bash appuser

# Copy virtual environment from builder
COPY --from=builder /opt/venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"


# Copy application code
COPY src/ ./src/
COPY database/ ./database/
COPY config/ ./config/
# Gateway projects the same checked-in capability catalog consumed by the
# Rust worker into its Python package data directory at image-build time.
COPY rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json \
    ./src/core/data/platform_catalog_v1.json

# Create necessary directories and fix permissions
RUN mkdir -p /app/logs && \
    chown -R appuser:appuser /app

# Switch to non-root user
USER appuser

# Environment variables
ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONPATH=/app \
    GATEWAY_HOST=0.0.0.0 \
    GATEWAY_PORT=8080

# Expose port
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD curl -fsS http://localhost:8080/health || exit 1

# Run the application
CMD ["python", "-m", "uvicorn", "src.main:app", "--host", "0.0.0.0", "--port", "8080"]
