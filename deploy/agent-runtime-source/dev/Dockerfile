# Offline packaging from one existing source-bound Rust1.95 target cache only.
ARG BUILD_IMAGE=rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1
ARG RUNTIME_DONOR
ARG PYTHON_DONOR=${RUNTIME_DONOR}
FROM ${BUILD_IMAGE} AS artifacts
COPY cache-binaries.sha256 cache-binaries.bytes /proof/
ARG VERIFY_NONCE
RUN --network=none --mount=type=cache,id=ai-platform-capability-target-rust-1-95-v1,target=/cargo-target,sharing=locked,readonly \
    set -eu; test -n "$VERIFY_NONCE"; mkdir -p /payload/bin; \
    sha256sum --check --strict /proof/cache-binaries.sha256; \
    while read -r bytes name; do \
      file="/cargo-target/debug/$name"; test -f "$file"; test ! -L "$file"; \
      test "$(wc -c < "$file" | tr -d ' ')" = "$bytes"; \
      test "$(od -An -tx1 -N4 "$file" | tr -d ' \n')" = 7f454c46; \
      cp "$file" "/payload/bin/$name"; chmod 0555 "/payload/bin/$name"; \
    done < /proof/cache-binaries.bytes

FROM ${RUNTIME_DONOR} AS donor
USER 0:0
COPY collect-rootfs.sh /collect-rootfs.sh
RUN --network=none --mount=type=bind,from=artifacts,source=/payload,target=/payload,ro     /bin/sh /collect-rootfs.sh runtime
FROM ${RUNTIME_DONOR} AS worker-donor
USER 0:0
COPY collect-rootfs.sh /collect-rootfs.sh
RUN --network=none --mount=type=bind,from=artifacts,source=/payload,target=/payload,ro     /bin/sh /collect-rootfs.sh worker

FROM scratch AS dev-base
COPY --from=worker-donor /rootfs/ /
COPY --chown=10001:10001 --from=worker-donor /rootfs/workspace/ /workspace/
COPY --chown=10001:10001 --from=worker-donor /rootfs/run/agentforge-sandbox/ /run/agentforge-sandbox/
COPY dev-identity.json built-binaries.json source-unit.json /usr/share/agentforge/dev/
COPY LICENSE NOTICE /usr/share/doc/agentforge/
ARG SOURCE_UNIT_SHA256
ARG BINARY_RECEIPT_SHA256
LABEL agentforge.execution.packaging="local-dev-artifact-v1" agentforge.execution.production-admitted="false" agentforge.execution.generated-release-identity="false" agentforge.execution.source-unit-sha256="$SOURCE_UNIT_SHA256" agentforge.execution.binary-receipt-sha256="$BINARY_RECEIPT_SHA256" agentforge.execution.toolchain="absent"
ENV PATH=/usr/local/bin:/usr/bin:/bin RUST_BACKTRACE=1
USER 10001:10001
WORKDIR /workspace
RUN ["/bin/sh", "-ec", "test \"$(id -u)\" = 10001; test -w /workspace; test -r /etc/ssl/certs/ca-certificates.crt; test ! -e /usr/local/cargo; test ! -e /usr/local/rustup; test ! -e /source; test ! -e /usr/bin/python3; test ! -e /usr/bin/rustc; curl --version; openssl version"]

