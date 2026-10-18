# Existing pinned Rust1.95 builder/cache only. This prepares source evidence,
# never a production image or sandbox deployment. No package installer or fetch.
ARG BUILD_IMAGE=rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1
FROM ${BUILD_IMAGE} AS collector
WORKDIR /source/codex-rs
COPY --from=composed / /source/
COPY . /control/
RUN --network=none set -eu; \
    cd /control; sha256sum --check --status control-static.sha256; \
    test "$(od -An -tx1 -N4 /control/native-validator/platform-tools | tr -d ' \n')" = 7f454c46; \
    ldd /control/native-validator/platform-tools > /native-validator-linkage.txt; \
    ! grep -F 'not found' /native-validator-linkage.txt; \
    cd /source/codex-rs; \
    rustc +1.95.0 --edition=2024 /control/collector.rs -o /collector; \
    if ! command -v ld.lld >/dev/null 2>&1; then \
      sysroot="$(rustc +1.95.0 --print sysroot)"; \
      target="$(rustc +1.95.0 -vV | sed -n 's/^host: //p')"; \
      test -x "$sysroot/lib/rustlib/$target/bin/rust-lld"; \
      ln -s "$sysroot/lib/rustlib/$target/bin/rust-lld" /usr/local/bin/ld.lld; \
    fi
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_TARGET_DIR=/cargo-target CARGO_TERM_COLOR=never CARGO_NET_OFFLINE=true RUSTFLAGS="-C link-arg=-fuse-ld=lld" RUST_MIN_STACK=16777216 RUST_TEST_THREADS=1
RUN --network=none \
    --mount=type=cache,id=ai-platform-capability-cargo-registry-v1,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ai-platform-capability-cargo-git-v1,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=ai-platform-capability-target-rust-1-95-v1,target=/cargo-target,sharing=locked \
    /collector
FROM scratch AS export
COPY --from=collector /export/ /
