# syntax=docker/dockerfile:1.7
# Optional candidate image. No provider keys or operator tokens are build inputs.
# Resolve/publish only after the release gates in docs/deployment/sdk-agent-worker.md.
ARG PYTHON_BASE_IMAGE=python:3.12-slim-bookworm
ARG UV_IMAGE=ghcr.io/astral-sh/uv:0.13.0
FROM ${UV_IMAGE} AS uv
FROM ${PYTHON_BASE_IMAGE} AS builder
COPY --from=uv /uv /usr/local/bin/uv
ENV UV_PYTHON_DOWNLOADS=never UV_LINK_MODE=copy
WORKDIR /build
COPY pyproject.toml uv.lock ./
# Check metadata drift before using the lock. Registry artifacts (including the
# separate build toolchain) are hash-verified; no dependency sdist is executed.
RUN uv lock --check --offline --no-cache && \
    UV_PROJECT_ENVIRONMENT=/opt/build-venv uv sync --frozen --only-group build \
        --no-install-project --no-build --no-cache && \
    UV_PROJECT_ENVIRONMENT=/opt/venv uv sync --frozen --no-default-groups \
        --no-install-project --no-build --no-cache
COPY sdk_agent_worker ./sdk_agent_worker
# Build only our local wheel with the locked backend, without network access or
# implicit build dependencies. Only /opt/venv is copied to the runtime stage.
RUN --network=none uv build --wheel --no-build-isolation --python /opt/build-venv/bin/python --offline && \
    uv pip install --python /opt/venv/bin/python --no-deps --no-index --no-cache dist/*.whl

FROM ${PYTHON_BASE_IMAGE} AS runtime
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="AI Platform Optional SDK Agent Worker" \
      org.opencontainers.image.source="https://github.com/misaya-yang/AI--Platfform" \
      org.opencontainers.image.revision="$VCS_REF"
COPY --from=builder /opt/venv /opt/venv
RUN groupadd --gid 10001 sdk-worker && \
    useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin sdk-worker && \
    mkdir -p /var/lib/sdk-agent && chown 10001:10001 /var/lib/sdk-agent && chmod 0700 /var/lib/sdk-agent
ENV PATH="/opt/venv/bin:/usr/local/bin:/usr/bin:/bin" \
    PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HOME=/tmp \
    SDK_AGENT_STATE_ROOT=/var/lib/sdk-agent OPENAI_AGENTS_DISABLE_TRACING=1
USER 10001:10001
WORKDIR /var/lib/sdk-agent
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD ["python", "-m", "sdk_agent_worker.runtime", "health"]
CMD ["python", "-m", "sdk_agent_worker.runtime", "serve"]