FROM dev-base AS runtime
COPY --from=donor /rootfs/ /
COPY --chown=10001:10001 --from=donor /rootfs/workspace/ /workspace/
COPY --chown=10001:10001 --from=donor /rootfs/run/agentforge-sandbox/ /run/agentforge-sandbox/
COPY --chown=10001:10001 --from=donor /rootfs/runtime-home/ /runtime-home/
COPY --chown=10001:10001 --from=donor /rootfs/var/lib/ai-platform-agent-runtime/ /var/lib/ai-platform-agent-runtime/
COPY --chmod=0555 container-entrypoint.sh /usr/local/bin/ai-platform-runtime-entrypoint
COPY --from=artifacts /payload/bin/ai-platform-agent-runtime /payload/bin/ai-platform-history-gate /usr/local/bin/
LABEL agentforge.execution.role="runtime"
ENV AI_PLATFORM_RUNTIME_BIND=0.0.0.0:8094 AI_PLATFORM_AGENT_HOME=/runtime-home CODEX_HOME=/runtime-home HOME=/var/lib/ai-platform-agent-runtime SHELL=/usr/bin/bash
EXPOSE 8094
RUN ["/bin/sh", "-ec", "test -w /runtime-home; test -w /var/lib/ai-platform-agent-runtime; git --version; bash --version; bwrap --version; rg --version"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 CMD ["curl","-fsS","http://127.0.0.1:8094/health/ready"]
ENTRYPOINT ["/usr/local/bin/ai-platform-runtime-entrypoint"]

FROM dev-base AS worker
COPY --from=artifacts /payload/bin/ai-platform-capability-worker /payload/bin/ai-platform-capability-worker-health /payload/bin/ai-platform-plugin-packs /payload/bin/ai-platform-sandbox-pool /payload/bin/ai-platform-sandbox-attestor /payload/bin/ai-platform-python-sandbox-health /usr/local/bin/
COPY office/NotoSansCJK-Regular.ttc /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc
COPY office/LICENSE /usr/share/doc/agentforge/NotoSansCJK-LICENSE
LABEL agentforge.execution.role="worker-control"
ENV AI_PLATFORM_CAPABILITY_WORKER_BIND=0.0.0.0:8095 AI_PLATFORM_CAPABILITY_WORKER_HEALTH_ADDR=127.0.0.1:8095 AI_PLATFORM_OFFICE_FONT_PATHS=/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc
EXPOSE 8095
RUN ["/bin/sh", "-ec", "test -s /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc; test ! -e /usr/local/bin/ai-platform-python-supervisor; test ! -e /usr/local/bin/ai-platform-python-sandbox"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 CMD ["ai-platform-capability-worker-health"]
ENTRYPOINT ["ai-platform-capability-worker"]

# Deliberately separate roles; actual runtimeclass/attestation policy still applies.
FROM worker AS sandbox-pool
LABEL agentforge.execution.role="sandbox-pool-controller"
HEALTHCHECK NONE
ENTRYPOINT ["ai-platform-sandbox-pool"]
FROM worker AS sandbox-attestor
LABEL agentforge.execution.role="sandbox-attestor"
HEALTHCHECK NONE
ENTRYPOINT ["ai-platform-sandbox-attestor"]

# Artifact export only; no claim this is an executable Python data-plane image.
FROM scratch AS sandbox-artifacts
COPY --from=artifacts /payload/bin/ai-platform-python-sandbox /payload/bin/ai-platform-python-sandbox-health /payload/bin/ai-platform-python-supervisor /payload/bin/ai-platform-sandbox-measurement /bin/
COPY dev-identity.json built-binaries.json /proof/

# Optional data plane. An explicit existing Python3.14/package donor is mandatory.
FROM ${PYTHON_DONOR} AS python-donor
USER 0:0
COPY python-catalog.json /proof/python-catalog.json
COPY collect-rootfs.sh /collect-rootfs.sh
COPY collect-python.sh /collect-python.sh
RUN --network=none --mount=type=bind,from=artifacts,source=/payload,target=/payload,ro \
    /bin/sh /collect-python.sh
FROM scratch AS python-sandbox
COPY --from=python-donor /rootfs/ /
COPY --chown=10001:10001 --from=python-donor /rootfs/workspace/ /workspace/
COPY --chown=10001:10001 --from=python-donor /rootfs/run/agentforge-sandbox/ /run/agentforge-sandbox/
COPY --chown=10001:10001 --from=python-donor /rootfs/var/lib/agentforge-sandbox/ /var/lib/agentforge-sandbox/
COPY --from=artifacts /payload/bin/ai-platform-python-sandbox /payload/bin/ai-platform-python-sandbox-health /payload/bin/ai-platform-python-supervisor /payload/bin/ai-platform-sandbox-measurement /usr/local/bin/
COPY dev-identity.json built-binaries.json python-catalog.json /usr/share/agentforge/dev/
COPY LICENSE NOTICE /usr/share/doc/agentforge/
ARG SOURCE_UNIT_SHA256
ARG BINARY_RECEIPT_SHA256
LABEL agentforge.execution.packaging="local-dev-artifact-v1" agentforge.execution.production-admitted="false" agentforge.execution.generated-release-identity="false" agentforge.execution.source-unit-sha256="$SOURCE_UNIT_SHA256" agentforge.execution.binary-receipt-sha256="$BINARY_RECEIPT_SHA256" agentforge.execution.role="credential-free-python-sandbox"
ENV PATH=/usr/local/bin:/usr/bin:/bin RUST_BACKTRACE=1
USER 10001:10001
WORKDIR /workspace
RUN ["/bin/sh","-ec","python3 -I -S -B -c 'import sys; assert sys.version_info[:2]==(3,14)'; test -w /workspace; test -w /var/lib/agentforge-sandbox/jobs; test ! -e /usr/bin/cargo; test ! -e /usr/bin/rustc"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD ["ai-platform-python-sandbox-health"]
ENTRYPOINT ["ai-platform-python-sandbox"]
