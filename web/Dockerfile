# syntax=docker/dockerfile:1.7
# =============================================================================
# AI Gateway Frontend - Multi-stage Dockerfile
# =============================================================================
# Build: docker build -t ai-gateway-web:2.0.0 .
# Run:   docker run -p 8081:8080 ai-gateway-web:2.0.0
#
# 国内构建（使用镜像源）:
#   docker build --build-arg NPM_REGISTRY=https://registry.npmmirror.com -t ai-gateway-web:2.0.0 .
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: Builder - Build the React application
# -----------------------------------------------------------------------------
ARG NODE_BASE_IMAGE=node:26.11.1-alpine3.24
ARG NGINX_BASE_IMAGE=nginxinc/nginx-unprivileged:1.30-alpine

FROM ${NODE_BASE_IMAGE} AS builder

WORKDIR /app

# Build arguments for mirror configuration
ARG NPM_REGISTRY=https://registry.npmjs.org

# Node 26 no longer bundles Corepack. Bootstrap only the pinned package manager;
# application dependencies below are still installed exclusively with pnpm.
RUN npm install --global pnpm@12.11.2 --allow-scripts=pnpm --registry ${NPM_REGISTRY}

# Copy package files
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./

# Install dependencies
RUN pnpm install --frozen-lockfile --registry ${NPM_REGISTRY}

# Copy source code
COPY . .

# Build arguments for environment variables
ARG VITE_API_URL=
ARG VITE_API_BASE_URL=
ARG VITE_AUTH_EMAIL_DOMAIN=example.com
ARG VITE_SUPPORT_EMAIL=

# Set environment variables for build
ENV VITE_API_URL=${VITE_API_URL}
ENV VITE_API_BASE_URL=${VITE_API_BASE_URL}
ENV VITE_AUTH_EMAIL_DOMAIN=${VITE_AUTH_EMAIL_DOMAIN}
ENV VITE_SUPPORT_EMAIL=${VITE_SUPPORT_EMAIL}

# Build the application
RUN pnpm build

# -----------------------------------------------------------------------------
# Stage 2: Runtime - Nginx serving static files
# -----------------------------------------------------------------------------
FROM ${NGINX_BASE_IMAGE} AS runtime

ARG APP_VERSION=2.0.0
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="AI Gateway Web" \
      org.opencontainers.image.source="https://github.com/misaya-yang/AI--Platfform" \
      org.opencontainers.image.version="$APP_VERSION" \
      org.opencontainers.image.revision="$VCS_REF"

# Copy custom nginx configuration
COPY nginx.conf /etc/nginx/nginx.conf
COPY nginx-precompressed-assets.conf /etc/nginx/nginx-precompressed-assets.conf
COPY docker-entrypoint.d/40-runtime-config.sh /docker-entrypoint.d/40-runtime-config.sh
COPY docker-entrypoint.d/45-security-headers.sh /docker-entrypoint.d/45-security-headers.sh
USER root
RUN chmod +x /docker-entrypoint.d/40-runtime-config.sh /docker-entrypoint.d/45-security-headers.sh
ENV RUNTIME_CONFIG_OUTPUT_PATH=/tmp/runtime-config.json
USER 101:101

# Copy built assets from builder
COPY --from=builder /app/dist /usr/share/nginx/html

# Expose port
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

# Run nginx
CMD ["nginx", "-g", "daemon off;"]
