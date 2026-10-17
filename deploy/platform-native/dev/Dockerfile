# Development compiler only. Never copies project source or runtime configuration.
ARG NATIVE_DEV_BASE_IMAGE=rust:1.99.0-bookworm
FROM ${NATIVE_DEV_BASE_IMAGE}
LABEL agentforge.native.dev.toolchain="1.99.0"
RUN test "$(rustc --version | cut -d' ' -f2)" = "1.99.0" \
    && test -s /etc/ssl/certs/ca-certificates.crt \
    && sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o APT::Update::Error-Mode=any -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt \
       -o Acquire::Retries=3 -o Acquire::https::Timeout=30 update \
    && apt-get -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt \
       -o Acquire::Retries=3 -o Acquire::https::Timeout=30 install -y --no-install-recommends \
       pkg-config protobuf-compiler libssl-dev clang cmake libx11-dev libxkbcommon-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home native-dev \
    && mkdir -p /native-cache/cargo /native-cache/target \
    && chown -R 10001:10001 /native-cache
# Real PDF regression tests use the same Poppler executables as service images.
# Keep this separate so adding test tools reuses the existing compiler layer.
RUN apt-get -o APT::Update::Error-Mode=any -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt \
       -o Acquire::Retries=3 -o Acquire::https::Timeout=30 update \
    && apt-get -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt \
       -o Acquire::Retries=3 -o Acquire::https::Timeout=30 install -y --no-install-recommends poppler-utils \
    && rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
    CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_HOME=/native-cache/cargo CARGO_TARGET_DIR=/native-cache/target \
    RUSTUP_TOOLCHAIN=1.99.0 RUSTUP_HOME=/usr/local/rustup
USER 10001:10001
WORKDIR /source/rust/platform-native
ENTRYPOINT ["cargo"]
