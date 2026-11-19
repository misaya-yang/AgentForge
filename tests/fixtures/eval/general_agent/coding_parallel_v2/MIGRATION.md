# Rust fixture version 2

This is a new benchmark fixture version, not a continuation of v1 evidence.
It retains both intentional defects: floor-only cent allocation and nested-object-order-sensitive request fingerprints. Five native tests preserve the original acceptance scenarios; four are expected to fail before a candidate repairs the two allowed modules. The tenant-isolation test should pass unchanged.

The source language, implementation paths, test commands, and receipt schemas changed. Every contract/source hash and all collector, host-test, golden, and merge receipts must be regenerated. Historical v1 receipts cannot establish acceptance for v2.

Release provisioning must supply a verified digest-pinned Rust sandbox image plus Cargo.lock and vendored dependencies. The image is deliberately null until supplied; validators reject this state. No image digest is guessed and no live benchmark is claimed here.

Provisioning update (2026-10-15): the official `rust:1.99.0-slim-bookworm` linux/amd64 platform manifest and config were retrieved read-only from Docker Hub and independently SHA256-checked. `sandbox-image.json` preserves their distinct identities and metadata; the validator does not confuse a manifest digest with Docker's config-based image ID. The contract is now digest-pinned. The official image's metadata records a minimal Rust toolchain installation and cargo/rustc version checks; no layers were pulled and no sandbox was executed during this verification.

The fixture retains serde_json's insertion-order representation and SHA256 behavior. Cargo.lock, a local offline source replacement, and 25 vendored crates are supplied; `vendor-inventory.json` binds every Cargo checksum manifest. All 1179 upstream files were checksum-verified, and 49 original license/notice files are retained. No Python source or bytecode is present in the vendor tree. Before provider collection, the native verifier checks source-artifact bindings, every vendor checksum, and the preloaded image's config identity. A missing preloaded image fails closed. Host tests and live benchmark acceptance still require separate explicit execution evidence.
