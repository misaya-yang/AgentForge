# syntax=docker/dockerfile:1.7
# =============================================================================
# Migration runner — the single PostgreSQL schema authority.
# =============================================================================
# A one-shot container. Compose runs it before every application service. It
# carries the complete immutable history, frozen baseline and authority code;
# no second runner or ledger implementation exists in this image.
#
# Why a separate service:
#   - Removes the "service startup auto-migrates" antipattern (race conditions
#     during multi-service rollout, hard to reason about during deploys).
#   - One pod / one job runs migrations; service replicas wait. No more
#     migration_already_applied conditional logic in service code.
#
# =============================================================================

ARG PYTHON_BASE_IMAGE=python:3.12-slim-bookworm

FROM ${PYTHON_BASE_IMAGE}

ARG APP_VERSION=2.0.0
ARG VCS_REF=unknown
ARG PIP_INDEX_URL=https://pypi.org/simple
ARG PIP_TRUSTED_HOST=""
LABEL org.opencontainers.image.title="AI Gateway Migration Runner" \
      org.opencontainers.image.source="https://github.com/misaya-yang/AI--Platfform" \
      org.opencontainers.image.version="$APP_VERSION" \
      org.opencontainers.image.revision="$VCS_REF"

WORKDIR /app

RUN --mount=type=cache,id=ai-gateway-pip,target=/root/.cache/pip,sharing=locked \
    pip install 'asyncpg==0.31.0' 'PyYAML==6.0.3' \
    --index-url ${PIP_INDEX_URL} \
    ${PIP_TRUSTED_HOST:+--trusted-host ${PIP_TRUSTED_HOST}}

# Namespace-package import works from /app; copying the complete database tree
# is intentional because the authority verifies manifests against every frozen
# SQL input before it connects or writes.
COPY packages/ai-gateway-contracts/src/ai_gateway_contracts /app/ai_gateway_contracts
COPY database /app/database

# Same DATABASE_URL fallback chain as the runner expects.
ENV PYTHONUNBUFFERED=1

RUN useradd --uid 1000 --create-home migrator && chown -R migrator:migrator /app
USER migrator

ENTRYPOINT ["python", "-m", "database.authority"]
CMD ["migrate"]
