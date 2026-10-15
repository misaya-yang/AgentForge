# syntax=docker/dockerfile:1.7

ARG PYTHON_BASE_IMAGE=python:3.12-slim-bookworm

FROM ${PYTHON_BASE_IMAGE} AS builder

ARG DEBIAN_MIRROR=https://deb.debian.org

RUN sed -i "s|http://deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources && \
    apt-get -o Acquire::Retries=5 update && \
    apt-get -o Acquire::Retries=5 install -y --no-install-recommends build-essential && \
    rm -rf /var/lib/apt/lists/*

RUN python -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"

ARG PIP_INDEX_URL=https://pypi.org/simple
ARG PIP_TRUSTED_HOST=""

WORKDIR /build

# uv.lock is the single dependency source. These generated exports keep image
# build tooling separate from runtime dependencies while retaining hashes for
# every accepted artifact.
COPY apps/knowledge-service/build-requirements.lock.txt /build/knowledge-service/build-requirements.lock.txt
COPY apps/knowledge-service/requirements.lock.txt /build/knowledge-service/requirements.lock.txt
COPY packages/ai-gateway-contracts/ /build/packages/ai-gateway-contracts/
COPY packages/ai-gateway-core/ /build/packages/ai-gateway-core/
COPY apps/knowledge-service/pyproject.toml apps/knowledge-service/README.md /build/knowledge-service/
COPY apps/knowledge-service/src /build/knowledge-service/src

# PRD T7-3: install the hash-locked build toolchain first so source-only
# transitive packages cannot pull floating setuptools/wheel versions. The three
# local workspace wheels use the exact-pinned Hatchling from that toolchain,
# with build isolation disabled. `make kb-image-lock-refresh` regenerates the
# exports and `make kb-image-lock-gate` rejects drift.
RUN --mount=type=cache,id=ai-gateway-pip,target=/root/.cache/pip,sharing=locked \
    python -m pip install --require-hashes \
    -r /build/knowledge-service/build-requirements.lock.txt \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}} && \
    python -m pip install --require-hashes --no-build-isolation \
    -r /build/knowledge-service/requirements.lock.txt \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}} && \
    python -m pip install --no-deps --no-build-isolation \
    /build/packages/ai-gateway-contracts \
    /build/packages/ai-gateway-core \
    /build/knowledge-service \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}}

FROM ${PYTHON_BASE_IMAGE} AS runtime

ARG APP_VERSION=2.0.0
ARG VCS_REF=unknown
ARG DEBIAN_MIRROR=https://deb.debian.org
LABEL org.opencontainers.image.title="AI Gateway Knowledge Service" \
      org.opencontainers.image.source="https://github.com/misaya-yang/AI--Platfform" \
      org.opencontainers.image.version="$APP_VERSION" \
      org.opencontainers.image.revision="$VCS_REF"

RUN sed -i "s|http://deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources && \
    apt-get -o Acquire::Retries=5 update && \
    apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    poppler-utils curl \
    && rm -rf /var/lib/apt/lists/*

ENV PATH="/opt/venv/bin:$PATH" \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
WORKDIR /app

COPY --from=builder /opt/venv /opt/venv

RUN useradd --uid 1000 --create-home appuser && \
    mkdir -p /app/data && \
    chown -R appuser:appuser /app
USER appuser

EXPOSE 8092
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://localhost:8092/health || exit 1

CMD ["python", "-m", "uvicorn", "knowledge_service.main:app", "--host", "0.0.0.0", "--port", "8092"]
